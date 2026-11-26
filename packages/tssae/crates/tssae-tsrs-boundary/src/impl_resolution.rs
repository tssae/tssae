use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use rustc_hash::FxHashMap;
use serde::Serialize;
use tsrs_ast::{
    Kind, Node, SourceFile, Symbol, SymbolFlags, get_source_file_of_node,
    is_function_like_declaration,
};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::{P, tspath};
use tsrs_module::packagejson::{InfoCacheEntry, JSONValueType};
use tsrs_scanner::get_token_pos_of_node;
use tsrs_sourcemap::{
    DocumentPosition, DocumentPositionMapper, ECMALineInfo, Host as SourceMapHost,
    create_ecma_line_info, get_document_position_mapper,
};
use tssae_common::{DiagnosticLocation, EvidenceItem, GapItem, UnknownReason};

use crate::semantic_facade::{SemanticFacade, resolve_symbol_for_node};
use crate::syntax_view::{
    node_location_relative, node_text, normalize_path, prop_key_text, relative_to_cwd,
    unwrap_expr_wrappers,
};

const MAX_ALIAS_HOPS: usize = 16;

/// How a first-party workspace implementation was located through compiler semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FirstPartyResolutionSource {
    CurrentProject,
    ProjectReference,
    CustomConditions,
    DeclarationMap,
}

/// Target input accepted by [`SemanticFacade::resolve_implementation`].
#[derive(Clone, Copy)]
pub enum FacadeImplementationInput {
    Node(P<Node>),
    Declaration(P<Node>, Option<P<Symbol>>),
}

/// Concrete first-party implementation target located in workspace source.
#[derive(Clone, Debug)]
pub struct FacadeImplementationTarget {
    pub file: String,
    pub location: DiagnosticLocation,
    pub declaration_node: P<Node>,
    pub declaration_symbol: Option<P<Symbol>>,
    pub fn_node: Option<P<Node>>,
    pub export_name: Option<String>,
    pub package_name: Option<String>,
    pub resolution_source: FirstPartyResolutionSource,
    pub declaration_file: Option<String>,
}

/// Outcome of resolving an implementation target through [`SemanticFacade::resolve_implementation`].
#[derive(Clone, Debug)]
pub enum FacadeImplementationOutcome {
    Resolved(FacadeImplementationTarget),
    External {
        package_name: Option<String>,
        declaration_file: Option<String>,
        evidence: Vec<EvidenceItem>,
    },
    Inconclusive {
        declaration_file: Option<String>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

/// Package and export provenance extracted for a resolved declaration.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeclarationProvenance {
    pub package_name: Option<String>,
    pub export_name: Option<String>,
}

/// Snapshot-scoped cache of parsed `.d.ts.map` / inline source-map mappers.
#[derive(Default, Clone)]
pub struct SourceMapCache {
    mappers: Arc<Mutex<FxHashMap<String, Option<Arc<DocumentPositionMapper>>>>>,
}

impl SourceMapCache {
    fn get_or_load(
        &self,
        program: &'static Program,
        workspace_programs: &[&'static Program],
        generated_file_name: &str,
    ) -> Option<Arc<DocumentPositionMapper>> {
        let key = normalize_path(generated_file_name);
        {
            if let Ok(guard) = self.mappers.lock()
                && let Some(cached) = guard.get(&key)
            {
                return cached.clone();
            }
        }

        let host = FacadeSourceMapHost::new(program, workspace_programs);
        let loaded = get_document_position_mapper(&host, generated_file_name).map(Arc::new);
        if let Ok(mut guard) = self.mappers.lock() {
            guard.insert(key, loaded.clone());
        }
        loaded
    }
}

struct FacadeSourceMapHost<'a> {
    program: &'static Program,
    workspace_programs: &'a [&'static Program],
    line_info_cache: RefCell<FxHashMap<String, Option<Arc<ECMALineInfo>>>>,
}

impl<'a> FacadeSourceMapHost<'a> {
    fn new(program: &'static Program, workspace_programs: &'a [&'static Program]) -> Self {
        Self {
            program,
            workspace_programs,
            line_info_cache: RefCell::new(FxHashMap::default()),
        }
    }
}

impl SourceMapHost for FacadeSourceMapHost<'_> {
    fn use_case_sensitive_file_names(&self) -> bool {
        self.program.use_case_sensitive_file_names()
    }

    fn get_ecma_line_info(&self, file_name: &str) -> Option<Arc<ECMALineInfo>> {
        if let Some(cached) = self.line_info_cache.borrow().get(file_name) {
            return cached.clone();
        }
        let info = find_source_file_in_programs(self.program, self.workspace_programs, file_name)
            .map_or_else(
                || {
                    self.program.host().fs().read_file(file_name).map(|text| {
                        let line_starts = tsrs_core::compute_ecma_line_starts(&text);
                        create_ecma_line_info(text, line_starts)
                    })
                },
                |(_, sf)| {
                    Some(create_ecma_line_info(
                        sf.text().to_string(),
                        sf.ecma_line_map().to_vec(),
                    ))
                },
            );
        self.line_info_cache
            .borrow_mut()
            .insert(file_name.to_string(), info.clone());
        info
    }

    fn read_file(&self, file_name: &str) -> Option<String> {
        self.program.host().fs().read_file(file_name)
    }
}

struct ModuleProvenance {
    containing_sf: P<SourceFile>,
    specifier_node: P<Node>,
    specifier_text: String,
    export_name: Option<String>,
}

/// Returns `true` when `file_name` resolves inside `cwd` outside any `node_modules` directory.
#[must_use]
pub fn is_first_party_workspace_path(cwd: &str, file_name: &str) -> bool {
    let workspace_root = canonical_or_normalized(cwd);
    let candidate = canonical_or_normalized(file_name);
    candidate.starts_with(&workspace_root)
        && !candidate
            .components()
            .any(|component| component.as_os_str() == "node_modules")
}

/// Returns `true` when `sf` is a first-party workspace file rather than a third-party or stdlib file.
#[must_use]
pub fn is_first_party_source_file(cwd: &str, program: &Program, sf: P<SourceFile>) -> bool {
    !program.is_source_file_default_library(sf.path())
        && is_first_party_workspace_path(cwd, sf.file_name())
}

fn canonical_or_normalized(raw: &str) -> PathBuf {
    Path::new(raw)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(normalize_path(raw)))
}

fn is_bare_package_specifier(spec: &str) -> bool {
    !spec.is_empty() && !spec.starts_with('.') && !spec.starts_with('/')
}

fn split_package_specifier(spec: &str) -> (String, String) {
    let mut parts = spec.split('/');
    let Some(first) = parts.next() else {
        return (spec.to_string(), ".".to_string());
    };
    if first.starts_with('@')
        && let Some(second) = parts.next()
    {
        let pkg = format!("{first}/{second}");
        let rest: Vec<&str> = parts.collect();
        let subpath = if rest.is_empty() {
            ".".to_string()
        } else {
            format!("./{}", rest.join("/"))
        };
        return (pkg, subpath);
    }
    let rest: Vec<&str> = parts.collect();
    let subpath = if rest.is_empty() {
        ".".to_string()
    } else {
        format!("./{}", rest.join("/"))
    };
    (first.to_string(), subpath)
}

fn string_literal_text(node: P<Node>) -> Option<String> {
    if matches!(
        node.kind(),
        Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
    ) {
        return Some(node.text().to_string());
    }
    node_text(node).map(|s| s.trim_matches(&['\'', '"', '`'][..]).to_string())
}

fn extract_require_specifier_node(node: P<Node>) -> Option<P<Node>> {
    let unwrapped = unwrap_expr_wrappers(node);
    if unwrapped.kind() != Kind::CallExpression {
        return None;
    }
    let callee = unwrapped.expression()?;
    if callee.kind() == Kind::Identifier && node_text(callee).as_deref() == Some("require") {
        let arg = unwrapped.arguments().first().copied()?;
        if matches!(
            arg.kind(),
            Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
        ) {
            return Some(arg);
        }
    }
    None
}

fn extract_callable_from_decl(decl: P<Node>) -> Option<P<Node>> {
    if is_function_like_declaration(Some(decl)) {
        return Some(decl);
    }
    match decl.kind() {
        Kind::VariableDeclaration | Kind::PropertyDeclaration | Kind::PropertyAssignment => {
            decl.initializer().and_then(|init| {
                let inner = unwrap_expr_wrappers(init);
                is_function_like_declaration(Some(inner)).then_some(inner)
            })
        }
        Kind::ExportAssignment => decl.expression().and_then(|expr| {
            let inner = unwrap_expr_wrappers(expr);
            is_function_like_declaration(Some(inner)).then_some(inner)
        }),
        _ => None,
    }
}

fn export_name_from_alias_decl(decl: P<Node>) -> Option<String> {
    match decl.kind() {
        Kind::ImportSpecifier => {
            let spec = decl.as_import_specifier();
            spec.property_name()
                .and_then(node_text)
                .or_else(|| node_text(spec.name()))
        }
        Kind::ExportSpecifier => {
            let spec = decl.as_export_specifier();
            spec.property_name()
                .and_then(node_text)
                .or_else(|| node_text(spec.name()))
        }
        _ => None,
    }
}

fn provenance_from_import_decl_node(
    decl: P<Node>,
    preferred_export: Option<String>,
) -> Option<ModuleProvenance> {
    let sf = get_source_file_of_node(decl)?;
    match decl.kind() {
        Kind::ImportSpecifier => {
            let spec_struct = decl.as_import_specifier();
            let export_name = spec_struct
                .property_name()
                .and_then(node_text)
                .or_else(|| node_text(spec_struct.name()))
                .or(preferred_export);
            let named = decl.parent()?;
            let clause = named.parent()?;
            let import_decl = clause.parent()?;
            let spec_node = import_decl.module_specifier()?;
            Some(ModuleProvenance {
                containing_sf: sf,
                specifier_node: spec_node,
                specifier_text: string_literal_text(spec_node)?,
                export_name,
            })
        }
        Kind::ImportClause => {
            let import_decl = decl.parent()?;
            let spec_node = import_decl.module_specifier()?;
            Some(ModuleProvenance {
                containing_sf: sf,
                specifier_node: spec_node,
                specifier_text: string_literal_text(spec_node)?,
                export_name: preferred_export.or_else(|| Some("default".to_string())),
            })
        }
        Kind::NamespaceImport => {
            let clause = decl.parent()?;
            let import_decl = clause.parent()?;
            let spec_node = import_decl.module_specifier()?;
            Some(ModuleProvenance {
                containing_sf: sf,
                specifier_node: spec_node,
                specifier_text: string_literal_text(spec_node)?,
                export_name: preferred_export,
            })
        }
        Kind::ImportDeclaration => {
            let spec_node = decl.module_specifier()?;
            Some(ModuleProvenance {
                containing_sf: sf,
                specifier_node: spec_node,
                specifier_text: string_literal_text(spec_node)?,
                export_name: preferred_export,
            })
        }
        Kind::ImportEqualsDeclaration => {
            let mod_ref = decl.as_import_equals_declaration().module_reference();
            if mod_ref.kind() != Kind::ExternalModuleReference {
                return None;
            }
            let spec_node = mod_ref.as_external_module_reference().expression();
            Some(ModuleProvenance {
                containing_sf: sf,
                specifier_node: spec_node,
                specifier_text: string_literal_text(spec_node)?,
                export_name: preferred_export,
            })
        }
        Kind::ExportSpecifier => {
            let exp_spec = decl.as_export_specifier();
            let export_name = exp_spec
                .property_name()
                .and_then(node_text)
                .or_else(|| node_text(exp_spec.name()))
                .or(preferred_export);
            let named = decl.parent()?;
            let export_decl = named.parent()?;
            let spec_node = export_decl.module_specifier()?;
            Some(ModuleProvenance {
                containing_sf: sf,
                specifier_node: spec_node,
                specifier_text: string_literal_text(spec_node)?,
                export_name,
            })
        }
        _ => None,
    }
}

fn provenance_from_require_decl(
    decl: P<Node>,
    preferred_export: Option<String>,
) -> Option<ModuleProvenance> {
    let sf = get_source_file_of_node(decl)?;
    match decl.kind() {
        Kind::VariableDeclaration => {
            let init = unwrap_expr_wrappers(decl.initializer()?);
            if let Some(spec_node) = extract_require_specifier_node(init) {
                return Some(ModuleProvenance {
                    containing_sf: sf,
                    specifier_node: spec_node,
                    specifier_text: string_literal_text(spec_node)?,
                    export_name: preferred_export,
                });
            }
            if init.kind() == Kind::PropertyAccessExpression {
                let pa = init.as_property_access_expression();
                if let Some(spec_node) = extract_require_specifier_node(pa.expression()) {
                    return Some(ModuleProvenance {
                        containing_sf: sf,
                        specifier_node: spec_node,
                        specifier_text: string_literal_text(spec_node)?,
                        export_name: preferred_export.or_else(|| node_text(pa.name())),
                    });
                }
            }
            None
        }
        Kind::BindingElement => {
            let be = decl.as_binding_element();
            let prop = be
                .property_name()
                .and_then(prop_key_text)
                .or_else(|| be.name().and_then(node_text))
                .or(preferred_export);
            let pattern = decl.parent()?;
            let var_decl = pattern.parent()?;
            if var_decl.kind() == Kind::VariableDeclaration {
                let init = unwrap_expr_wrappers(var_decl.initializer()?);
                if let Some(spec_node) = extract_require_specifier_node(init) {
                    return Some(ModuleProvenance {
                        containing_sf: sf,
                        specifier_node: spec_node,
                        specifier_text: string_literal_text(spec_node)?,
                        export_name: prop,
                    });
                }
            }
            None
        }
        _ => None,
    }
}

fn trace_symbol_package_provenance(
    checker: &mut CheckerHandle,
    start_sym: P<Symbol>,
    preferred_export: Option<String>,
    deepest_only: bool,
) -> Option<ModuleProvenance> {
    let mut current = start_sym;
    let mut export_override = preferred_export;
    let mut found: Option<ModuleProvenance> = None;
    let mut visited = Vec::new();

    for _ in 0..MAX_ALIAS_HOPS {
        if visited.contains(&current) {
            break;
        }
        visited.push(current);

        for &decl in current.declarations() {
            if let Some(prov) = provenance_from_import_decl_node(decl, export_override.clone())
                && is_bare_package_specifier(&prov.specifier_text)
            {
                if !deepest_only {
                    return Some(prov);
                }
                found = Some(prov);
            }
            if let Some(prov) = provenance_from_require_decl(decl, export_override.clone())
                && is_bare_package_specifier(&prov.specifier_text)
            {
                if !deepest_only {
                    return Some(prov);
                }
                found = Some(prov);
            }
        }

        if let Some(vd) = current.value_declaration() {
            if let Some(prov) = provenance_from_import_decl_node(vd, export_override.clone())
                && is_bare_package_specifier(&prov.specifier_text)
            {
                if !deepest_only {
                    return Some(prov);
                }
                found = Some(prov);
            }
            if let Some(prov) = provenance_from_require_decl(vd, export_override.clone())
                && is_bare_package_specifier(&prov.specifier_text)
            {
                if !deepest_only {
                    return Some(prov);
                }
                found = Some(prov);
            }
        }

        for &decl in current.declarations() {
            if let Some(orig) = export_name_from_alias_decl(decl)
                && (deepest_only || export_override.is_none())
            {
                export_override = Some(orig);
            }
        }

        if !current.flags().intersects(SymbolFlags::Alias) {
            break;
        }
        let next = checker.get_aliased_symbol(current);
        if next == current || next == checker.get_unknown_symbol() {
            break;
        }
        current = next;
    }

    found
}

fn find_provenance_for_expr(
    checker: &mut CheckerHandle,
    expr: P<Node>,
    deepest_only: bool,
) -> Option<ModuleProvenance> {
    let unwrapped = unwrap_expr_wrappers(expr);
    if let Some(spec_node) = extract_require_specifier_node(unwrapped) {
        let sf = get_source_file_of_node(spec_node)?;
        return Some(ModuleProvenance {
            containing_sf: sf,
            specifier_node: spec_node,
            specifier_text: string_literal_text(spec_node)?,
            export_name: Some("default".to_string()),
        });
    }

    if unwrapped.kind() == Kind::PropertyAccessExpression {
        let pa = unwrapped.as_property_access_expression();
        let prop_name = node_text(pa.name());
        if let Some(spec_node) = extract_require_specifier_node(pa.expression()) {
            let sf = get_source_file_of_node(spec_node)?;
            return Some(ModuleProvenance {
                containing_sf: sf,
                specifier_node: spec_node,
                specifier_text: string_literal_text(spec_node)?,
                export_name: prop_name,
            });
        }
        if let Some(recv_sym) = checker.get_symbol_at_location_exported(pa.expression())
            && let Some(prov) =
                trace_symbol_package_provenance(checker, recv_sym, prop_name, deepest_only)
        {
            return Some(prov);
        }
    }

    if let Some(raw_sym) = checker.get_symbol_at_location_exported(unwrapped)
        && let Some(prov) = trace_symbol_package_provenance(checker, raw_sym, None, deepest_only)
    {
        return Some(prov);
    }

    None
}

fn find_package_info(
    program: &'static Program,
    file_path: &str,
    expected_pkg_name: Option<&str>,
) -> Option<(String, Option<P<InfoCacheEntry>>)> {
    let mut search_dir = tspath::get_directory_path(&normalize_path(file_path));
    for _ in 0..8 {
        if search_dir.is_empty() {
            break;
        }
        let pkg_dir = program.get_nearest_ancestor_directory_with_package_json(&search_dir);
        if pkg_dir.is_empty() {
            break;
        }
        let pkg_json_path = tspath::combine_paths(&pkg_dir, &["package.json"]);
        let info = program.get_package_json_info(&pkg_json_path);
        let matches = info.is_some_and(|entry| {
            let Some(contents) = entry.contents else {
                return false;
            };
            let fields = &contents.get().fields;
            if let Some(expected) = expected_pkg_name {
                return fields.name.get_value().is_some_and(|n| n == expected);
            }
            fields.name.get_value().is_some()
                || fields.exports.type_() != JSONValueType::NotPresent
                || fields.main.get_value().is_some()
        });
        if matches {
            return Some((pkg_dir, info));
        }
        let parent = tspath::get_directory_path(&pkg_dir);
        if parent == pkg_dir {
            break;
        }
        search_dir = parent;
    }
    None
}

fn package_name_for_file(
    program: &'static Program,
    file_path: &str,
    provenance: Option<&ModuleProvenance>,
) -> Option<String> {
    if let Some(prov) = provenance
        && let Some(resolved) = program
            .get_resolved_module_from_module_specifier(prov.containing_sf, prov.specifier_node)
        && !resolved.package_id.name.is_empty()
    {
        return Some(resolved.package_id.name.to_string());
    }
    if let Some((_, Some(entry))) = find_package_info(program, file_path, None)
        && let Some(contents) = entry.contents
        && let Some(name) = contents.get().fields.name.get_value()
    {
        return Some(name.clone());
    }
    provenance.map(|p| split_package_specifier(&p.specifier_text).0)
}

fn find_source_file_in_programs(
    primary: &'static Program,
    workspace_programs: &[&'static Program],
    file_name: &str,
) -> Option<(&'static Program, P<SourceFile>)> {
    if let Some(sf) = primary.get_source_file(file_name) {
        return Some((primary, sf));
    }
    for &prog in workspace_programs {
        if let Some(sf) = prog.get_source_file(file_name) {
            return Some((prog, sf));
        }
    }
    let target_canon = canonical_or_normalized(file_name);
    for prog in std::iter::once(primary).chain(workspace_programs.iter().copied()) {
        for &sf in prog.get_source_files() {
            if canonical_or_normalized(sf.file_name()) == target_canon {
                return Some((prog, sf));
            }
        }
    }
    None
}

const fn is_declaration_node_kind(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::FunctionDeclaration
            | Kind::FunctionExpression
            | Kind::ArrowFunction
            | Kind::MethodDeclaration
            | Kind::Constructor
            | Kind::GetAccessor
            | Kind::SetAccessor
            | Kind::ClassDeclaration
            | Kind::ClassExpression
            | Kind::VariableDeclaration
            | Kind::PropertyDeclaration
            | Kind::PropertyAssignment
            | Kind::ShorthandPropertyAssignment
            | Kind::ExportAssignment
            | Kind::ExportSpecifier
            | Kind::InterfaceDeclaration
            | Kind::TypeAliasDeclaration
            | Kind::EnumDeclaration
    )
}

fn find_declaration_at_offset(sf: P<SourceFile>, offset: u32) -> Option<P<Node>> {
    fn visit(sf: P<SourceFile>, node: P<Node>, offset: u32, best: &mut Option<P<Node>>) {
        let start = get_token_pos_of_node(node, sf, false);
        let end = node.end().source_offset().unwrap_or(start).max(start);
        if offset < start || offset > end {
            return;
        }
        if is_declaration_node_kind(node.kind()) {
            *best = Some(node);
        }
        node.for_each_child(&mut |child| {
            visit(sf, child, offset, best);
            false
        });
    }

    let mut best = None;
    visit(sf, sf.as_node(), offset, &mut best);
    best
}

fn pick_bodied_declaration(
    sym_opt: Option<P<Symbol>>,
    preferred: Option<P<Node>>,
) -> Option<P<Node>> {
    if let Some(sym) = sym_opt {
        if let Some(vd) = sym.value_declaration()
            && get_source_file_of_node(vd).is_some_and(|sf| !sf.is_declaration_file())
            && extract_callable_from_decl(vd).is_some_and(|f| f.body().is_some())
        {
            return Some(vd);
        }
        for &decl in sym.declarations() {
            if get_source_file_of_node(decl).is_some_and(|sf| !sf.is_declaration_file())
                && extract_callable_from_decl(decl).is_some_and(|f| f.body().is_some())
            {
                return Some(decl);
            }
        }
    }
    preferred.or_else(|| {
        sym_opt.and_then(|s| {
            s.value_declaration()
                .or_else(|| s.declarations().first().copied())
        })
    })
}

fn resolve_sibling_bodied_declaration(sf: P<SourceFile>, decl: P<Node>) -> Option<P<Node>> {
    if extract_callable_from_decl(decl).is_some_and(|f| f.body().is_some()) {
        return Some(decl);
    }
    let name = decl.name().and_then(node_text)?;
    let parent = decl.parent()?;
    if parent.kind() == Kind::SourceFile {
        for &stmt in sf.statements.nodes() {
            if stmt.kind() == decl.kind()
                && stmt.name().and_then(node_text).as_deref() == Some(name.as_str())
                && stmt.body().is_some()
            {
                return Some(stmt);
            }
        }
    } else if matches!(
        parent.kind(),
        Kind::ClassDeclaration | Kind::ClassExpression
    ) {
        let members = if parent.kind() == Kind::ClassDeclaration {
            parent.as_class_declaration().members().nodes()
        } else {
            parent.as_class_expression().members().nodes()
        };
        for &member in members {
            if member.kind() == decl.kind()
                && member.name().and_then(node_text).as_deref() == Some(name.as_str())
                && member.body().is_some()
            {
                return Some(member);
            }
        }
    }
    None
}

fn is_unbodied_callable_or_signature(node: P<Node>) -> bool {
    if is_function_like_declaration(Some(node)) {
        return node.body().is_none();
    }
    matches!(
        node.kind(),
        Kind::MethodSignature | Kind::CallSignature | Kind::FunctionType | Kind::Parameter
    )
}

fn format_first_party_dts_gap_message(decl_name: &str, decl_file_rel: &str) -> String {
    format!(
        "Declaration '{decl_name}' in '{decl_file_rel}' belongs to a workspace package, but only its type declaration (.d.ts) was resolved. Enable TypeScript project references (tsconfig references with composite: true), compilerOptions.customConditions pointing to source exports, or compilerOptions.declarationMap (.d.ts.map) with the package project included to locate its TypeScript source."
    )
}

impl SemanticFacade<'_> {
    #[must_use]
    pub fn is_first_party_source_file(&self, sf: P<SourceFile>) -> bool {
        is_first_party_source_file(self.cwd(), self.program(), sf)
    }

    /// Resolves package name and exported symbol name provenance for a declaration.
    pub fn resolve_declaration_provenance(
        &mut self,
        node: P<Node>,
        declaration: P<Node>,
    ) -> DeclarationProvenance {
        let Some(decl_sf) = get_source_file_of_node(declaration) else {
            return DeclarationProvenance::default();
        };
        if self
            .program()
            .is_source_file_default_library(decl_sf.path())
        {
            return DeclarationProvenance::default();
        }

        let expr = if matches!(node.kind(), Kind::CallExpression | Kind::NewExpression) {
            let Some(callee) = node.expression() else {
                return DeclarationProvenance::default();
            };
            unwrap_expr_wrappers(callee)
        } else {
            unwrap_expr_wrappers(node)
        };

        let Some(prov) = find_provenance_for_expr(self.checker_mut(), expr, true) else {
            return DeclarationProvenance::default();
        };
        if !is_bare_package_specifier(&prov.specifier_text) {
            return DeclarationProvenance::default();
        }

        let (spec_pkg_name, _) = split_package_specifier(&prov.specifier_text);
        let decl_pkg_name = find_package_info(self.program(), decl_sf.file_name(), None)
            .and_then(|(_, info)| info)
            .and_then(|entry| entry.contents)
            .and_then(|contents| contents.get().fields.name.get_value().cloned());

        let resolved_module = self
            .program()
            .get_resolved_module_from_module_specifier(prov.containing_sf, prov.specifier_node)
            .is_some_and(|resolved| resolved.is_resolved());
        if !resolved_module {
            return DeclarationProvenance::default();
        }

        let same_package = decl_pkg_name.as_deref() == Some(spec_pkg_name.as_str());
        let package_name = decl_pkg_name.or(Some(spec_pkg_name));
        let export_name = if same_package {
            prov.export_name
                .or_else(|| declaration.name().and_then(node_text))
        } else {
            declaration.name().and_then(node_text).or(prov.export_name)
        };

        DeclarationProvenance {
            package_name,
            export_name,
        }
    }

    /// Reports a `missing-package` gap when a call's callee imports from an unresolved package specifier.
    pub fn check_call_missing_package_gap(&mut self, call_node: P<Node>) -> Option<GapItem> {
        let callee_expr = call_node.expression()?;
        let prov = find_provenance_for_expr(self.checker_mut(), callee_expr, false)?;
        if !is_bare_package_specifier(&prov.specifier_text) {
            return None;
        }
        let resolved = self
            .program()
            .get_resolved_module_from_module_specifier(prov.containing_sf, prov.specifier_node);
        if resolved.is_some_and(|r| r.is_resolved()) {
            return None;
        }
        Some(GapItem {
            kind: "missing-package",
            message: format!(
                "Package specifier '{}' is not installed or could not be resolved from '{}'.",
                prov.specifier_text,
                relative_to_cwd(self.cwd(), prov.containing_sf.file_name())
            ),
            at: node_location_relative(self.cwd(), call_node),
        })
    }

    /// Locates first-party implementation sources using TypeScript project references,
    /// `customConditions`, and `declarationMap`, treating third-party and stdlib symbols as external.
    #[expect(
        clippy::too_many_lines,
        reason = "Coordinates symbol/module provenance, external filtering, and the three compiler-standard first-party mappings."
    )]
    pub fn resolve_implementation(
        &mut self,
        input: FacadeImplementationInput,
        workspace_programs: &[&'static Program],
        sourcemap_cache: &SourceMapCache,
    ) -> FacadeImplementationOutcome {
        let program = self.program();
        let cwd = self.cwd().to_string();

        let (anchor_node, provenance, mut decl_opt, mut sym_opt) = match input {
            FacadeImplementationInput::Node(node) => {
                if matches!(node.kind(), Kind::CallExpression | Kind::NewExpression) {
                    let prov = node
                        .expression()
                        .and_then(|e| find_provenance_for_expr(self.checker_mut(), e, false));
                    let resolved = resolve_symbol_for_node(self.checker_mut(), node);
                    let decl = pick_bodied_declaration(resolved.symbol, resolved.primary_decl);
                    (Some(node), prov, decl, resolved.symbol)
                } else {
                    let prov = provenance_from_import_decl_node(node, None)
                        .or_else(|| {
                            node.parent().and_then(|parent| {
                                provenance_from_import_decl_node(parent, None).or_else(|| {
                                    extract_require_specifier_node(parent).and_then(|spec| {
                                        let sf = get_source_file_of_node(spec)?;
                                        Some(ModuleProvenance {
                                            containing_sf: sf,
                                            specifier_node: spec,
                                            specifier_text: string_literal_text(spec)?,
                                            export_name: None,
                                        })
                                    })
                                })
                            })
                        })
                        .or_else(|| find_provenance_for_expr(self.checker_mut(), node, false));
                    let resolved = resolve_symbol_for_node(self.checker_mut(), node);
                    let decl = pick_bodied_declaration(resolved.symbol, resolved.primary_decl);
                    (Some(node), prov, decl, resolved.symbol)
                }
            }
            FacadeImplementationInput::Declaration(decl, sym) => {
                let prov = sym.and_then(|s| {
                    trace_symbol_package_provenance(self.checker_mut(), s, None, false)
                });
                let effective_decl = pick_bodied_declaration(sym, Some(decl));
                (Some(decl), prov, effective_decl, sym)
            }
        };

        let gap_at = anchor_node.and_then(|n| node_location_relative(&cwd, n));

        // 1. Check unresolved bare package specifiers before concluding on declarations.
        if let Some(ref prov) = provenance
            && is_bare_package_specifier(&prov.specifier_text)
        {
            let resolved_mod = program
                .get_resolved_module_from_module_specifier(prov.containing_sf, prov.specifier_node);
            if resolved_mod.is_none_or(|r| !r.is_resolved()) {
                return FacadeImplementationOutcome::Inconclusive {
                    declaration_file: None,
                    gaps: vec![GapItem::new(
                        "missing-package",
                        format!(
                            "Package specifier '{}' could not be resolved from '{}'.",
                            prov.specifier_text,
                            relative_to_cwd(&cwd, prov.containing_sf.file_name())
                        ),
                        gap_at,
                    )],
                    evidence: Vec::new(),
                };
            }

            // Module-level import (e.g. `ImportDeclaration` or specifier string) without a value declaration.
            if decl_opt.is_none()
                && let Some(resolved) = resolved_mod
                && let Some((_, target_sf)) = find_source_file_in_programs(
                    program,
                    workspace_programs,
                    resolved.resolved_file_name,
                )
            {
                if program.is_source_file_default_library(target_sf.path())
                    || !is_first_party_workspace_path(&cwd, target_sf.file_name())
                {
                    let pkg_name = if resolved.package_id.name.is_empty() {
                        Some(split_package_specifier(&prov.specifier_text).0)
                    } else {
                        Some(resolved.package_id.name.to_string())
                    };
                    let decl_rel = relative_to_cwd(&cwd, target_sf.file_name());
                    return FacadeImplementationOutcome::External {
                        package_name: pkg_name.clone(),
                        declaration_file: Some(decl_rel.clone()),
                        evidence: vec![EvidenceItem::simple(
                            "note",
                            format!(
                                "External module '{}' ({}) is outside the workspace and is not followed into implementation code.",
                                pkg_name.as_deref().unwrap_or(&prov.specifier_text),
                                decl_rel
                            ),
                            gap_at,
                        )],
                    };
                }
                decl_opt = Some(target_sf.as_node());
                sym_opt = target_sf.as_node().symbol();
            }
        }

        let Some(decl_node) = decl_opt else {
            return FacadeImplementationOutcome::Inconclusive {
                declaration_file: None,
                gaps: vec![GapItem::from_reason(
                    UnknownReason::UnresolvedDeclaration,
                    "Target has no statically resolved declaration.".to_string(),
                    gap_at,
                )],
                evidence: Vec::new(),
            };
        };

        let Some(decl_sf) = get_source_file_of_node(decl_node) else {
            return FacadeImplementationOutcome::Inconclusive {
                declaration_file: None,
                gaps: vec![GapItem::from_reason(
                    UnknownReason::UnresolvedDeclaration,
                    "Resolved declaration has no containing source file.".to_string(),
                    gap_at,
                )],
                evidence: Vec::new(),
            };
        };

        let decl_file_rel = relative_to_cwd(&cwd, decl_sf.file_name());
        let decl_loc = node_location_relative(&cwd, decl_node);
        let export_name = provenance
            .as_ref()
            .and_then(|p| p.export_name.clone())
            .or_else(|| decl_node.name().and_then(node_text))
            .or_else(|| sym_opt.map(|s| s.name().to_string()));

        // 2. Standard library and third-party `node_modules` declarations are never followed.
        if program.is_source_file_default_library(decl_sf.path()) {
            return FacadeImplementationOutcome::External {
                package_name: None,
                declaration_file: Some(decl_file_rel.clone()),
                evidence: vec![EvidenceItem::simple(
                    "type",
                    format!("Target resolves to standard library declaration '{decl_file_rel}'."),
                    decl_loc,
                )],
            };
        }

        if !is_first_party_workspace_path(&cwd, decl_sf.file_name()) {
            let pkg_name = package_name_for_file(program, decl_sf.file_name(), provenance.as_ref());
            return FacadeImplementationOutcome::External {
                package_name: pkg_name.clone(),
                declaration_file: Some(decl_file_rel.clone()),
                evidence: vec![EvidenceItem::simple(
                    "type",
                    format!(
                        "Target belongs to external package '{}' ('{}') and is not followed into implementation code.",
                        pkg_name.as_deref().unwrap_or("<external>"),
                        decl_file_rel
                    ),
                    decl_loc,
                )],
            };
        }

        // 3. First-party non-declaration source file: determine whether resolved via project reference,
        // `customConditions`, or directly in the current project.
        if !decl_sf.is_declaration_file() {
            if is_unbodied_callable_or_signature(decl_node) {
                return FacadeImplementationOutcome::Inconclusive {
                    declaration_file: None,
                    gaps: vec![GapItem::from_reason(
                        UnknownReason::UnresolvedDeclaration,
                        format!(
                            "Declaration '{}' in '{}' has no implementation body.",
                            export_name.as_deref().unwrap_or("<symbol>"),
                            decl_file_rel
                        ),
                        gap_at.or(decl_loc),
                    )],
                    evidence: Vec::new(),
                };
            }

            let resolution_source = self.classify_first_party_source(decl_sf, provenance.as_ref());
            let pkg_name = find_package_info(program, decl_sf.file_name(), None)
                .and_then(|(_, info)| info)
                .and_then(|entry| entry.contents)
                .and_then(|contents| contents.get().fields.name.get_value().cloned())
                .or_else(|| {
                    provenance
                        .as_ref()
                        .filter(|p| is_bare_package_specifier(&p.specifier_text))
                        .map(|p| split_package_specifier(&p.specifier_text).0)
                });
            let fn_node = extract_callable_from_decl(decl_node).filter(|f| f.body().is_some());
            let location = decl_loc.unwrap_or_else(|| DiagnosticLocation {
                file: decl_file_rel.clone(),
                line: 1,
                column: 1,
                end_line: 1,
                end_column: 1,
                start: 0,
                end: 0,
            });
            return FacadeImplementationOutcome::Resolved(FacadeImplementationTarget {
                file: decl_file_rel,
                location,
                declaration_node: decl_node,
                declaration_symbol: sym_opt,
                fn_node,
                export_name,
                package_name: pkg_name,
                resolution_source,
                declaration_file: None,
            });
        }

        // 4. First-party `.d.ts` file: try Priority 1 (parse redirect) then Priority 3 (`declarationMap`).
        let redirect = program.get_parse_file_redirect(decl_sf.file_name());
        if !redirect.is_empty()
            && is_first_party_workspace_path(&cwd, &redirect)
            && let Some((_, target_sf)) =
                find_source_file_in_programs(program, workspace_programs, &redirect)
            && !target_sf.is_declaration_file()
        {
            let target_decl = target_sf.as_node();
            let target_rel = relative_to_cwd(&cwd, target_sf.file_name());
            let location =
                node_location_relative(&cwd, target_decl).unwrap_or_else(|| DiagnosticLocation {
                    file: target_rel.clone(),
                    line: 1,
                    column: 1,
                    end_line: 1,
                    end_column: 1,
                    start: 0,
                    end: 0,
                });
            let pkg_name =
                package_name_for_file(program, target_sf.file_name(), provenance.as_ref());
            return FacadeImplementationOutcome::Resolved(FacadeImplementationTarget {
                file: target_rel,
                location,
                declaration_node: target_decl,
                declaration_symbol: target_decl.symbol(),
                fn_node: None,
                export_name,
                package_name: pkg_name,
                resolution_source: FirstPartyResolutionSource::ProjectReference,
                declaration_file: Some(decl_file_rel),
            });
        }

        let pkg_name = package_name_for_file(program, decl_sf.file_name(), provenance.as_ref());
        if let Some(mapper) =
            sourcemap_cache.get_or_load(program, workspace_programs, decl_sf.file_name())
        {
            let anchor_pos = decl_node.name().map_or_else(
                || get_token_pos_of_node(decl_node, decl_sf, false),
                |name_node| get_token_pos_of_node(name_node, decl_sf, false),
            );
            let mapped = mapper
                .get_source_position(&DocumentPosition {
                    file_name: decl_sf.file_name().to_string(),
                    pos: anchor_pos,
                })
                .or_else(|| {
                    let node_pos = get_token_pos_of_node(decl_node, decl_sf, false);
                    (node_pos != anchor_pos).then(|| {
                        mapper.get_source_position(&DocumentPosition {
                            file_name: decl_sf.file_name().to_string(),
                            pos: node_pos,
                        })
                    })?
                });

            if let Some(source_pos) = mapped
                && is_first_party_workspace_path(&cwd, &source_pos.file_name)
            {
                if let Some((target_program, target_sf)) =
                    find_source_file_in_programs(program, workspace_programs, &source_pos.file_name)
                    && !target_sf.is_declaration_file()
                    && let Some(raw_impl_decl) =
                        find_declaration_at_offset(target_sf, source_pos.pos)
                {
                    let resolved_impl_decl = if std::ptr::eq(target_program, program) {
                        let resolved = resolve_symbol_for_node(self.checker_mut(), raw_impl_decl);
                        pick_bodied_declaration(resolved.symbol, resolved.primary_decl)
                            .unwrap_or(raw_impl_decl)
                    } else {
                        resolve_sibling_bodied_declaration(target_sf, raw_impl_decl)
                            .unwrap_or(raw_impl_decl)
                    };
                    let final_sf = get_source_file_of_node(resolved_impl_decl).unwrap_or(target_sf);
                    let final_rel = relative_to_cwd(&cwd, final_sf.file_name());
                    let location =
                        node_location_relative(&cwd, resolved_impl_decl).unwrap_or_else(|| {
                            DiagnosticLocation {
                                file: final_rel.clone(),
                                line: 1,
                                column: 1,
                                end_line: 1,
                                end_column: 1,
                                start: 0,
                                end: 0,
                            }
                        });
                    let fn_node = extract_callable_from_decl(resolved_impl_decl)
                        .filter(|f| f.body().is_some());
                    let final_sym = resolved_impl_decl.symbol();
                    return FacadeImplementationOutcome::Resolved(FacadeImplementationTarget {
                        file: final_rel,
                        location,
                        declaration_node: resolved_impl_decl,
                        declaration_symbol: final_sym,
                        fn_node,
                        export_name,
                        package_name: pkg_name,
                        resolution_source: FirstPartyResolutionSource::DeclarationMap,
                        declaration_file: Some(decl_file_rel),
                    });
                }

                let mapped_rel = relative_to_cwd(&cwd, &source_pos.file_name);
                return FacadeImplementationOutcome::Inconclusive {
                    declaration_file: Some(decl_file_rel.clone()),
                    gaps: vec![GapItem::from_reason(
                        UnknownReason::MissingSourceFile,
                        format!(
                            "Declaration '{}' in '{}' maps via declarationMap to '{}', which is not loaded in any opened TypeScript project. Include the package's tsconfig.json, or enable TypeScript project references (tsconfig references with composite: true) or compilerOptions.customConditions pointing to source exports.",
                            export_name.as_deref().unwrap_or("<symbol>"),
                            decl_file_rel,
                            mapped_rel
                        ),
                        gap_at.or(decl_loc),
                    )],
                    evidence: Vec::new(),
                };
            }
        }

        FacadeImplementationOutcome::Inconclusive {
            declaration_file: Some(decl_file_rel.clone()),
            gaps: vec![GapItem::from_reason(
                UnknownReason::MissingSourceFile,
                format_first_party_dts_gap_message(
                    export_name.as_deref().unwrap_or("<symbol>"),
                    &decl_file_rel,
                ),
                gap_at.or(decl_loc),
            )],
            evidence: Vec::new(),
        }
    }

    fn classify_first_party_source(
        &self,
        sf: P<SourceFile>,
        provenance: Option<&ModuleProvenance>,
    ) -> FirstPartyResolutionSource {
        let program = self.program();
        if program.is_source_from_project_reference(sf.path()) {
            return FirstPartyResolutionSource::ProjectReference;
        }
        let has_custom_conditions = program
            .options()
            .custom_conditions
            .as_deref()
            .is_some_and(|conds| !conds.is_empty());
        if has_custom_conditions
            && (program.is_source_file_from_external_library(sf)
                || provenance.is_some_and(|p| is_bare_package_specifier(&p.specifier_text)))
        {
            return FirstPartyResolutionSource::CustomConditions;
        }
        FirstPartyResolutionSource::CurrentProject
    }
}
