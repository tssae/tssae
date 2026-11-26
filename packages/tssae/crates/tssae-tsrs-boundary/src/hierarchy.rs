use rustc_hash::{FxHashMap, FxHashSet};
use tsrs_ast::{
    Kind, ModifierFlags, Node, SourceFile, Symbol, SymbolFlags, get_combined_modifier_flags,
    get_source_file_of_node, is_function_like_declaration,
};
use tsrs_checker::{Type, TypeFlags};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::P;
use tssae_common::{
    ClassMemberFact, ClassOrInterfaceKind, MemberTargetHierarchy, SymbolId, SyntaxId,
    TypeHierarchyNode,
};

use crate::{
    impl_resolution::is_first_party_workspace_path,
    semantic_facade::{unwrap_aliased_symbol, unwrap_sym_declarations},
    syntax_view::{
        is_project_source_file, node_location_relative, node_text, prop_key_text,
        unwrap_expr_wrappers,
    },
};

/// Indexed class/interface inheritance graph collected lazily across the program's source files.
pub(crate) struct ProgramHierarchyIndex {
    nodes_by_symbol: FxHashMap<P<Symbol>, TypeHierarchyNodeRaw>,
}

#[derive(Clone)]
struct TypeHierarchyNodeRaw {
    node: P<Node>,
    symbol: P<Symbol>,
    name: Option<String>,
    kind: ClassOrInterfaceKind,
    is_exported: bool,
    is_abstract: bool,
    is_external: bool,
    base_symbols: Vec<P<Symbol>>,
    members: Vec<RawMemberFact>,
    constructor_node: Option<P<Node>>,
}

#[derive(Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Represents independent member modifier flags mirrored into ClassMemberFact."
)]
struct RawMemberFact {
    name: String,
    member_node: P<Node>,
    symbol: Option<P<Symbol>>,
    kind: String,
    has_body: bool,
    is_abstract: bool,
    is_static: bool,
    is_private_ecma: bool,
    is_override: bool,
    initializer_node: Option<P<Node>>,
    callable_node: Option<P<Node>>,
}

pub(crate) struct HierarchyQueryEnv<'a> {
    pub(crate) cwd: &'a str,
    pub(crate) program: &'static Program,
    pub(crate) checker: &'a mut CheckerHandle,
}

pub(crate) trait HierarchyInterner {
    fn intern_syntax(&mut self, node: P<Node>) -> Option<SyntaxId>;
    fn intern_symbol(&mut self, sym: P<Symbol>) -> SymbolId;
}

impl ProgramHierarchyIndex {
    pub(crate) fn build(
        cwd: &str,
        program: &'static Program,
        workspace_programs: &[&'static Program],
        checker: &mut CheckerHandle,
    ) -> Self {
        let mut exported_symbols: FxHashSet<P<Symbol>> = FxHashSet::default();
        let mut nodes_by_symbol: FxHashMap<P<Symbol>, TypeHierarchyNodeRaw> = FxHashMap::default();
        let mut seen_sfs: FxHashSet<P<SourceFile>> = FxHashSet::default();

        // Collect all non-default-library source files across primary & workspace programs,
        // plus any source files referenced directly via `program.get_source_files()`.
        let mut source_files: Vec<P<SourceFile>> = Vec::new();
        for &sf in program.get_source_files() {
            if seen_sfs.insert(sf) {
                source_files.push(sf);
            }
        }
        for &wp in workspace_programs {
            for &sf in wp.get_source_files() {
                if !wp.is_source_file_default_library(sf.path()) && seen_sfs.insert(sf) {
                    source_files.push(sf);
                }
            }
        }

        // Gather module-level exported symbols (`export { X }`, `export default X`, etc.).
        for &sf in &source_files {
            if program.is_source_file_default_library(sf.path()) {
                continue;
            }
            if let Some(sf_sym) = checker.get_symbol_at_location_exported(sf.as_node()) {
                let exports = checker.get_exports_of_module_exported(sf_sym);
                for exp in exports {
                    let unwrapped = unwrap_aliased_symbol(checker, exp);
                    if unwrapped != checker.get_unknown_symbol() {
                        exported_symbols.insert(unwrapped);
                    }
                }
            }
            collect_export_assignments(sf.as_node(), checker, &mut exported_symbols);
        }

        // Index every ClassDeclaration, ClassExpression, and InterfaceDeclaration in the snapshot.
        for &sf in &source_files {
            if program.is_source_file_default_library(sf.path()) {
                continue;
            }
            let is_external_file = !is_project_source_file(program, sf)
                && !is_first_party_workspace_path(cwd, sf.file_name());
            index_types_in_node(
                sf.as_node(),
                is_external_file,
                checker,
                &exported_symbols,
                &mut nodes_by_symbol,
            );
        }

        Self { nodes_by_symbol }
    }

    pub(crate) fn build_hierarchy_for_receiver(
        &self,
        env: &mut HierarchyQueryEnv<'_>,
        receiver_node: P<Node>,
        static_receiver: bool,
        interner: &mut impl HierarchyInterner,
    ) -> Option<MemberTargetHierarchy> {
        if static_receiver {
            let expr = if receiver_node.kind() == Kind::PropertyAccessExpression {
                receiver_node.as_property_access_expression().expression()
            } else {
                receiver_node
            };
            let sym = env
                .checker
                .get_symbol_at_location_exported(expr)
                .map(|s| unwrap_aliased_symbol(env.checker, s))
                .filter(|&s| s != env.checker.get_unknown_symbol())?;
            return Some(self.hierarchy_from_symbols(env, &[sym], false, false, interner));
        }

        let ty = env.checker.get_type_at_location(receiver_node);
        let mut syms: Vec<P<Symbol>> = Vec::new();
        let mut has_unresolved = false;
        let is_union_or_intersection = ty.is_union() || ty.is_intersection();
        collect_receiver_type_symbols(ty, env.checker, &mut syms, &mut has_unresolved, 0);

        if syms.is_empty() && !has_unresolved {
            return Some(MemberTargetHierarchy {
                receiver_kind: ClassOrInterfaceKind::OtherType,
                receiver_symbols: Vec::new(),
                receiver_name: None,
                receiver_is_exported: false,
                receiver_is_abstract: false,
                receiver_is_external: false,
                is_union_or_intersection,
                has_unresolved_type: false,
                candidate_classes: Vec::new(),
            });
        }

        Some(self.hierarchy_from_symbols(
            env,
            &syms,
            is_union_or_intersection,
            has_unresolved,
            interner,
        ))
    }

    pub(crate) fn hierarchy_from_symbols(
        &self,
        env: &mut HierarchyQueryEnv<'_>,
        root_syms: &[P<Symbol>],
        is_union_or_intersection: bool,
        has_unresolved_type: bool,
        interner: &mut impl HierarchyInterner,
    ) -> MemberTargetHierarchy {
        if root_syms.is_empty() {
            return MemberTargetHierarchy {
                receiver_kind: ClassOrInterfaceKind::OtherType,
                receiver_symbols: Vec::new(),
                receiver_name: None,
                receiver_is_exported: false,
                receiver_is_abstract: false,
                receiver_is_external: false,
                is_union_or_intersection,
                has_unresolved_type,
                candidate_classes: Vec::new(),
            };
        }

        let mut all_nodes: FxHashMap<P<Symbol>, TypeHierarchyNodeRaw> = FxHashMap::default();
        let mut receiver_symbols = Vec::new();
        let mut receiver_kind = ClassOrInterfaceKind::OtherType;
        let mut receiver_name = None;
        let mut receiver_is_exported = false;
        let mut receiver_is_abstract = false;
        let mut receiver_is_external = false;

        for (idx, &sym) in root_syms.iter().enumerate() {
            let sid = interner.intern_symbol(sym);
            receiver_symbols.push(sid);
            let node_raw = self
                .nodes_by_symbol
                .get(&sym)
                .cloned()
                .or_else(|| synthesize_external_node(env.cwd, env.program, env.checker, sym));
            if let Some(raw) = node_raw {
                if idx == 0 {
                    receiver_kind = raw.kind;
                    receiver_name.clone_from(&raw.name);
                    receiver_is_exported = raw.is_exported;
                    receiver_is_abstract = raw.is_abstract;
                    receiver_is_external = raw.is_external;
                } else {
                    if raw.kind == ClassOrInterfaceKind::Interface {
                        receiver_kind = ClassOrInterfaceKind::Interface;
                    }
                    receiver_is_exported |= raw.is_exported;
                    receiver_is_abstract |= raw.is_abstract;
                    receiver_is_external |= raw.is_external;
                }
                all_nodes.insert(sym, raw);
            }
        }

        for (&sym, raw) in &self.nodes_by_symbol {
            all_nodes.entry(sym).or_insert_with(|| raw.clone());
        }

        let mut candidate_classes = Vec::with_capacity(all_nodes.len());
        for raw in all_nodes.into_values() {
            if let Some(converted) = convert_raw_hierarchy_node(env.cwd, raw, interner) {
                candidate_classes.push(converted);
            }
        }

        MemberTargetHierarchy {
            receiver_kind,
            receiver_symbols,
            receiver_name,
            receiver_is_exported,
            receiver_is_abstract,
            receiver_is_external,
            is_union_or_intersection,
            has_unresolved_type,
            candidate_classes,
        }
    }
}

fn convert_raw_hierarchy_node(
    cwd: &str,
    raw: TypeHierarchyNodeRaw,
    interner: &mut impl HierarchyInterner,
) -> Option<TypeHierarchyNode> {
    let syntax = interner.intern_syntax(raw.node)?;
    let symbol = interner.intern_symbol(raw.symbol);
    let base_symbols = raw
        .base_symbols
        .into_iter()
        .map(|s| interner.intern_symbol(s))
        .collect();
    let constructor_syntax = raw.constructor_node.and_then(|n| interner.intern_syntax(n));
    let mut members = Vec::with_capacity(raw.members.len());
    for m in raw.members {
        let Some(member_syntax) = interner.intern_syntax(m.member_node) else {
            continue;
        };
        members.push(ClassMemberFact {
            name: m.name,
            member_syntax,
            symbol: m.symbol.map(|s| interner.intern_symbol(s)),
            kind: m.kind,
            location: node_location_relative(cwd, m.member_node),
            has_body: m.has_body,
            is_abstract: m.is_abstract,
            is_static: m.is_static,
            is_private_ecma: m.is_private_ecma,
            is_override: m.is_override,
            initializer_syntax: m.initializer_node.and_then(|n| interner.intern_syntax(n)),
            callable_syntax: m.callable_node.and_then(|n| interner.intern_syntax(n)),
        });
    }

    Some(TypeHierarchyNode {
        syntax,
        symbol,
        name: raw.name,
        kind: raw.kind,
        is_exported: raw.is_exported,
        is_abstract: raw.is_abstract,
        is_external: raw.is_external,
        base_symbols,
        members,
        constructor_syntax,
    })
}

fn collect_export_assignments(
    node: P<Node>,
    checker: &mut CheckerHandle,
    exported_symbols: &mut FxHashSet<P<Symbol>>,
) {
    if node.kind() == Kind::ExportAssignment
        && let Some(expr) = node.expression()
        && let Some(sym) = checker.get_symbol_at_location_exported(unwrap_expr_wrappers(expr))
    {
        let unwrapped = unwrap_aliased_symbol(checker, sym);
        if unwrapped != checker.get_unknown_symbol() {
            exported_symbols.insert(unwrapped);
        }
    }
    node.for_each_child(&mut |child| {
        collect_export_assignments(child, checker, exported_symbols);
        false
    });
}

fn index_types_in_node(
    node: P<Node>,
    is_external_file: bool,
    checker: &mut CheckerHandle,
    exported_symbols: &FxHashSet<P<Symbol>>,
    out: &mut FxHashMap<P<Symbol>, TypeHierarchyNodeRaw>,
) {
    if matches!(
        node.kind(),
        Kind::ClassDeclaration | Kind::ClassExpression | Kind::InterfaceDeclaration
    ) && let Some(raw) =
        extract_hierarchy_node_raw(node, is_external_file, checker, exported_symbols)
    {
        out.entry(raw.symbol).or_insert(raw);
    }

    node.for_each_child(&mut |child| {
        index_types_in_node(child, is_external_file, checker, exported_symbols, out);
        false
    });
}

fn extract_hierarchy_node_raw(
    node: P<Node>,
    is_external_file: bool,
    checker: &mut CheckerHandle,
    exported_symbols: &FxHashSet<P<Symbol>>,
) -> Option<TypeHierarchyNodeRaw> {
    let sym = checker
        .get_symbol_at_location_exported(node)
        .or_else(|| {
            node.name()
                .and_then(|n| checker.get_symbol_at_location_exported(n))
        })
        .or_else(|| node.symbol())
        .map(|s| unwrap_aliased_symbol(checker, s))
        .filter(|&s| s != checker.get_unknown_symbol())?;

    let mod_flags = get_combined_modifier_flags(node);
    let is_exported =
        mod_flags.intersects(ModifierFlags::Export) || exported_symbols.contains(&sym);
    let is_abstract = mod_flags.intersects(ModifierFlags::Abstract);
    let kind = if node.kind() == Kind::InterfaceDeclaration {
        ClassOrInterfaceKind::Interface
    } else {
        ClassOrInterfaceKind::Class
    };

    let name = node
        .name()
        .and_then(node_text)
        .or_else(|| Some(sym.name().to_string()).filter(|s| !s.is_empty() && s != "__class"));

    let mut base_symbols = Vec::new();
    let declared_ty = checker.get_declared_type_of_symbol_exported(sym);
    for &base_ty in checker.get_base_types_exported(declared_ty) {
        if let Some(base_sym) = base_ty.symbol() {
            let unwrapped = unwrap_aliased_symbol(checker, base_sym);
            if unwrapped != checker.get_unknown_symbol() && !base_symbols.contains(&unwrapped) {
                base_symbols.push(unwrapped);
            }
        }
    }

    let heritage = match node.kind() {
        Kind::ClassDeclaration => node.as_class_declaration().heritage_clauses(),
        Kind::ClassExpression => node.as_class_expression().heritage_clauses(),
        Kind::InterfaceDeclaration => node.as_interface_declaration().heritage_clauses(),
        _ => None,
    };
    if let Some(clauses) = heritage {
        for &clause in clauses.nodes() {
            if clause.kind() != Kind::HeritageClause {
                continue;
            }
            for &ty_node in clause.as_heritage_clause().types().nodes() {
                if ty_node.kind() != Kind::ExpressionWithTypeArguments {
                    continue;
                }
                let expr = ty_node.as_expression_with_type_arguments().expression();
                if let Some(base_sym) = checker.get_symbol_at_location_exported(expr) {
                    let unwrapped = unwrap_aliased_symbol(checker, base_sym);
                    if unwrapped != checker.get_unknown_symbol()
                        && !base_symbols.contains(&unwrapped)
                    {
                        base_symbols.push(unwrapped);
                    }
                }
            }
        }
    }

    let raw_members: &[P<Node>] = match node.kind() {
        Kind::ClassDeclaration => node.as_class_declaration().members().nodes(),
        Kind::ClassExpression => node.as_class_expression().members().nodes(),
        Kind::InterfaceDeclaration => node.as_interface_declaration().members().nodes(),
        _ => &[],
    };

    let mut constructor_node: Option<P<Node>> = None;
    let mut members: Vec<RawMemberFact> = Vec::new();
    for &member in raw_members {
        if member.kind() == Kind::Constructor {
            if member.body().is_some() || constructor_node.is_none() {
                constructor_node = Some(member);
            }
            continue;
        }
        if let Some(fact) = extract_member_fact(member, checker) {
            // If an overloaded method has both signature and implementation body, prefer bodied entry.
            if let Some(existing) = members.iter_mut().find(|m| {
                m.name == fact.name && m.kind == fact.kind && m.is_static == fact.is_static
            }) {
                if fact.has_body && !existing.has_body {
                    *existing = fact;
                }
            } else {
                members.push(fact);
            }
        }
    }

    Some(TypeHierarchyNodeRaw {
        node,
        symbol: sym,
        name,
        kind,
        is_exported,
        is_abstract,
        is_external: is_external_file,
        base_symbols,
        members,
        constructor_node,
    })
}

fn extract_member_fact(member: P<Node>, checker: &mut CheckerHandle) -> Option<RawMemberFact> {
    let mk = member.kind();
    if !matches!(
        mk,
        Kind::MethodDeclaration
            | Kind::GetAccessor
            | Kind::SetAccessor
            | Kind::PropertyDeclaration
            | Kind::MethodSignature
            | Kind::PropertySignature
    ) {
        return None;
    }
    let name_node = member.name()?;
    let name = extract_member_name(name_node)?;
    let mod_flags = get_combined_modifier_flags(member);
    let is_static = mod_flags.intersects(ModifierFlags::Static);
    let is_abstract = mod_flags.intersects(ModifierFlags::Abstract);
    let is_override = mod_flags.intersects(ModifierFlags::Override);
    let is_private_ecma = name_node.kind() == Kind::PrivateIdentifier;

    let sym = checker
        .get_symbol_at_location_exported(member)
        .or_else(|| checker.get_symbol_at_location_exported(name_node))
        .or_else(|| member.symbol())
        .map(|s| unwrap_aliased_symbol(checker, s))
        .filter(|&s| s != checker.get_unknown_symbol());

    let (has_body, initializer_node, callable_node) = match mk {
        Kind::MethodDeclaration | Kind::GetAccessor | Kind::SetAccessor => {
            let bodied = member.body().is_some();
            (bodied, None, bodied.then_some(member))
        }
        Kind::PropertyDeclaration => {
            let init = member.initializer().map(unwrap_expr_wrappers);
            let callable = init.filter(|&n| is_function_like_declaration(Some(n)));
            let bodied = callable.is_some_and(|f| f.body().is_some());
            (bodied, init, callable)
        }
        _ => (false, None, None),
    };

    Some(RawMemberFact {
        name,
        member_node: member,
        symbol: sym,
        kind: format!("{mk:?}"),
        has_body,
        is_abstract,
        is_static,
        is_private_ecma,
        is_override,
        initializer_node,
        callable_node,
    })
}

fn extract_member_name(name_node: P<Node>) -> Option<String> {
    if name_node.kind() == Kind::ComputedPropertyName {
        if let Some(expr) = name_node.expression()
            && let Some(raw) = node_text(expr)
        {
            let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
            if compact == "Symbol.dispose" {
                return Some("[Symbol.dispose]".to_string());
            }
            if compact == "Symbol.asyncDispose" {
                return Some("[Symbol.asyncDispose]".to_string());
            }
        }
        return prop_key_text(name_node);
    }
    if name_node.kind() == Kind::PrivateIdentifier {
        return node_text(name_node);
    }
    prop_key_text(name_node).or_else(|| node_text(name_node))
}

fn synthesize_external_node(
    cwd: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    sym: P<Symbol>,
) -> Option<TypeHierarchyNodeRaw> {
    let decls = unwrap_sym_declarations(sym);
    let decl = decls.into_iter().find(|&d| {
        matches!(
            d.kind(),
            Kind::ClassDeclaration | Kind::ClassExpression | Kind::InterfaceDeclaration
        )
    })?;
    let sf = get_source_file_of_node(decl)?;
    let is_external = program.is_source_file_default_library(sf.path())
        || !is_first_party_workspace_path(cwd, sf.file_name());
    let empty_exports = FxHashSet::default();
    let mut raw = extract_hierarchy_node_raw(decl, is_external, checker, &empty_exports)?;
    raw.is_external = is_external;
    raw.is_exported = true;
    Some(raw)
}

fn collect_receiver_type_symbols(
    ty: P<Type>,
    checker: &mut CheckerHandle,
    out_syms: &mut Vec<P<Symbol>>,
    has_unresolved: &mut bool,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    if ty.flags().intersects(TypeFlags::Any | TypeFlags::Unknown) {
        *has_unresolved = true;
        return;
    }
    if ty
        .flags()
        .intersects(TypeFlags::Null | TypeFlags::Undefined | TypeFlags::Void)
    {
        return;
    }
    if ty.is_union() || ty.is_intersection() {
        for &member in ty.types() {
            collect_receiver_type_symbols(member, checker, out_syms, has_unresolved, depth + 1);
        }
        return;
    }
    if ty.flags().intersects(TypeFlags::TypeParameter)
        && let Some(constraint) = checker.get_base_constraint_of_type_exported(ty)
    {
        collect_receiver_type_symbols(constraint, checker, out_syms, has_unresolved, depth + 1);
        return;
    }
    if ty.symbol().is_none() && ty.flags().intersects(TypeFlags::Object) {
        // Check anonymous/object literal type vs apparent type.
        let apparent = checker.get_apparent_type_exported(ty);
        if let Some(sym) = apparent.symbol()
            && (sym
                .flags()
                .intersects(SymbolFlags::Class | SymbolFlags::Interface))
        {
            let unwrapped = unwrap_aliased_symbol(checker, sym);
            if unwrapped != checker.get_unknown_symbol() && !out_syms.contains(&unwrapped) {
                out_syms.push(unwrapped);
            }
        }
        return;
    }
    if let Some(sym) = ty
        .symbol()
        .or_else(|| checker.get_apparent_type_exported(ty).symbol())
    {
        let unwrapped = unwrap_aliased_symbol(checker, sym);
        if unwrapped != checker.get_unknown_symbol()
            && unwrapped
                .flags()
                .intersects(SymbolFlags::Class | SymbolFlags::Interface)
            && !out_syms.contains(&unwrapped)
        {
            out_syms.push(unwrapped);
        }
    }
}
