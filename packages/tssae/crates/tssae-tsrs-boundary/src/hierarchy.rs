use std::{collections::VecDeque, sync::Mutex};

use rustc_hash::{FxHashMap, FxHashSet};
use tsrs_ast::{
    Kind, ModifierFlags, Node, NodeList, SourceFile, Symbol, SymbolFlags,
    get_combined_modifier_flags, get_source_file_of_node, is_function_like_declaration,
};
use tsrs_checker::{Type, TypeFlags};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::P;
use tssae_common::{
    ClassMemberFact, ClassOrInterfaceKind, DiagnosticLocation, MemberTargetHierarchy, SymbolId,
    SyntaxId, TypeHierarchyNode,
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
pub struct ProgramHierarchyIndex {
    children_by_base: FxHashMap<P<Symbol>, Vec<P<Symbol>>>,
    known_decl_nodes: FxHashMap<P<Symbol>, Vec<(P<Node>, bool)>>,
    lazy: Mutex<HierarchyLazyState>,
}

#[derive(Default)]
struct HierarchyLazyState {
    nodes_by_symbol: FxHashMap<P<Symbol>, TypeHierarchyNodeRaw>,
    exported_by_sf: FxHashMap<P<SourceFile>, FxHashSet<P<Symbol>>>,
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
    location: Option<DiagnosticLocation>,
    has_body: bool,
    is_abstract: bool,
    is_static: bool,
    is_private: bool,
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
        let mut children_by_base: FxHashMap<P<Symbol>, Vec<P<Symbol>>> = FxHashMap::default();
        let mut known_decl_nodes: FxHashMap<P<Symbol>, Vec<(P<Node>, bool)>> = FxHashMap::default();
        let mut seen_sfs: FxHashSet<P<SourceFile>> = FxHashSet::default();

        // Collect all non-default-library source files across primary & workspace programs,
        // filtering to files that contain class/interface heritage declarations.
        let mut source_files: Vec<P<SourceFile>> = Vec::new();
        for &sf in program.get_source_files() {
            if !program.is_source_file_default_library(sf.path()) && seen_sfs.insert(sf) {
                let text = sf.text();
                if (text.contains("class") || text.contains("interface"))
                    && (text.contains("extends") || text.contains("implements"))
                {
                    source_files.push(sf);
                }
            }
        }
        for &wp in workspace_programs {
            for &sf in wp.get_source_files() {
                if !wp.is_source_file_default_library(sf.path())
                    && !program.is_source_file_default_library(sf.path())
                    && seen_sfs.insert(sf)
                {
                    let text = sf.text();
                    if (text.contains("class") || text.contains("interface"))
                        && (text.contains("extends") || text.contains("implements"))
                    {
                        source_files.push(sf);
                    }
                }
            }
        }

        // Phase 1: walk ASTs only for ClassDeclaration/ClassExpression/InterfaceDeclaration nodes
        // with heritage clauses to populate `children_by_base` and `known_decl_nodes` without
        // computing exports or extracting members up front.
        for &sf in &source_files {
            let is_external_file = !is_project_source_file(program, sf)
                && !is_first_party_workspace_path(cwd, sf.file_name());
            collect_heritage_subtypes_in_node(
                sf.as_node(),
                is_external_file,
                checker,
                &mut children_by_base,
                &mut known_decl_nodes,
            );
        }

        Self {
            children_by_base,
            known_decl_nodes,
            lazy: Mutex::new(HierarchyLazyState::default()),
        }
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
            if let Some(raw) = self.ensure_symbol_node_loaded(env, sym) {
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

        self.collect_connected_hierarchy(env, root_syms, &mut all_nodes);

        let mut candidate_classes = Vec::with_capacity(all_nodes.len());
        for raw in all_nodes.into_values() {
            if let Some(converted) = convert_raw_hierarchy_node(raw, interner) {
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

    fn collect_connected_hierarchy(
        &self,
        env: &mut HierarchyQueryEnv<'_>,
        root_syms: &[P<Symbol>],
        all_nodes: &mut FxHashMap<P<Symbol>, TypeHierarchyNodeRaw>,
    ) {
        // a) BFS downward through `children_by_base` for all reachable descendants.
        let mut down_queue: VecDeque<P<Symbol>> = VecDeque::new();
        let mut visited_down: FxHashSet<P<Symbol>> = FxHashSet::default();
        for &sym in root_syms {
            if visited_down.insert(sym) {
                down_queue.push_back(sym);
            }
        }
        while let Some(cur) = down_queue.pop_front() {
            if let Some(children) = self.children_by_base.get(&cur) {
                for &child in children {
                    if visited_down.insert(child) {
                        if let Some(raw) = self.ensure_symbol_node_loaded(env, child) {
                            all_nodes.entry(child).or_insert(raw);
                        }
                        down_queue.push_back(child);
                    }
                }
            }
        }

        // b) BFS upward from all nodes currently in `all_nodes` through `base_symbols`
        //    to include all reachable ancestors (synthesizing external nodes when needed).
        let mut up_queue: VecDeque<P<Symbol>> = all_nodes.keys().copied().collect();
        let mut visited_up: FxHashSet<P<Symbol>> = all_nodes.keys().copied().collect();
        while let Some(cur) = up_queue.pop_front() {
            let bases: Vec<P<Symbol>> = all_nodes
                .get(&cur)
                .map_or_default(|raw| raw.base_symbols.clone());
            for base_sym in bases {
                if visited_up.insert(base_sym)
                    && let Some(raw) = self.ensure_symbol_node_loaded(env, base_sym)
                {
                    all_nodes.insert(base_sym, raw);
                    up_queue.push_back(base_sym);
                }
            }
        }
    }

    fn ensure_symbol_node_loaded(
        &self,
        env: &mut HierarchyQueryEnv<'_>,
        sym: P<Symbol>,
    ) -> Option<TypeHierarchyNodeRaw> {
        {
            if let Ok(state) = self.lazy.lock()
                && let Some(cached) = state.nodes_by_symbol.get(&sym)
            {
                return Some(cached.clone());
            }
        }

        let mut decls: Vec<(P<Node>, bool)> = Vec::new();
        if let Some(known) = self.known_decl_nodes.get(&sym) {
            for &(node, is_ext) in known {
                if !decls.iter().any(|(existing, _)| *existing == node) {
                    decls.push((node, is_ext));
                }
            }
        }
        for decl in unwrap_sym_declarations(sym) {
            if !matches!(
                decl.kind(),
                Kind::ClassDeclaration | Kind::ClassExpression | Kind::InterfaceDeclaration
            ) || decls.iter().any(|(existing, _)| *existing == decl)
            {
                continue;
            }
            let Some(sf) = get_source_file_of_node(decl) else {
                continue;
            };
            let is_ext = env.program.is_source_file_default_library(sf.path())
                || (!is_project_source_file(env.program, sf)
                    && !is_first_party_workspace_path(env.cwd, sf.file_name()));
            decls.push((decl, is_ext));
        }

        if decls.is_empty() {
            return None;
        }

        let primary_idx = decls
            .iter()
            .position(|(d, _)| {
                get_source_file_of_node(*d)
                    .is_some_and(|sf| !env.program.is_source_file_default_library(sf.path()))
            })
            .unwrap_or(0);
        let (primary_node, is_external) = decls[primary_idx];

        let kind = if decls
            .iter()
            .any(|(d, _)| matches!(d.kind(), Kind::ClassDeclaration | Kind::ClassExpression))
        {
            ClassOrInterfaceKind::Class
        } else {
            ClassOrInterfaceKind::Interface
        };
        let is_abstract = decls
            .iter()
            .any(|(d, _)| get_combined_modifier_flags(*d).intersects(ModifierFlags::Abstract));

        let mut is_exported = decls.iter().any(|(d, _)| {
            (d.parent().is_some_and(|p| p.kind() == Kind::SourceFile)
                && get_combined_modifier_flags(*d).intersects(ModifierFlags::Export))
                || get_source_file_of_node(*d)
                    .is_some_and(|sf| env.program.is_source_file_default_library(sf.path()))
        });
        if !is_exported {
            for &(d, _) in &decls {
                if let Some(sf) = get_source_file_of_node(d)
                    && self.is_symbol_exported_in_sf(env, sf, sym)
                {
                    is_exported = true;
                    break;
                }
            }
        }

        let name = primary_node
            .name()
            .and_then(node_text)
            .or_else(|| Some(sym.name().to_string()).filter(|s| !s.is_empty() && s != "__class"));

        let base_symbols = extract_base_symbols_for_decls(&decls, sym, env.checker);
        let (members, constructor_node) = extract_members_for_decls(env.cwd, &decls, env.checker);

        let raw = TypeHierarchyNodeRaw {
            node: primary_node,
            symbol: sym,
            name,
            kind,
            is_exported,
            is_abstract,
            is_external,
            base_symbols,
            members,
            constructor_node,
        };

        if let Ok(mut state) = self.lazy.lock() {
            return Some(state.nodes_by_symbol.entry(sym).or_insert(raw).clone());
        }
        Some(raw)
    }

    fn is_symbol_exported_in_sf(
        &self,
        env: &mut HierarchyQueryEnv<'_>,
        sf: P<SourceFile>,
        sym: P<Symbol>,
    ) -> bool {
        {
            if let Ok(state) = self.lazy.lock()
                && let Some(exported) = state.exported_by_sf.get(&sf)
            {
                return exported.contains(&sym);
            }
        }

        let mut exported: FxHashSet<P<Symbol>> = FxHashSet::default();
        if let Some(sf_sym) = env.checker.get_symbol_at_location_exported(sf.as_node()) {
            for exp in env.checker.get_exports_of_module_exported(sf_sym) {
                let unwrapped = unwrap_aliased_symbol(env.checker, exp);
                if unwrapped != env.checker.get_unknown_symbol() {
                    exported.insert(unwrapped);
                }
            }
        }
        collect_export_assignments(sf.as_node(), env.checker, &mut exported);

        let result = exported.contains(&sym);
        if let Ok(mut state) = self.lazy.lock() {
            return state
                .exported_by_sf
                .entry(sf)
                .or_insert(exported)
                .contains(&sym);
        }
        result
    }
}

fn convert_raw_hierarchy_node(
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
            location: m.location,
            has_body: m.has_body,
            is_abstract: m.is_abstract,
            is_static: m.is_static,
            is_private: m.is_private,
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

fn heritage_clauses_of(node: P<Node>) -> Option<P<NodeList>> {
    match node.kind() {
        Kind::ClassDeclaration => node.as_class_declaration().heritage_clauses(),
        Kind::ClassExpression => node.as_class_expression().heritage_clauses(),
        Kind::InterfaceDeclaration => node.as_interface_declaration().heritage_clauses(),
        _ => None,
    }
}

fn resolve_type_decl_symbol(node: P<Node>, checker: &mut CheckerHandle) -> Option<P<Symbol>> {
    checker
        .get_symbol_at_location_exported(node)
        .or_else(|| {
            node.name()
                .and_then(|n| checker.get_symbol_at_location_exported(n))
        })
        .or_else(|| node.symbol())
        .map(|s| unwrap_aliased_symbol(checker, s))
        .filter(|&s| s != checker.get_unknown_symbol())
}

fn collect_heritage_subtypes_in_node(
    node: P<Node>,
    is_external_file: bool,
    checker: &mut CheckerHandle,
    children_by_base: &mut FxHashMap<P<Symbol>, Vec<P<Symbol>>>,
    known_decl_nodes: &mut FxHashMap<P<Symbol>, Vec<(P<Node>, bool)>>,
) {
    if matches!(
        node.kind(),
        Kind::ClassDeclaration | Kind::ClassExpression | Kind::InterfaceDeclaration
    ) && heritage_clauses_of(node).is_some_and(|clauses| !clauses.nodes().is_empty())
        && let Some(sym) = resolve_type_decl_symbol(node, checker)
    {
        let decls = known_decl_nodes.entry(sym).or_default();
        if !decls.iter().any(|(existing, _)| *existing == node) {
            decls.push((node, is_external_file));
        }
        for base_sym in extract_base_symbols(node, sym, checker) {
            let children = children_by_base.entry(base_sym).or_default();
            if !children.contains(&sym) {
                children.push(sym);
            }
        }
    }

    node.for_each_child(&mut |child| {
        collect_heritage_subtypes_in_node(
            child,
            is_external_file,
            checker,
            children_by_base,
            known_decl_nodes,
        );
        false
    });
}

fn extract_base_symbols(
    node: P<Node>,
    sym: P<Symbol>,
    checker: &mut CheckerHandle,
) -> Vec<P<Symbol>> {
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
    collect_heritage_base_symbols(node, checker, &mut base_symbols);
    base_symbols
}

fn extract_base_symbols_for_decls(
    decls: &[(P<Node>, bool)],
    sym: P<Symbol>,
    checker: &mut CheckerHandle,
) -> Vec<P<Symbol>> {
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
    for &(decl, _) in decls {
        collect_heritage_base_symbols(decl, checker, &mut base_symbols);
    }
    base_symbols
}

fn collect_heritage_base_symbols(
    node: P<Node>,
    checker: &mut CheckerHandle,
    base_symbols: &mut Vec<P<Symbol>>,
) {
    let Some(clauses) = heritage_clauses_of(node) else {
        return;
    };
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
                if unwrapped != checker.get_unknown_symbol() && !base_symbols.contains(&unwrapped) {
                    base_symbols.push(unwrapped);
                }
            }
        }
    }
}

fn extract_members_for_decls(
    cwd: &str,
    decls: &[(P<Node>, bool)],
    checker: &mut CheckerHandle,
) -> (Vec<RawMemberFact>, Option<P<Node>>) {
    let mut constructor_node: Option<P<Node>> = None;
    let mut members: Vec<RawMemberFact> = Vec::new();
    for &(decl, _) in decls {
        let raw_members: &[P<Node>] = match decl.kind() {
            Kind::ClassDeclaration => decl.as_class_declaration().members().nodes(),
            Kind::ClassExpression => decl.as_class_expression().members().nodes(),
            Kind::InterfaceDeclaration => decl.as_interface_declaration().members().nodes(),
            _ => &[],
        };
        for &member in raw_members {
            if member.kind() == Kind::Constructor {
                if member.body().is_some() || constructor_node.is_none() {
                    constructor_node = Some(member);
                }
                continue;
            }
            if let Some(fact) = extract_member_fact(cwd, member, checker) {
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
    }
    (members, constructor_node)
}

fn extract_member_fact(
    cwd: &str,
    member: P<Node>,
    checker: &mut CheckerHandle,
) -> Option<RawMemberFact> {
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
    let is_private = is_private_ecma || mod_flags.intersects(ModifierFlags::Private);

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
        location: node_location_relative(cwd, member),
        has_body,
        is_abstract,
        is_static,
        is_private,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_hierarchy_index_is_send_and_sync_and_lazy_state_defaults_empty() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ProgramHierarchyIndex>();

        let lazy = HierarchyLazyState::default();
        assert!(lazy.nodes_by_symbol.is_empty());
        assert!(lazy.exported_by_sf.is_empty());
    }
}
