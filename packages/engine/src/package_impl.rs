use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use tsrs_ast::{
    ExternalModuleIndicatorOptions, Kind, Node, SourceFile, SourceFileParseOptions, Symbol,
    SymbolFlags, get_source_file_of_node, is_function_like_declaration, set_parent_in_children,
};
use tsrs_binder::bind_source_file;
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::{ModuleKind, ModuleResolutionKind, P, ResolutionMode, ScriptKind, tspath};
use tsrs_module::packagejson::{ExportsOrImports, JSONValueType};
use tsrs_parser::parse_source_file_keep_text;

use crate::calls_values::{
    canonical_symbol_for_node, extract_callable_from_decl, node_location_rel, node_text,
    pick_preferred_impl_decl, prop_key_text, resolve_call_targets, unwrap_expr_wrappers,
};
use crate::cfg::{EvidenceItem, GapItem};
use crate::projects::{is_project_source_file, normalize_path, relative_to_cwd};
use crate::serve::{HandlePayload, RpcErr, SnapshotState, SourceLocation};

const MAX_ALIAS_HOPS: usize = 16;
const MAX_REEXPORT_DEPTH: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PackageResolutionMode {
    Import,
    Require,
    Unknown,
}

impl PackageResolutionMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Require => "require",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedImplTargetPayload {
    pub file: String,
    pub location: Option<SourceLocation>,
    pub export_name: Option<String>,
    pub is_external: bool,
    pub package_name: Option<String>,
    pub package_version: Option<String>,
    pub subpath: Option<String>,
    pub resolution_mode: &'static str,
    pub conditions: Vec<String>,
    pub declaration_file: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImplementationResolutionRow {
    pub target: String,
    pub status: &'static str,
    pub implementation: Option<ResolvedImplTargetPayload>,
    pub candidates: Vec<ResolvedImplTargetPayload>,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

#[derive(Default, Clone)]
pub struct PackageImplCache {
    parsed_files: Arc<Mutex<FxHashMap<String, Option<P<SourceFile>>>>>,
}

impl PackageImplCache {
    pub fn load_external_source_file(&self, cwd: &str, abs_path: &str) -> Option<P<SourceFile>> {
        let norm = normalize_path(abs_path);
        if let Ok(guard) = self.parsed_files.lock()
            && let Some(&cached) = guard.get(&norm)
        {
            return cached;
        }

        let parsed = parse_external_file_from_disk(cwd, &norm);
        if let Ok(mut guard) = self.parsed_files.lock() {
            guard.insert(norm, parsed);
        }
        parsed
    }
}

fn has_ext(path: &str, exts: &[&str]) -> bool {
    Path::new(path).extension().is_some_and(|ext| {
        exts.iter()
            .any(|candidate| ext.eq_ignore_ascii_case(candidate))
    })
}

fn script_kind_for_path(path: &str) -> Option<ScriptKind> {
    if tspath::is_declaration_file_name(path) {
        return None;
    }
    if has_ext(path, &["tsx"]) {
        Some(ScriptKind::TSX)
    } else if has_ext(path, &["ts", "mts", "cts"]) {
        Some(ScriptKind::TS)
    } else if has_ext(path, &["jsx"]) {
        Some(ScriptKind::JSX)
    } else if has_ext(path, &["js", "mjs", "cjs"]) {
        Some(ScriptKind::JS)
    } else {
        None
    }
}

fn parse_external_file_from_disk(cwd: &str, abs_path: &str) -> Option<P<SourceFile>> {
    let script_kind = script_kind_for_path(abs_path)?;
    let text = fs::read_to_string(abs_path).ok()?;
    let force_module = has_ext(abs_path, &["mjs", "mts"]);
    let opts = SourceFileParseOptions {
        file_name: abs_path.to_string(),
        path: tspath::to_path(abs_path, cwd, true),
        external_module_indicator_options: ExternalModuleIndicatorOptions {
            jsx: matches!(script_kind, ScriptKind::JSX | ScriptKind::TSX),
            force: force_module,
        },
    };
    let sf = parse_source_file_keep_text(opts, text, script_kind);
    set_parent_in_children(sf.as_node());
    bind_source_file(sf);
    Some(sf)
}

#[derive(Clone)]
pub enum PackageImplLookup {
    SourceBody {
        fn_node: P<Node>,
        info: ResolvedImplTargetPayload,
    },
    ResolvedExternal {
        fn_node: P<Node>,
        info: ResolvedImplTargetPayload,
    },
    DeclarationOnly {
        message: String,
    },
    MissingPackage {
        message: String,
    },
    Ambiguous {
        message: String,
        candidates: Vec<ResolvedImplTargetPayload>,
    },
    Unimplemented {
        message: String,
    },
}

struct ProvenanceSpec {
    containing_sf: P<SourceFile>,
    specifier_node: Option<P<Node>>,
    specifier_text: Option<String>,
    export_name: Option<String>,
    container_name: Option<String>,
    resolution_mode: PackageResolutionMode,
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

fn determine_resolution_mode(
    program: &'static Program,
    containing_sf: P<SourceFile>,
    spec_node: Option<P<Node>>,
) -> PackageResolutionMode {
    if let Some(spec) = spec_node {
        if let Some(parent) = spec.parent() {
            if parent.kind() == Kind::CallExpression
                && extract_require_specifier_node(parent).is_some()
            {
                return PackageResolutionMode::Require;
            }
            if parent.kind() == Kind::ExternalModuleReference {
                return PackageResolutionMode::Require;
            }
        }
        let mode: ResolutionMode = program.get_mode_for_usage_location(containing_sf, spec);
        if mode == ModuleKind::ES_NEXT {
            return PackageResolutionMode::Import;
        }
        if mode == ModuleKind::COMMON_JS {
            return PackageResolutionMode::Require;
        }
    }

    let default_mode = program.get_default_resolution_mode_for_file(containing_sf);
    if default_mode == ModuleKind::ES_NEXT {
        return PackageResolutionMode::Import;
    }
    if default_mode == ModuleKind::COMMON_JS {
        return PackageResolutionMode::Require;
    }

    if let Some(opts) = program.command_line().compiler_options() {
        let emit_module = opts.get_emit_module_kind();
        let mod_res = opts.get_module_resolution_kind();
        if emit_module == ModuleKind::COMMON_JS {
            return PackageResolutionMode::Require;
        }
        if mod_res == ModuleResolutionKind::BUNDLER
            || emit_module.is_non_node_esm()
            || emit_module == ModuleKind::PRESERVE
        {
            return PackageResolutionMode::Import;
        }
    }
    let fname = containing_sf.file_name();
    if has_ext(fname, &["cts", "cjs"]) {
        return PackageResolutionMode::Require;
    }
    if has_ext(fname, &["mts", "mjs"]) {
        return PackageResolutionMode::Import;
    }
    PackageResolutionMode::Unknown
}

fn collect_active_conditions(
    program: &'static Program,
    mode: PackageResolutionMode,
) -> Vec<String> {
    let mut conditions = Vec::new();
    match mode {
        PackageResolutionMode::Import => conditions.push("import".to_string()),
        PackageResolutionMode::Require => conditions.push("require".to_string()),
        PackageResolutionMode::Unknown => {}
    }
    if let Some(opts) = program.command_line().compiler_options() {
        if opts.get_module_resolution_kind() != ModuleResolutionKind::BUNDLER {
            conditions.push("node".to_string());
        }
        if let Some(custom) = opts.custom_conditions.as_deref() {
            for c in custom {
                if !conditions.iter().any(|existing| existing == c) {
                    conditions.push(c.clone());
                }
            }
        }
    }
    conditions
}

#[expect(
    clippy::too_many_lines,
    reason = "Matches all TS/JS import and re-export declaration AST shapes."
)]
fn spec_from_import_decl_node(
    program: &'static Program,
    decl: P<Node>,
    preferred_export: Option<String>,
) -> Option<ProvenanceSpec> {
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
            let spec_text = string_literal_text(spec_node);
            let mode = determine_resolution_mode(program, sf, Some(spec_node));
            Some(ProvenanceSpec {
                containing_sf: sf,
                specifier_node: Some(spec_node),
                specifier_text: spec_text,
                export_name,
                container_name: None,
                resolution_mode: mode,
            })
        }
        Kind::ImportClause => {
            let import_decl = decl.parent()?;
            let spec_node = import_decl.module_specifier()?;
            let spec_text = string_literal_text(spec_node);
            let mode = determine_resolution_mode(program, sf, Some(spec_node));
            Some(ProvenanceSpec {
                containing_sf: sf,
                specifier_node: Some(spec_node),
                specifier_text: spec_text,
                export_name: preferred_export.or_else(|| Some("default".to_string())),
                container_name: None,
                resolution_mode: mode,
            })
        }
        Kind::NamespaceImport => {
            let clause = decl.parent()?;
            let import_decl = clause.parent()?;
            let spec_node = import_decl.module_specifier()?;
            let spec_text = string_literal_text(spec_node);
            let mode = determine_resolution_mode(program, sf, Some(spec_node));
            Some(ProvenanceSpec {
                containing_sf: sf,
                specifier_node: Some(spec_node),
                specifier_text: spec_text,
                export_name: preferred_export,
                container_name: None,
                resolution_mode: mode,
            })
        }
        Kind::ImportDeclaration => {
            let spec_node = decl.module_specifier()?;
            let spec_text = string_literal_text(spec_node);
            let mode = determine_resolution_mode(program, sf, Some(spec_node));
            Some(ProvenanceSpec {
                containing_sf: sf,
                specifier_node: Some(spec_node),
                specifier_text: spec_text,
                export_name: preferred_export,
                container_name: None,
                resolution_mode: mode,
            })
        }
        Kind::ImportEqualsDeclaration => {
            let mod_ref = decl.as_import_equals_declaration().module_reference();
            if mod_ref.kind() != Kind::ExternalModuleReference {
                return None;
            }
            let spec_node = mod_ref.as_external_module_reference().expression();
            let spec_text = string_literal_text(spec_node);
            Some(ProvenanceSpec {
                containing_sf: sf,
                specifier_node: Some(spec_node),
                specifier_text: spec_text,
                export_name: preferred_export,
                container_name: None,
                resolution_mode: PackageResolutionMode::Require,
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
            let spec_text = string_literal_text(spec_node);
            let mode = determine_resolution_mode(program, sf, Some(spec_node));
            Some(ProvenanceSpec {
                containing_sf: sf,
                specifier_node: Some(spec_node),
                specifier_text: spec_text,
                export_name,
                container_name: None,
                resolution_mode: mode,
            })
        }
        _ => None,
    }
}

fn spec_from_require_binding_decl(
    decl: P<Node>,
    preferred_export: Option<String>,
) -> Option<ProvenanceSpec> {
    let sf = get_source_file_of_node(decl)?;
    match decl.kind() {
        Kind::VariableDeclaration => {
            let init = unwrap_expr_wrappers(decl.initializer()?);
            if let Some(spec_node) = extract_require_specifier_node(init) {
                return Some(ProvenanceSpec {
                    containing_sf: sf,
                    specifier_node: Some(spec_node),
                    specifier_text: string_literal_text(spec_node),
                    export_name: preferred_export,
                    container_name: None,
                    resolution_mode: PackageResolutionMode::Require,
                });
            }
            if init.kind() == Kind::PropertyAccessExpression {
                let pa = init.as_property_access_expression();
                if let Some(spec_node) = extract_require_specifier_node(pa.expression()) {
                    return Some(ProvenanceSpec {
                        containing_sf: sf,
                        specifier_node: Some(spec_node),
                        specifier_text: string_literal_text(spec_node),
                        export_name: preferred_export.or_else(|| node_text(pa.name())),
                        container_name: None,
                        resolution_mode: PackageResolutionMode::Require,
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
                    return Some(ProvenanceSpec {
                        containing_sf: sf,
                        specifier_node: Some(spec_node),
                        specifier_text: string_literal_text(spec_node),
                        export_name: prop,
                        container_name: None,
                        resolution_mode: PackageResolutionMode::Require,
                    });
                }
            }
            None
        }
        _ => None,
    }
}

fn trace_symbol_to_package_provenance(
    program: &'static Program,
    checker: &mut CheckerHandle,
    start_sym: P<Symbol>,
    preferred_export: Option<String>,
) -> Option<ProvenanceSpec> {
    let mut current = start_sym;
    let mut export_override = preferred_export;
    let mut visited = Vec::new();

    for _ in 0..MAX_ALIAS_HOPS {
        if visited.contains(&current) {
            break;
        }
        visited.push(current);

        for &decl in current.declarations() {
            if let Some(prov) = spec_from_import_decl_node(program, decl, export_override.clone())
                && prov
                    .specifier_text
                    .as_deref()
                    .is_some_and(is_bare_package_specifier)
            {
                return Some(prov);
            }
            if let Some(prov) = spec_from_require_binding_decl(decl, export_override.clone())
                && prov
                    .specifier_text
                    .as_deref()
                    .is_some_and(is_bare_package_specifier)
            {
                return Some(prov);
            }
        }

        if let Some(vd) = current.value_declaration() {
            if let Some(prov) = spec_from_import_decl_node(program, vd, export_override.clone())
                && prov
                    .specifier_text
                    .as_deref()
                    .is_some_and(is_bare_package_specifier)
            {
                return Some(prov);
            }
            if let Some(prov) = spec_from_require_binding_decl(vd, export_override.clone())
                && prov
                    .specifier_text
                    .as_deref()
                    .is_some_and(is_bare_package_specifier)
            {
                return Some(prov);
            }
        }

        // Preserve exported name across intermediate local re-export aliases (`import { a as b }`).
        for &decl in current.declarations() {
            if decl.kind() == Kind::ImportSpecifier
                && let Some(orig) = decl
                    .as_import_specifier()
                    .property_name()
                    .and_then(node_text)
                    .or_else(|| node_text(decl.as_import_specifier().name()))
                && export_override.is_none()
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

    None
}

fn find_provenance_for_expr(
    program: &'static Program,
    checker: &mut CheckerHandle,
    expr: P<Node>,
) -> Option<ProvenanceSpec> {
    let unwrapped = unwrap_expr_wrappers(expr);
    if let Some(spec_node) = extract_require_specifier_node(unwrapped) {
        let sf = get_source_file_of_node(spec_node)?;
        return Some(ProvenanceSpec {
            containing_sf: sf,
            specifier_node: Some(spec_node),
            specifier_text: string_literal_text(spec_node),
            export_name: Some("default".to_string()),
            container_name: None,
            resolution_mode: PackageResolutionMode::Require,
        });
    }

    if unwrapped.kind() == Kind::PropertyAccessExpression {
        let pa = unwrapped.as_property_access_expression();
        let prop_name = node_text(pa.name());
        if let Some(spec_node) = extract_require_specifier_node(pa.expression()) {
            let sf = get_source_file_of_node(spec_node)?;
            return Some(ProvenanceSpec {
                containing_sf: sf,
                specifier_node: Some(spec_node),
                specifier_text: string_literal_text(spec_node),
                export_name: prop_name,
                container_name: None,
                resolution_mode: PackageResolutionMode::Require,
            });
        }
        if let Some(recv_sym) = checker.get_symbol_at_location_exported(pa.expression())
            && let Some(prov) =
                trace_symbol_to_package_provenance(program, checker, recv_sym, prop_name)
        {
            return Some(prov);
        }
    }

    if let Some(raw_sym) = checker.get_symbol_at_location_exported(unwrapped)
        && let Some(prov) = trace_symbol_to_package_provenance(program, checker, raw_sym, None)
    {
        return Some(prov);
    }

    None
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeclarationProvenance {
    pub package_name: Option<String>,
    pub export_name: Option<String>,
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

fn trace_symbol_deepest_package_provenance(
    program: &'static Program,
    checker: &mut CheckerHandle,
    start_sym: P<Symbol>,
    preferred_export: Option<String>,
) -> Option<ProvenanceSpec> {
    let mut current = start_sym;
    let mut export_override = preferred_export;
    let mut deepest: Option<ProvenanceSpec> = None;
    let mut visited = Vec::new();

    for _ in 0..MAX_ALIAS_HOPS {
        if visited.contains(&current) {
            break;
        }
        visited.push(current);

        for &decl in current.declarations() {
            if let Some(prov) = spec_from_import_decl_node(program, decl, export_override.clone())
                && prov
                    .specifier_text
                    .as_deref()
                    .is_some_and(is_bare_package_specifier)
            {
                deepest = Some(prov);
            }
            if let Some(prov) = spec_from_require_binding_decl(decl, export_override.clone())
                && prov
                    .specifier_text
                    .as_deref()
                    .is_some_and(is_bare_package_specifier)
            {
                deepest = Some(prov);
            }
        }

        if let Some(vd) = current.value_declaration() {
            if let Some(prov) = spec_from_import_decl_node(program, vd, export_override.clone())
                && prov
                    .specifier_text
                    .as_deref()
                    .is_some_and(is_bare_package_specifier)
            {
                deepest = Some(prov);
            }
            if let Some(prov) = spec_from_require_binding_decl(vd, export_override.clone())
                && prov
                    .specifier_text
                    .as_deref()
                    .is_some_and(is_bare_package_specifier)
            {
                deepest = Some(prov);
            }
        }

        for &decl in current.declarations() {
            if let Some(orig) = export_name_from_alias_decl(decl) {
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

    deepest
}

fn deepest_provenance_for_expr(
    program: &'static Program,
    checker: &mut CheckerHandle,
    expr: P<Node>,
) -> Option<ProvenanceSpec> {
    let unwrapped = unwrap_expr_wrappers(expr);
    if let Some(spec_node) = extract_require_specifier_node(unwrapped) {
        let sf = get_source_file_of_node(spec_node)?;
        return Some(ProvenanceSpec {
            containing_sf: sf,
            specifier_node: Some(spec_node),
            specifier_text: string_literal_text(spec_node),
            export_name: Some("default".to_string()),
            container_name: None,
            resolution_mode: PackageResolutionMode::Require,
        });
    }

    if unwrapped.kind() == Kind::PropertyAccessExpression {
        let pa = unwrapped.as_property_access_expression();
        let prop_name = node_text(pa.name());
        if let Some(spec_node) = extract_require_specifier_node(pa.expression()) {
            let sf = get_source_file_of_node(spec_node)?;
            return Some(ProvenanceSpec {
                containing_sf: sf,
                specifier_node: Some(spec_node),
                specifier_text: string_literal_text(spec_node),
                export_name: prop_name,
                container_name: None,
                resolution_mode: PackageResolutionMode::Require,
            });
        }
        if let Some(recv_sym) = checker.get_symbol_at_location_exported(pa.expression())
            && let Some(prov) =
                trace_symbol_deepest_package_provenance(program, checker, recv_sym, prop_name)
        {
            return Some(prov);
        }
    }

    if let Some(raw_sym) = checker.get_symbol_at_location_exported(unwrapped)
        && let Some(prov) = trace_symbol_deepest_package_provenance(program, checker, raw_sym, None)
    {
        return Some(prov);
    }

    None
}

pub fn resolve_declaration_provenance(
    program: &'static Program,
    checker: &mut CheckerHandle,
    node: P<Node>,
    declaration: P<Node>,
) -> DeclarationProvenance {
    let Some(decl_sf) = get_source_file_of_node(declaration) else {
        return DeclarationProvenance::default();
    };
    if program.is_source_file_default_library(decl_sf.path()) {
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

    let Some(prov) = deepest_provenance_for_expr(program, checker, expr) else {
        return DeclarationProvenance::default();
    };
    let Some(spec_text) = prov.specifier_text.as_deref() else {
        return DeclarationProvenance::default();
    };
    if !is_bare_package_specifier(spec_text) {
        return DeclarationProvenance::default();
    }

    let (spec_pkg_name, _) = split_package_specifier(spec_text);
    let decl_pkg_name = find_package_dir_and_info(program, decl_sf.file_name(), None)
        .and_then(|(_, info)| info)
        .and_then(|entry| entry.contents)
        .and_then(|contents| contents.get().fields.name.get_value().cloned());

    let resolved_module = prov.specifier_node.is_some_and(|spec_node| {
        program
            .get_resolved_module_from_module_specifier(prov.containing_sf, spec_node)
            .is_some_and(|resolved| resolved.is_resolved())
    });
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

fn find_package_dir_and_info(
    program: &'static Program,
    file_path: &str,
    expected_pkg_name: Option<&str>,
) -> Option<(String, Option<P<tsrs_module::packagejson::InfoCacheEntry>>)> {
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

enum ExportTargetEval {
    Matched(String),
    DeclarationOnly(String),
    Ambiguous(Vec<String>),
    NotMatched,
}

fn resolve_target_rel_path(pkg_dir: &str, raw_target: &str, star_subst: Option<&str>) -> String {
    let expanded = star_subst.map_or_else(
        || raw_target.to_string(),
        |sub| raw_target.replace('*', sub),
    );
    normalize_path(&tspath::combine_paths(pkg_dir, &[&expanded]))
}

#[expect(
    clippy::too_many_lines,
    reason = "Evaluates package.json exports targets across string, array, and conditional object forms."
)]
fn eval_exports_target(
    pkg_dir: &str,
    target: &ExportsOrImports,
    star_subst: Option<&str>,
    mode: PackageResolutionMode,
    conditions: &[String],
) -> ExportTargetEval {
    match target.type_() {
        JSONValueType::String => {
            let raw = target.as_string();
            if !raw.starts_with("./") {
                return ExportTargetEval::NotMatched;
            }
            let full = resolve_target_rel_path(pkg_dir, raw, star_subst);
            if tspath::is_declaration_file_name(&full) {
                return ExportTargetEval::DeclarationOnly(full);
            }
            ExportTargetEval::Matched(full)
        }
        JSONValueType::Array => {
            let mut existing_candidates = Vec::new();
            let mut decl_only = None;
            for elem in target.as_array() {
                match eval_exports_target(pkg_dir, elem, star_subst, mode, conditions) {
                    ExportTargetEval::Matched(path) => {
                        if Path::new(&path).is_file() && !existing_candidates.contains(&path) {
                            existing_candidates.push(path);
                        }
                    }
                    ExportTargetEval::DeclarationOnly(path) => {
                        if decl_only.is_none() {
                            decl_only = Some(path);
                        }
                    }
                    ExportTargetEval::Ambiguous(paths) => {
                        for p in paths {
                            if !existing_candidates.contains(&p) {
                                existing_candidates.push(p);
                            }
                        }
                    }
                    ExportTargetEval::NotMatched => {}
                }
            }
            if existing_candidates.len() > 1 {
                return ExportTargetEval::Ambiguous(existing_candidates);
            }
            if let Some(only) = existing_candidates.into_iter().next() {
                return ExportTargetEval::Matched(only);
            }
            if let Some(decl) = decl_only {
                return ExportTargetEval::DeclarationOnly(decl);
            }
            ExportTargetEval::NotMatched
        }
        JSONValueType::Object => {
            let obj = target.as_object();
            if mode == PackageResolutionMode::Unknown
                && obj.get("import").is_some()
                && obj.get("require").is_some()
            {
                let mut import_conds = conditions.to_vec();
                import_conds.push("import".to_string());
                let mut require_conds = conditions.to_vec();
                require_conds.push("require".to_string());
                let imp_eval = eval_exports_target(
                    pkg_dir,
                    &obj["import"],
                    star_subst,
                    PackageResolutionMode::Import,
                    &import_conds,
                );
                let req_eval = eval_exports_target(
                    pkg_dir,
                    &obj["require"],
                    star_subst,
                    PackageResolutionMode::Require,
                    &require_conds,
                );
                if let (ExportTargetEval::Matched(a), ExportTargetEval::Matched(b)) =
                    (imp_eval, req_eval)
                    && a != b
                    && Path::new(&a).is_file()
                    && Path::new(&b).is_file()
                {
                    return ExportTargetEval::Ambiguous(vec![a, b]);
                }
            }

            let mut decl_fallback = None;
            for (cond, sub_target) in obj {
                // Skip `types` and versioned `types@...` because we are resolving runtime code.
                if cond == "types" || cond.starts_with("types@") {
                    continue;
                }
                let matches = cond == "default" || conditions.iter().any(|c| c == cond);
                if !matches {
                    continue;
                }
                match eval_exports_target(pkg_dir, sub_target, star_subst, mode, conditions) {
                    ExportTargetEval::Matched(path) => {
                        return ExportTargetEval::Matched(path);
                    }
                    ExportTargetEval::Ambiguous(paths) => {
                        return ExportTargetEval::Ambiguous(paths);
                    }
                    ExportTargetEval::DeclarationOnly(path) => {
                        if decl_fallback.is_none() {
                            decl_fallback = Some(path);
                        }
                    }
                    ExportTargetEval::NotMatched => {}
                }
            }
            decl_fallback.map_or(
                ExportTargetEval::NotMatched,
                ExportTargetEval::DeclarationOnly,
            )
        }
        _ => ExportTargetEval::NotMatched,
    }
}

fn match_exports_subpath<'a>(
    exports: &'a ExportsOrImports,
    subpath: &str,
) -> Option<(&'a ExportsOrImports, Option<String>)> {
    match exports.type_() {
        JSONValueType::String | JSONValueType::Array if subpath == "." => Some((exports, None)),
        JSONValueType::Object => {
            if exports.is_conditions() {
                return (subpath == ".").then_some((exports, None));
            }
            let obj = exports.as_object();
            if let Some(exact) = obj.get(subpath) {
                return Some((exact, None));
            }
            // Match pattern exports such as `"./*"` or `"./sub/*"`.
            let mut best: Option<(&str, &ExportsOrImports, String)> = None;
            for (key, val) in obj {
                if let Some(star_pos) = key.find('*') {
                    let prefix = &key[..star_pos];
                    let suffix = &key[star_pos + 1..];
                    if subpath.starts_with(prefix)
                        && subpath.ends_with(suffix)
                        && subpath.len() >= prefix.len() + suffix.len()
                    {
                        let captured = &subpath[prefix.len()..subpath.len() - suffix.len()];
                        let replace = best
                            .as_ref()
                            .is_none_or(|(best_key, _, _)| key.len() > best_key.len());
                        if replace {
                            best = Some((key.as_str(), val, captured.to_string()));
                        }
                    }
                }
            }
            best.map(|(_, val, cap)| (val, Some(cap)))
        }
        _ => None,
    }
}

fn read_package_json_module_field(pkg_dir: &str) -> Option<String> {
    let raw_path = tspath::combine_paths(pkg_dir, &["package.json"]);
    let content = fs::read_to_string(&raw_path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;
    val.get("module")?.as_str().map(str::to_string)
}

fn probe_sibling_implementations(decl_file: &str, mode: PackageResolutionMode) -> Vec<String> {
    let base = if let Some(stem) = decl_file.strip_suffix(".d.ts") {
        stem
    } else if let Some(stem) = decl_file.strip_suffix(".d.mts") {
        stem
    } else if let Some(stem) = decl_file.strip_suffix(".d.cts") {
        stem
    } else {
        return Vec::new();
    };

    let candidate_exts: &[&str] = if decl_file.ends_with(".d.mts") {
        &[".mjs", ".mts"]
    } else if decl_file.ends_with(".d.cts") {
        &[".cjs", ".cts"]
    } else {
        match mode {
            PackageResolutionMode::Import => &[".mjs", ".js", ".ts", ".mts"],
            PackageResolutionMode::Require => &[".cjs", ".js", ".ts", ".cts"],
            PackageResolutionMode::Unknown => &[".js", ".mjs", ".cjs", ".ts"],
        }
    };

    let mut found = Vec::new();
    for ext in candidate_exts {
        let candidate = format!("{base}{ext}");
        if Path::new(&candidate).is_file() {
            found.push(candidate);
        }
    }
    found
}

fn resolve_package_file_from_manifest(
    program: &'static Program,
    pkg_dir: &str,
    pkg_info: Option<P<tsrs_module::packagejson::InfoCacheEntry>>,
    subpath: &str,
    decl_file: Option<&str>,
    mode: PackageResolutionMode,
    conditions: &[String],
) -> ManifestFileOutcome {
    if let Some(entry) = pkg_info
        && let Some(contents) = entry.contents
    {
        let fields = &contents.get().fields;
        if fields.exports.type_() != JSONValueType::NotPresent {
            if let Some((target, star_subst)) = match_exports_subpath(&fields.exports, subpath) {
                match eval_exports_target(pkg_dir, target, star_subst.as_deref(), mode, conditions)
                {
                    ExportTargetEval::Matched(path) => {
                        if Path::new(&path).is_file() {
                            return ManifestFileOutcome::Unique(path);
                        }
                        return ManifestFileOutcome::MissingOnDisk(path);
                    }
                    ExportTargetEval::Ambiguous(paths) => {
                        return ManifestFileOutcome::Ambiguous(paths);
                    }
                    ExportTargetEval::DeclarationOnly(_) => {
                        if let Some(df) = decl_file {
                            let siblings = probe_sibling_implementations(df, mode);
                            if siblings.is_empty() {
                                return ManifestFileOutcome::DeclarationOnly;
                            }
                            if siblings.len() == 1 {
                                return ManifestFileOutcome::Unique(siblings[0].clone());
                            }
                            return ManifestFileOutcome::Ambiguous(siblings);
                        }
                        return ManifestFileOutcome::DeclarationOnly;
                    }
                    ExportTargetEval::NotMatched => {}
                }
            } else {
                return ManifestFileOutcome::DeclarationOnly;
            }
        }

        if subpath == "." {
            let main_rel = fields.main.get_value().cloned();
            let module_rel = read_package_json_module_field(pkg_dir);

            let main_abs = main_rel
                .as_deref()
                .map(|m| normalize_path(&tspath::combine_paths(pkg_dir, &[m])))
                .filter(|p| !tspath::is_declaration_file_name(p) && Path::new(p).is_file());
            let module_abs = module_rel
                .as_deref()
                .map(|m| normalize_path(&tspath::combine_paths(pkg_dir, &[m])))
                .filter(|p| !tspath::is_declaration_file_name(p) && Path::new(p).is_file());

            match mode {
                PackageResolutionMode::Import => {
                    if let Some(m) = module_abs {
                        return ManifestFileOutcome::Unique(m);
                    }
                    if let Some(m) = main_abs {
                        return ManifestFileOutcome::Unique(m);
                    }
                }
                PackageResolutionMode::Require => {
                    if let Some(m) = main_abs {
                        return ManifestFileOutcome::Unique(m);
                    }
                }
                PackageResolutionMode::Unknown => match (module_abs, main_abs) {
                    (Some(a), Some(b)) if a != b => {
                        return ManifestFileOutcome::Ambiguous(vec![a, b]);
                    }
                    (Some(a), _) | (_, Some(a)) => return ManifestFileOutcome::Unique(a),
                    (None, None) => {}
                },
            }
        }
    }

    let _ = program;
    if let Some(df) = decl_file {
        let siblings = probe_sibling_implementations(df, mode);
        if siblings.len() == 1 {
            return ManifestFileOutcome::Unique(siblings[0].clone());
        }
        if siblings.len() > 1 {
            // When `.d.ts` has both `.js` and `.mjs`/`.cjs` without manifest guidance, check ambiguity.
            let found_esm_sibling = siblings.iter().any(|p| has_ext(p, &["mjs"]));
            let found_cjs_sibling = siblings.iter().any(|p| has_ext(p, &["cjs"]));
            let found_plain_js_sibling = siblings.iter().any(|p| has_ext(p, &["js"]));
            if mode == PackageResolutionMode::Import && found_esm_sibling && !found_plain_js_sibling
            {
                if let Some(p) = siblings.into_iter().find(|p| has_ext(p, &["mjs"])) {
                    return ManifestFileOutcome::Unique(p);
                }
            } else if mode == PackageResolutionMode::Require
                && found_cjs_sibling
                && !found_plain_js_sibling
            {
                if let Some(p) = siblings.into_iter().find(|p| has_ext(p, &["cjs"])) {
                    return ManifestFileOutcome::Unique(p);
                }
            } else {
                return ManifestFileOutcome::Ambiguous(siblings);
            }
        }
    }

    ManifestFileOutcome::DeclarationOnly
}

enum ManifestFileOutcome {
    Unique(String),
    MissingOnDisk(String),
    Ambiguous(Vec<String>),
    DeclarationOnly,
}

fn has_modifier(node: P<Node>, kind: Kind) -> bool {
    node.modifiers()
        .is_some_and(|mods| mods.nodes().iter().any(|&m| m.kind() == kind))
}

fn find_local_binding_callable(
    sf: P<SourceFile>,
    local_name: &str,
    seen: &mut FxHashSet<String>,
) -> Option<(P<Node>, P<Node>)> {
    if !seen.insert(local_name.to_string()) {
        return None;
    }
    for &stmt in sf.statements.nodes() {
        if stmt.kind() == Kind::FunctionDeclaration
            && stmt.name().and_then(node_text).as_deref() == Some(local_name)
            && stmt.body().is_some()
        {
            return Some((stmt, stmt));
        }
        if stmt.kind() == Kind::VariableStatement {
            let decl_list = stmt.as_variable_statement().declaration_list();
            for &decl in decl_list
                .as_variable_declaration_list()
                .declarations()
                .nodes()
            {
                if decl.name().and_then(node_text).as_deref() == Some(local_name)
                    && let Some(init) = decl.initializer()
                {
                    let inner = unwrap_expr_wrappers(init);
                    if let Some(fn_node) = extract_callable_from_decl(decl)
                        && fn_node.body().is_some()
                    {
                        return Some((decl, fn_node));
                    }
                    if inner.kind() == Kind::Identifier
                        && let Some(target_name) = node_text(inner)
                        && let Some(found) = find_local_binding_callable(sf, &target_name, seen)
                    {
                        return Some(found);
                    }
                }
            }
        }
    }
    None
}

fn find_class_method_callable(
    sf: P<SourceFile>,
    class_name: &str,
    method_name: &str,
) -> Option<(P<Node>, P<Node>)> {
    for &stmt in sf.statements.nodes() {
        if stmt.kind() == Kind::ClassDeclaration
            && stmt.name().and_then(node_text).as_deref() == Some(class_name)
        {
            for &member in stmt.as_class_declaration().members().nodes() {
                if member.kind() == Kind::MethodDeclaration
                    && member.name().and_then(node_text).as_deref() == Some(method_name)
                    && member.body().is_some()
                {
                    return Some((member, member));
                }
            }
        }
    }
    None
}

fn resolve_relative_impl_path(from_file: &str, spec: &str) -> Option<String> {
    if !spec.starts_with('.') {
        return None;
    }
    let base_dir = tspath::get_directory_path(from_file);
    let raw = normalize_path(&tspath::combine_paths(&base_dir, &[spec]));
    if Path::new(&raw).is_file() && !tspath::is_declaration_file_name(&raw) {
        return Some(raw);
    }
    let stem = raw
        .strip_suffix(".js")
        .or_else(|| raw.strip_suffix(".mjs"))
        .or_else(|| raw.strip_suffix(".cjs"))
        .unwrap_or(&raw);
    for ext in &[".ts", ".tsx", ".mjs", ".cjs", ".js", ".jsx"] {
        let candidate = format!("{stem}{ext}");
        if Path::new(&candidate).is_file() {
            return Some(candidate);
        }
    }
    for ext in &["/index.ts", "/index.mjs", "/index.cjs", "/index.js"] {
        let candidate = format!("{raw}{ext}");
        if Path::new(&candidate).is_file() {
            return Some(candidate);
        }
    }
    None
}

#[expect(
    clippy::too_many_lines,
    reason = "Matches ESM/CJS exports, default exports, and relative re-exports in one traversal."
)]
fn find_exported_callable_in_sf(
    cache: &PackageImplCache,
    cwd: &str,
    sf: P<SourceFile>,
    export_name: &str,
    container_name: Option<&str>,
    depth: usize,
) -> Option<(P<Node>, P<Node>)> {
    if depth >= MAX_REEXPORT_DEPTH {
        return None;
    }

    if let Some(class_name) = container_name
        && let Some(found) = find_class_method_callable(sf, class_name, export_name)
    {
        return Some(found);
    }

    for &stmt in sf.statements.nodes() {
        match stmt.kind() {
            Kind::FunctionDeclaration => {
                let is_export = has_modifier(stmt, Kind::ExportKeyword);
                let is_default = has_modifier(stmt, Kind::DefaultKeyword);
                let name_matches = if export_name == "default" {
                    is_export && is_default
                } else {
                    is_export && stmt.name().and_then(node_text).as_deref() == Some(export_name)
                };
                if name_matches && stmt.body().is_some() {
                    return Some((stmt, stmt));
                }
            }
            Kind::VariableStatement if has_modifier(stmt, Kind::ExportKeyword) => {
                let decl_list = stmt.as_variable_statement().declaration_list();
                for &decl in decl_list
                    .as_variable_declaration_list()
                    .declarations()
                    .nodes()
                {
                    if decl.name().and_then(node_text).as_deref() == Some(export_name)
                        && let Some(fn_node) = extract_callable_from_decl(decl)
                        && fn_node.body().is_some()
                    {
                        return Some((decl, fn_node));
                    }
                }
            }
            Kind::ExportAssignment => {
                let ea = stmt.as_export_assignment();
                if export_name == "default" || ea.is_export_equals() {
                    let inner = unwrap_expr_wrappers(ea.expression());
                    if is_function_like_declaration(Some(inner)) && inner.body().is_some() {
                        return Some((stmt, inner));
                    }
                    if inner.kind() == Kind::Identifier
                        && let Some(target_name) = node_text(inner)
                    {
                        let mut seen = FxHashSet::default();
                        if let Some(found) =
                            find_local_binding_callable(sf, &target_name, &mut seen)
                        {
                            return Some(found);
                        }
                    }
                    if inner.kind() == Kind::ObjectLiteralExpression {
                        for &prop in inner.as_object_literal_expression().properties().nodes() {
                            if let Some(found) = match_cjs_object_export_prop(
                                cache,
                                cwd,
                                sf,
                                prop,
                                export_name,
                                depth,
                            ) {
                                return Some(found);
                            }
                        }
                    }
                }
            }
            Kind::ExportDeclaration => {
                let ed = stmt.as_export_declaration();
                if let Some(clause) = ed.export_clause
                    && clause.kind() == Kind::NamedExports
                {
                    for &spec in clause.as_named_exports().elements().nodes() {
                        if spec.kind() != Kind::ExportSpecifier {
                            continue;
                        }
                        let es = spec.as_export_specifier();
                        let exported = node_text(es.name);
                        if exported.as_deref() != Some(export_name) {
                            continue;
                        }
                        let local_or_remote = es
                            .property_name
                            .and_then(node_text)
                            .unwrap_or_else(|| export_name.to_string());
                        if let Some(mod_spec) = ed.module_specifier
                            && let Some(rel_spec) = string_literal_text(mod_spec)
                            && let Some(target_file) =
                                resolve_relative_impl_path(sf.file_name(), &rel_spec)
                            && let Some(target_sf) =
                                cache.load_external_source_file(cwd, &target_file)
                            && let Some(found) = find_exported_callable_in_sf(
                                cache,
                                cwd,
                                target_sf,
                                &local_or_remote,
                                container_name,
                                depth + 1,
                            )
                        {
                            return Some(found);
                        }
                        let mut seen = FxHashSet::default();
                        if let Some(found) =
                            find_local_binding_callable(sf, &local_or_remote, &mut seen)
                        {
                            return Some(found);
                        }
                    }
                } else if ed.export_clause.is_none()
                    && let Some(mod_spec) = ed.module_specifier
                    && let Some(rel_spec) = string_literal_text(mod_spec)
                    && let Some(target_file) = resolve_relative_impl_path(sf.file_name(), &rel_spec)
                    && let Some(target_sf) = cache.load_external_source_file(cwd, &target_file)
                    && let Some(found) = find_exported_callable_in_sf(
                        cache,
                        cwd,
                        target_sf,
                        export_name,
                        container_name,
                        depth + 1,
                    )
                {
                    return Some(found);
                }
            }
            Kind::ExpressionStatement => {
                let expr = unwrap_expr_wrappers(stmt.as_expression_statement().expression());
                if expr.kind() == Kind::BinaryExpression {
                    let bin = expr.as_binary_expression();
                    if bin.operator_token().kind() == Kind::EqualsToken
                        && let Some(found) = match_cjs_assignment_export(
                            cache,
                            cwd,
                            sf,
                            bin.left(),
                            bin.right(),
                            export_name,
                            container_name,
                            depth,
                        )
                    {
                        return Some(found);
                    }
                } else if let Some(found) = match_define_property_export(sf, expr, export_name) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }

    None
}

fn match_cjs_object_export_prop(
    cache: &PackageImplCache,
    cwd: &str,
    sf: P<SourceFile>,
    prop: P<Node>,
    export_name: &str,
    _depth: usize,
) -> Option<(P<Node>, P<Node>)> {
    match prop.kind() {
        Kind::ShorthandPropertyAssignment => {
            let name = prop.name().and_then(node_text)?;
            if name == export_name {
                let mut seen = FxHashSet::default();
                return find_local_binding_callable(sf, &name, &mut seen);
            }
            None
        }
        Kind::PropertyAssignment => {
            let pa = prop.as_property_assignment();
            let key = prop_key_text(pa.name())?;
            if key != export_name {
                return None;
            }
            let init = unwrap_expr_wrappers(pa.initializer());
            if is_function_like_declaration(Some(init)) && init.body().is_some() {
                return Some((prop, init));
            }
            if init.kind() == Kind::Identifier
                && let Some(target_name) = node_text(init)
            {
                let mut seen = FxHashSet::default();
                return find_local_binding_callable(sf, &target_name, &mut seen);
            }
            let _ = (cache, cwd);
            None
        }
        Kind::MethodDeclaration => {
            if prop.name().and_then(node_text).as_deref() == Some(export_name)
                && prop.body().is_some()
            {
                return Some((prop, prop));
            }
            None
        }
        _ => None,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Matches CommonJS exports.x / module.exports = ... assignment patterns."
)]
fn match_cjs_assignment_export(
    cache: &PackageImplCache,
    cwd: &str,
    sf: P<SourceFile>,
    lhs_raw: P<Node>,
    rhs_raw: P<Node>,
    export_name: &str,
    container_name: Option<&str>,
    depth: usize,
) -> Option<(P<Node>, P<Node>)> {
    let lhs = unwrap_expr_wrappers(lhs_raw);
    let rhs = unwrap_expr_wrappers(rhs_raw);
    if lhs.kind() != Kind::PropertyAccessExpression {
        return None;
    }
    let pa = lhs.as_property_access_expression();
    let member = node_text(pa.name())?;
    let base = unwrap_expr_wrappers(pa.expression());

    let is_exports_obj = (base.kind() == Kind::Identifier
        && node_text(base).as_deref() == Some("exports"))
        || (base.kind() == Kind::PropertyAccessExpression
            && node_text(base.as_property_access_expression().expression()).as_deref()
                == Some("module")
            && node_text(base.as_property_access_expression().name()).as_deref()
                == Some("exports"));

    if is_exports_obj && member == export_name {
        if is_function_like_declaration(Some(rhs)) && rhs.body().is_some() {
            return Some((rhs, rhs));
        }
        if rhs.kind() == Kind::Identifier
            && let Some(target_name) = node_text(rhs)
        {
            let mut seen = FxHashSet::default();
            return find_local_binding_callable(sf, &target_name, &mut seen);
        }
        if rhs.kind() == Kind::PropertyAccessExpression {
            let rhs_pa = rhs.as_property_access_expression();
            if let Some(spec_node) = extract_require_specifier_node(rhs_pa.expression())
                && let Some(rel_spec) = string_literal_text(spec_node)
                && let Some(target_file) = resolve_relative_impl_path(sf.file_name(), &rel_spec)
                && let Some(target_sf) = cache.load_external_source_file(cwd, &target_file)
                && let Some(remote_name) = node_text(rhs_pa.name())
            {
                return find_exported_callable_in_sf(
                    cache,
                    cwd,
                    target_sf,
                    &remote_name,
                    container_name,
                    depth + 1,
                );
            }
        }
    }

    let is_module_exports = base.kind() == Kind::Identifier
        && node_text(base).as_deref() == Some("module")
        && member == "exports";

    if is_module_exports {
        if rhs.kind() == Kind::ObjectLiteralExpression {
            for &prop in rhs.as_object_literal_expression().properties().nodes() {
                if let Some(found) =
                    match_cjs_object_export_prop(cache, cwd, sf, prop, export_name, depth)
                {
                    return Some(found);
                }
            }
        }
        if let Some(spec_node) = extract_require_specifier_node(rhs)
            && let Some(rel_spec) = string_literal_text(spec_node)
            && let Some(target_file) = resolve_relative_impl_path(sf.file_name(), &rel_spec)
            && let Some(target_sf) = cache.load_external_source_file(cwd, &target_file)
        {
            return find_exported_callable_in_sf(
                cache,
                cwd,
                target_sf,
                export_name,
                container_name,
                depth + 1,
            );
        }
        if export_name == "default" || export_name == "*" {
            if is_function_like_declaration(Some(rhs)) && rhs.body().is_some() {
                return Some((rhs, rhs));
            }
            if rhs.kind() == Kind::Identifier
                && let Some(target_name) = node_text(rhs)
            {
                let mut seen = FxHashSet::default();
                return find_local_binding_callable(sf, &target_name, &mut seen);
            }
        }
    }

    None
}

fn match_define_property_export(
    sf: P<SourceFile>,
    expr: P<Node>,
    export_name: &str,
) -> Option<(P<Node>, P<Node>)> {
    if expr.kind() != Kind::CallExpression {
        return None;
    }
    let callee = expr.expression()?;
    if callee.kind() != Kind::PropertyAccessExpression {
        return None;
    }
    let pa = callee.as_property_access_expression();
    if node_text(pa.expression()).as_deref() != Some("Object")
        || node_text(pa.name()).as_deref() != Some("defineProperty")
    {
        return None;
    }
    let args = expr.arguments();
    if args.len() < 3 {
        return None;
    }
    let target_obj = unwrap_expr_wrappers(args[0]);
    if node_text(target_obj).as_deref() != Some("exports") {
        return None;
    }
    if string_literal_text(args[1]).as_deref() != Some(export_name) {
        return None;
    }
    let desc = unwrap_expr_wrappers(args[2]);
    if desc.kind() != Kind::ObjectLiteralExpression {
        return None;
    }
    for &prop in desc.as_object_literal_expression().properties().nodes() {
        if prop.kind() != Kind::PropertyAssignment {
            continue;
        }
        let prop_pa = prop.as_property_assignment();
        let key = prop_key_text(prop_pa.name())?;
        let val = unwrap_expr_wrappers(prop_pa.initializer());
        if key == "value" {
            if is_function_like_declaration(Some(val)) && val.body().is_some() {
                return Some((prop, val));
            }
            if val.kind() == Kind::Identifier
                && let Some(name) = node_text(val)
            {
                let mut seen = FxHashSet::default();
                return find_local_binding_callable(sf, &name, &mut seen);
            }
        } else if key == "get" && is_function_like_declaration(Some(val)) && val.body().is_some() {
            // Handle tsc `Object.defineProperty(exports, "fn", { get: function () { return mod.fn; } })`.
            if let Some(body) = val.body()
                && body.kind() == Kind::Block
                && body.statements().len() == 1
                && body.statements()[0].kind() == Kind::ReturnStatement
                && let Some(ret_expr) = body.statements()[0].as_return_statement().expression()
            {
                let inner = unwrap_expr_wrappers(ret_expr);
                if inner.kind() == Kind::Identifier
                    && let Some(name) = node_text(inner)
                {
                    let mut seen = FxHashSet::default();
                    return find_local_binding_callable(sf, &name, &mut seen);
                }
            }
        }
    }
    None
}

fn enclosing_container_name(decl: P<Node>) -> Option<String> {
    let mut cur = decl.parent();
    while let Some(p) = cur {
        if matches!(
            p.kind(),
            Kind::ClassDeclaration
                | Kind::ClassExpression
                | Kind::InterfaceDeclaration
                | Kind::ModuleDeclaration
        ) && let Some(name) = p.name().and_then(node_text)
        {
            return Some(name);
        }
        cur = p.parent();
    }
    None
}

fn find_import_for_decl_sf(
    program: &'static Program,
    caller_sf: P<SourceFile>,
    decl_sf_name: &str,
) -> Option<(P<Node>, PackageResolutionMode)> {
    for &imp in caller_sf.imports() {
        if let Some(resolved) = program.get_resolved_module_from_module_specifier(caller_sf, imp)
            && resolved.resolved_file_name == decl_sf_name
        {
            let mode = determine_resolution_mode(program, caller_sf, Some(imp));
            return Some((imp, mode));
        }
    }
    None
}

struct TargetPayloadInput<'a> {
    cwd: &'a str,
    file_abs: &'a str,
    node_for_loc: Option<P<Node>>,
    export_name: Option<String>,
    is_external: bool,
    package_name: Option<String>,
    package_version: Option<String>,
    subpath: Option<String>,
    mode: PackageResolutionMode,
    conditions: Vec<String>,
    declaration_file: Option<String>,
}

fn build_target_payload(input: TargetPayloadInput<'_>) -> ResolvedImplTargetPayload {
    let rel_file = relative_to_cwd(input.cwd, input.file_abs);
    let location = input
        .node_for_loc
        .and_then(|n| node_location_rel(input.cwd, n));
    ResolvedImplTargetPayload {
        file: rel_file,
        location,
        export_name: input.export_name,
        is_external: input.is_external,
        package_name: input.package_name,
        package_version: input.package_version,
        subpath: input.subpath,
        resolution_mode: input.mode.as_str(),
        conditions: input.conditions,
        declaration_file: input.declaration_file,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Coordinates module provenance, package.json resolution, ambiguity detection, and AST export binding."
)]
fn resolve_from_provenance_and_decl(
    cache: &PackageImplCache,
    cwd: &str,
    program: &'static Program,
    prov: Option<ProvenanceSpec>,
    decl_node_opt: Option<P<Node>>,
    fn_node_opt: Option<P<Node>>,
) -> PackageImplLookup {
    // 1. If a target already has an implementation body in `Program`, return `SourceBody`.
    if let Some(fn_node) = fn_node_opt
        && fn_node.body().is_some()
        && let Some(sf) = get_source_file_of_node(fn_node)
        && !sf.is_declaration_file()
    {
        let is_ext = !is_project_source_file(program, sf);
        let (pkg_name, pkg_version, subpath, mode, conditions) = prov.as_ref().map_or_else(
            || (None, None, None, PackageResolutionMode::Unknown, Vec::new()),
            |p| {
                let spec_str = p.specifier_text.as_deref().unwrap_or("");
                let (pkg_n, sub) = if is_bare_package_specifier(spec_str) {
                    let (pn, sb) = split_package_specifier(spec_str);
                    (Some(pn), Some(sb))
                } else {
                    (None, None)
                };
                let ver = p
                    .specifier_node
                    .and_then(|sn| {
                        program.get_resolved_module_from_module_specifier(p.containing_sf, sn)
                    })
                    .map(|rm| rm.package_id.version)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string);
                let conds = collect_active_conditions(program, p.resolution_mode);
                (pkg_n, ver, sub, p.resolution_mode, conds)
            },
        );
        let info = build_target_payload(TargetPayloadInput {
            cwd,
            file_abs: sf.file_name(),
            node_for_loc: Some(fn_node),
            export_name: prov.as_ref().and_then(|p| p.export_name.clone()),
            is_external: is_ext,
            package_name: pkg_name,
            package_version: pkg_version,
            subpath,
            mode,
            conditions,
            declaration_file: None,
        });
        return PackageImplLookup::SourceBody { fn_node, info };
    }

    // 2. Check if provenance points to a missing/uninstalled package or unresolved JS module.
    if let Some(ref p) = prov
        && let Some(spec_node) = p.specifier_node
        && let Some(ref spec_text) = p.specifier_text
        && is_bare_package_specifier(spec_text)
    {
        let resolved_opt =
            program.get_resolved_module_from_module_specifier(p.containing_sf, spec_node);
        let is_resolved = resolved_opt.is_some_and(|r| r.is_resolved());
        if !is_resolved {
            return PackageImplLookup::MissingPackage {
                message: format!(
                    "Package specifier '{spec_text}' is not installed or could not be resolved from '{}'.",
                    relative_to_cwd(cwd, p.containing_sf.file_name())
                ),
            };
        }

        if let Some(resolved) = resolved_opt
            && !tspath::is_declaration_file_name(resolved.resolved_file_name)
            && Path::new(resolved.resolved_file_name).is_file()
        {
            // Direct pure-JS/TS dependency resolved by tsrs without entering `Program` (e.g., allowJs: false).
            let (pkg_name_guess, subpath) = split_package_specifier(spec_text);
            let pkg_name = if resolved.package_id.name.is_empty() {
                Some(pkg_name_guess)
            } else {
                Some(resolved.package_id.name.to_string())
            };
            let pkg_version = (!resolved.package_id.version.is_empty())
                .then(|| resolved.package_id.version.to_string());
            let export_name = p
                .export_name
                .clone()
                .or_else(|| decl_node_opt.and_then(|d| d.name().and_then(node_text)));
            let container_name = p
                .container_name
                .clone()
                .or_else(|| decl_node_opt.and_then(enclosing_container_name));
            let conditions = collect_active_conditions(program, p.resolution_mode);
            if let Some(sf) = cache.load_external_source_file(cwd, resolved.resolved_file_name)
                && let Some(exp) = export_name.as_deref()
                && let Some((_impl_decl, impl_fn)) =
                    find_exported_callable_in_sf(cache, cwd, sf, exp, container_name.as_deref(), 0)
            {
                let info = build_target_payload(TargetPayloadInput {
                    cwd,
                    file_abs: resolved.resolved_file_name,
                    node_for_loc: Some(impl_fn),
                    export_name,
                    is_external: true,
                    package_name: pkg_name,
                    package_version: pkg_version,
                    subpath: Some(subpath),
                    mode: p.resolution_mode,
                    conditions,
                    declaration_file: None,
                });
                return PackageImplLookup::ResolvedExternal {
                    fn_node: impl_fn,
                    info,
                };
            }
        }
    }

    // 3. Target is a declaration file (`.d.ts` / `.d.mts` / `.d.cts`) without implementation body.
    let Some(decl_node) = decl_node_opt else {
        return PackageImplLookup::Unimplemented {
            message: "Target has no static declaration or package provenance.".to_string(),
        };
    };
    let Some(decl_sf) = get_source_file_of_node(decl_node) else {
        return PackageImplLookup::Unimplemented {
            message: "Target declaration has no containing source file.".to_string(),
        };
    };
    if !decl_sf.is_declaration_file() {
        return PackageImplLookup::Unimplemented {
            message: "Call target has no static function implementation body.".to_string(),
        };
    }
    if program.is_source_file_default_library(decl_sf.path()) {
        return PackageImplLookup::Unimplemented {
            message: "Default TypeScript library declarations have no local JS/TS implementation."
                .to_string(),
        };
    }

    let decl_file_abs = decl_sf.file_name();
    let decl_file_rel = relative_to_cwd(cwd, decl_file_abs);

    // Extract package name, subpath, mode, export name from provenance or fallback to `decl_sf`.
    let (pkg_name_hint, subpath, mode, export_name, container_name) = if let Some(p) = prov {
        let spec_str = p.specifier_text.as_deref().unwrap_or("");
        let (pn, sub) = if is_bare_package_specifier(spec_str) {
            let (n, s) = split_package_specifier(spec_str);
            (Some(n), s)
        } else {
            (None, ".".to_string())
        };
        let exp = p
            .export_name
            .or_else(|| decl_node.name().and_then(node_text));
        let cont = p
            .container_name
            .or_else(|| enclosing_container_name(decl_node));
        (pn, sub, p.resolution_mode, exp, cont)
    } else {
        let exp = decl_node.name().and_then(node_text);
        let cont = enclosing_container_name(decl_node);
        (
            None,
            ".".to_string(),
            PackageResolutionMode::Unknown,
            exp,
            cont,
        )
    };

    let Some((pkg_dir, pkg_info)) =
        find_package_dir_and_info(program, decl_file_abs, pkg_name_hint.as_deref())
    else {
        return PackageImplLookup::DeclarationOnly {
            message: format!(
                "Declaration '{}' in '{}' has no associated package.json or local implementation source.",
                export_name.as_deref().unwrap_or("<symbol>"),
                decl_file_rel
            ),
        };
    };

    let (pkg_name, pkg_version) = match pkg_info.and_then(|entry| entry.contents) {
        Some(contents) => {
            let f = &contents.get().fields;
            (
                f.name.get_value().cloned().or(pkg_name_hint),
                f.version.get_value().cloned(),
            )
        }
        None => (pkg_name_hint, None),
    };

    let conditions = collect_active_conditions(program, mode);
    let outcome = resolve_package_file_from_manifest(
        program,
        &pkg_dir,
        pkg_info,
        &subpath,
        Some(decl_file_abs),
        mode,
        &conditions,
    );

    match outcome {
        ManifestFileOutcome::Unique(impl_abs) => {
            let Some(impl_sf) = cache.load_external_source_file(cwd, &impl_abs) else {
                return PackageImplLookup::DeclarationOnly {
                    message: format!(
                        "Resolved implementation '{}' for '{}' could not be parsed as JS/TS.",
                        relative_to_cwd(cwd, &impl_abs),
                        export_name.as_deref().unwrap_or("<symbol>")
                    ),
                };
            };
            let Some(exp) = export_name.as_deref() else {
                return PackageImplLookup::DeclarationOnly {
                    message: format!(
                        "Cannot match anonymous declaration in '{}' inside '{}'.",
                        decl_file_rel,
                        relative_to_cwd(cwd, &impl_abs)
                    ),
                };
            };
            if let Some((_impl_decl, impl_fn)) =
                find_exported_callable_in_sf(cache, cwd, impl_sf, exp, container_name.as_deref(), 0)
            {
                let info = build_target_payload(TargetPayloadInput {
                    cwd,
                    file_abs: impl_sf.file_name(),
                    node_for_loc: Some(impl_fn),
                    export_name: Some(exp.to_string()),
                    is_external: true,
                    package_name: pkg_name,
                    package_version: pkg_version,
                    subpath: Some(subpath),
                    mode,
                    conditions,
                    declaration_file: Some(decl_file_rel),
                });
                PackageImplLookup::ResolvedExternal {
                    fn_node: impl_fn,
                    info,
                }
            } else {
                PackageImplLookup::DeclarationOnly {
                    message: format!(
                        "Implementation file '{}' does not expose callable export '{exp}' declared in '{}'.",
                        relative_to_cwd(cwd, &impl_abs),
                        decl_file_rel
                    ),
                }
            }
        }
        ManifestFileOutcome::MissingOnDisk(expected_abs) => PackageImplLookup::DeclarationOnly {
            message: format!(
                "Package '{}' ({}) declares '{}' only in '{}'; expected implementation '{}' is missing on disk.",
                pkg_name.as_deref().unwrap_or("<package>"),
                mode.as_str(),
                export_name.as_deref().unwrap_or("<symbol>"),
                decl_file_rel,
                relative_to_cwd(cwd, &expected_abs),
            ),
        },
        ManifestFileOutcome::Ambiguous(paths) => {
            let candidates: Vec<ResolvedImplTargetPayload> = paths
                .iter()
                .map(|p| {
                    let loc_node = export_name.as_deref().and_then(|exp| {
                        cache
                            .load_external_source_file(cwd, p)
                            .and_then(|sf| {
                                find_exported_callable_in_sf(
                                    cache,
                                    cwd,
                                    sf,
                                    exp,
                                    container_name.as_deref(),
                                    0,
                                )
                            })
                            .map(|(_, fn_n)| fn_n)
                    });
                    build_target_payload(TargetPayloadInput {
                        cwd,
                        file_abs: p,
                        node_for_loc: loc_node,
                        export_name: export_name.clone(),
                        is_external: true,
                        package_name: pkg_name.clone(),
                        package_version: pkg_version.clone(),
                        subpath: Some(subpath.clone()),
                        mode,
                        conditions: conditions.clone(),
                        declaration_file: Some(decl_file_rel.clone()),
                    })
                })
                .collect();
            let rel_list: Vec<String> = candidates.iter().map(|c| c.file.clone()).collect();
            PackageImplLookup::Ambiguous {
                message: format!(
                    "Package '{}' subpath '{}' has ambiguous implementation candidates for '{}': {}.",
                    pkg_name.as_deref().unwrap_or("<package>"),
                    subpath,
                    export_name.as_deref().unwrap_or("<symbol>"),
                    rel_list.join(", "),
                ),
                candidates,
            }
        }
        ManifestFileOutcome::DeclarationOnly => PackageImplLookup::DeclarationOnly {
            message: format!(
                "Package '{}' provides declaration '{}' only with no local implementation source.",
                pkg_name.as_deref().unwrap_or("<package>"),
                decl_file_rel
            ),
        },
    }
}

pub fn resolve_impl_for_call_target(
    cache: &PackageImplCache,
    cwd: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    call_node: P<Node>,
    decl_node: Option<P<Node>>,
    fn_node: Option<P<Node>>,
) -> PackageImplLookup {
    let mut prov = call_node
        .expression()
        .and_then(|callee_expr| find_provenance_for_expr(program, checker, callee_expr));

    if prov.is_none()
        && let Some(dn) = decl_node
        && let Some(decl_sf) = get_source_file_of_node(dn)
        && decl_sf.is_declaration_file()
        && let Some(caller_sf) = get_source_file_of_node(call_node)
        && let Some((spec_node, mode)) =
            find_import_for_decl_sf(program, caller_sf, decl_sf.file_name())
    {
        prov = Some(ProvenanceSpec {
            containing_sf: caller_sf,
            specifier_node: Some(spec_node),
            specifier_text: string_literal_text(spec_node),
            export_name: dn.name().and_then(node_text),
            container_name: enclosing_container_name(dn),
            resolution_mode: mode,
        });
    }

    resolve_from_provenance_and_decl(cache, cwd, program, prov, decl_node, fn_node)
}

pub fn check_call_missing_package_gap(
    program: &'static Program,
    checker: &mut CheckerHandle,
    cwd: &str,
    call_node: P<Node>,
) -> Option<GapItem> {
    let callee_expr = call_node.expression()?;
    let prov = find_provenance_for_expr(program, checker, callee_expr)?;
    let spec_node = prov.specifier_node?;
    let spec_text = prov.specifier_text?;
    if !is_bare_package_specifier(&spec_text) {
        return None;
    }
    let resolved = program.get_resolved_module_from_module_specifier(prov.containing_sf, spec_node);
    if resolved.is_some_and(|r| r.is_resolved()) {
        return None;
    }
    Some(GapItem {
        kind: "missing-package",
        message: format!(
            "Package specifier '{spec_text}' is not installed or could not be resolved from '{}'.",
            relative_to_cwd(cwd, prov.containing_sf.file_name())
        ),
        at: node_location_rel(cwd, call_node),
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "Resolves each handle kind (call, syntax, declaration) into a structured ImplementationResolutionRow."
)]
pub fn run_resolve_implementations(
    snapshot: &SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    targets: &[String],
) -> Result<Vec<ImplementationResolutionRow>, RpcErr> {
    let mut resolved_inputs = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let entry = snapshot.get_entry(project_id, handle_id)?;
        match entry.payload {
            HandlePayload::Call(node) | HandlePayload::Syntax(node) => {
                resolved_inputs.push((handle_id.clone(), Some(node), None, None));
            }
            HandlePayload::Declaration { node, symbol } => {
                resolved_inputs.push((handle_id.clone(), None, Some(node), symbol));
            }
            _ => {
                return Err(RpcErr::new(
                    "INVALID_REFERENCE_KIND",
                    format!(
                        "Program reference {handle_id} cannot be used with resolveImplementations."
                    ),
                ));
            }
        }
    }

    let cwd = snapshot.cwd.clone();
    let cache = snapshot.package_impl_cache.clone();
    let mut rows = Vec::with_capacity(resolved_inputs.len());

    for (target_id, node_opt, decl_opt, sym_opt) in resolved_inputs {
        let (lookup, gap_at) = if let Some(node) = node_opt {
            if matches!(node.kind(), Kind::CallExpression | Kind::NewExpression) {
                let outcome = resolve_call_targets(&cwd, program, checker, node);
                let first = outcome.targets.first();
                let decl_n = first.map(|t| t.decl_node);
                let fn_n = first.and_then(|t| t.fn_node);
                let res = resolve_impl_for_call_target(
                    &cache,
                    cwd.as_str(),
                    program,
                    checker,
                    node,
                    decl_n,
                    fn_n,
                );
                (res, node_location_rel(&cwd, node))
            } else {
                let prov = match node.kind() {
                    Kind::ImportSpecifier
                    | Kind::ImportClause
                    | Kind::NamespaceImport
                    | Kind::ImportDeclaration
                    | Kind::ImportEqualsDeclaration
                    | Kind::ExportSpecifier => spec_from_import_decl_node(program, node, None),
                    Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral => node
                        .parent()
                        .and_then(|p| spec_from_import_decl_node(program, p, None))
                        .or_else(|| {
                            node.parent().and_then(|p| {
                                extract_require_specifier_node(p).and_then(|spec| {
                                    let sf = get_source_file_of_node(spec)?;
                                    Some(ProvenanceSpec {
                                        containing_sf: sf,
                                        specifier_node: Some(spec),
                                        specifier_text: string_literal_text(spec),
                                        export_name: None,
                                        container_name: None,
                                        resolution_mode: PackageResolutionMode::Require,
                                    })
                                })
                            })
                        }),
                    _ => find_provenance_for_expr(program, checker, node),
                };
                let sym = canonical_symbol_for_node(checker, node);
                let decl_n = sym.and_then(|s| pick_preferred_impl_decl(s, None));
                let fn_n = decl_n.and_then(extract_callable_from_decl);
                let res =
                    resolve_from_provenance_and_decl(&cache, &cwd, program, prov, decl_n, fn_n);
                (res, node_location_rel(&cwd, node))
            }
        } else {
            let decl_n = decl_opt;
            let fn_n = decl_n.and_then(extract_callable_from_decl);
            let prov = sym_opt
                .and_then(|s| trace_symbol_to_package_provenance(program, checker, s, None))
                .or_else(|| {
                    decl_n.and_then(|d| {
                        get_source_file_of_node(d).and_then(|sf| {
                            if !sf.is_declaration_file() {
                                return None;
                            }
                            for file in program.get_source_files() {
                                if !is_project_source_file(program, *file) {
                                    continue;
                                }
                                if let Some((spec_node, mode)) =
                                    find_import_for_decl_sf(program, *file, sf.file_name())
                                {
                                    return Some(ProvenanceSpec {
                                        containing_sf: *file,
                                        specifier_node: Some(spec_node),
                                        specifier_text: string_literal_text(spec_node),
                                        export_name: d.name().and_then(node_text),
                                        container_name: enclosing_container_name(d),
                                        resolution_mode: mode,
                                    });
                                }
                            }
                            None
                        })
                    })
                });
            let res = resolve_from_provenance_and_decl(&cache, &cwd, program, prov, decl_n, fn_n);
            (res, decl_n.and_then(|d| node_location_rel(&cwd, d)))
        };

        let row = match lookup {
            PackageImplLookup::SourceBody { info, .. } => {
                let ev = EvidenceItem::simple(
                    "source",
                    format!("Implementation is available in '{}'", info.file),
                    info.location.clone(),
                );
                ImplementationResolutionRow {
                    target: target_id,
                    status: "source",
                    implementation: Some(info),
                    candidates: Vec::new(),
                    evidence: vec![ev],
                    gaps: Vec::new(),
                }
            }
            PackageImplLookup::ResolvedExternal { info, .. } => {
                let mut evidence = Vec::new();
                if let Some(ref decl_file) = info.declaration_file {
                    evidence.push(EvidenceItem::simple(
                        "source",
                        format!(
                            "Mapped declaration '{}' to {} implementation '{}'",
                            decl_file, info.resolution_mode, info.file
                        ),
                        info.location.clone(),
                    ));
                } else {
                    evidence.push(EvidenceItem::simple(
                        "source",
                        format!(
                            "Resolved {} implementation in '{}'",
                            info.resolution_mode, info.file
                        ),
                        info.location.clone(),
                    ));
                }
                ImplementationResolutionRow {
                    target: target_id,
                    status: "resolved",
                    implementation: Some(info),
                    candidates: Vec::new(),
                    evidence,
                    gaps: Vec::new(),
                }
            }
            PackageImplLookup::DeclarationOnly { message, .. } => ImplementationResolutionRow {
                target: target_id,
                status: "declaration-only",
                implementation: None,
                candidates: Vec::new(),
                evidence: Vec::new(),
                gaps: vec![GapItem {
                    kind: "missing-package-implementation",
                    message,
                    at: gap_at,
                }],
            },
            PackageImplLookup::MissingPackage { message, .. } => ImplementationResolutionRow {
                target: target_id,
                status: "missing-package",
                implementation: None,
                candidates: Vec::new(),
                evidence: Vec::new(),
                gaps: vec![GapItem {
                    kind: "missing-package",
                    message,
                    at: gap_at,
                }],
            },
            PackageImplLookup::Ambiguous {
                message,
                candidates,
            } => ImplementationResolutionRow {
                target: target_id,
                status: "ambiguous",
                implementation: None,
                candidates,
                evidence: Vec::new(),
                gaps: vec![GapItem {
                    kind: "ambiguous-package-implementation",
                    message,
                    at: gap_at,
                }],
            },
            PackageImplLookup::Unimplemented { message } => ImplementationResolutionRow {
                target: target_id,
                status: "unimplemented",
                implementation: None,
                candidates: Vec::new(),
                evidence: Vec::new(),
                gaps: vec![GapItem {
                    kind: "missing-package-implementation",
                    message,
                    at: gap_at,
                }],
            },
        };
        rows.push(row);
    }

    Ok(rows)
}
