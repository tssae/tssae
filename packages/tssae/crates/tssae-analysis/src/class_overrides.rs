use std::collections::VecDeque;

use rustc_hash::{FxHashMap, FxHashSet};
use tssae_common::{
    ClassMemberFact, ClassOrInterfaceKind, DiagnosticLocation, GapItem, MemberTargetHierarchy,
    SymbolId, SyntaxId, TypeHierarchyNode, UnknownReason,
};

/// Requested member access flavor for override resolution.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemberAccessKind {
    MethodOrProp,
    Getter,
    Setter,
}

/// One resolved candidate member implementation produced by [`resolve_member_overrides`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedMemberOverride {
    pub class_syntax: SyntaxId,
    pub class_symbol: SymbolId,
    pub class_name: Option<String>,
    pub member_syntax: SyntaxId,
    pub member_symbol: Option<SymbolId>,
    pub target_syntax: SyntaxId,
    pub location: Option<DiagnosticLocation>,
    pub has_body: bool,
    pub is_external: bool,
    pub is_override: bool,
}

/// Outcome of resolving polymorphic method/accessor overrides over boundary hierarchy facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassOverrideResolution {
    pub targets: Vec<ResolvedMemberOverride>,
    pub is_complete: bool,
    pub gaps: Vec<GapItem>,
}

/// Resolves all candidate member implementations for a static receiver hierarchy and determines
/// whether the target set is closed (`is_complete: true`) or open to external extension/implementation.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "Unified override collection, ancestor inheritance fallback, and closed/open completeness judgment."
)]
pub fn resolve_member_overrides(
    hierarchy: &MemberTargetHierarchy,
    member_name: &str,
    access_kind: MemberAccessKind,
    call_location: Option<&DiagnosticLocation>,
    exact_instance_only: bool,
) -> ClassOverrideResolution {
    let mut by_symbol: FxHashMap<SymbolId, &TypeHierarchyNode> = FxHashMap::default();
    let mut children_by_base: FxHashMap<SymbolId, Vec<SymbolId>> = FxHashMap::default();

    for node in &hierarchy.candidate_classes {
        by_symbol.insert(node.symbol, node);
        for &base in &node.base_symbols {
            children_by_base.entry(base).or_default().push(node.symbol);
        }
    }

    let mut target_classes: Vec<SymbolId> = Vec::new();
    let mut seen_classes: FxHashSet<SymbolId> = FxHashSet::default();
    let mut queue: VecDeque<SymbolId> = VecDeque::new();

    for &root_sym in &hierarchy.receiver_symbols {
        if seen_classes.insert(root_sym) {
            queue.push_back(root_sym);
        }
    }

    while let Some(sym) = queue.pop_front() {
        target_classes.push(sym);
        if !exact_instance_only && let Some(children) = children_by_base.get(&sym) {
            for &child in children {
                if seen_classes.insert(child) {
                    queue.push_back(child);
                }
            }
        }
    }

    let mut targets: Vec<ResolvedMemberOverride> = Vec::new();
    let mut seen_target_syntax: FxHashSet<SyntaxId> = FxHashSet::default();
    let mut any_exported_or_external =
        hierarchy.receiver_is_external || (hierarchy.receiver_is_exported && !exact_instance_only);

    for class_sym in &target_classes {
        let Some(class_node) = by_symbol.get(class_sym).copied() else {
            continue;
        };
        if class_node.kind == ClassOrInterfaceKind::Class
            && !exact_instance_only
            && (class_node.is_exported || class_node.is_external)
        {
            any_exported_or_external = true;
        }

        if class_node.kind != ClassOrInterfaceKind::Class {
            continue;
        }

        if let Some(member) = find_concrete_member(class_node, member_name, access_kind) {
            push_member_override(class_node, member, &mut seen_target_syntax, &mut targets);
        } else if !class_node.is_abstract {
            // Concrete class inherits implementation from nearest non-abstract ancestor.
            if let Some((anc_node, anc_member)) =
                find_inherited_member(class_node, member_name, access_kind, &by_symbol)
            {
                if anc_node.is_exported || anc_node.is_external {
                    any_exported_or_external = !exact_instance_only && any_exported_or_external;
                }
                push_member_override(anc_node, anc_member, &mut seen_target_syntax, &mut targets);
            }
        }
    }

    let mut gaps = Vec::new();
    if hierarchy.has_unresolved_type {
        gaps.push(GapItem::from_reason(
            UnknownReason::UnresolvedType,
            format!("Receiver type for member '{member_name}' is unresolved."),
            call_location.cloned(),
        ));
    }

    let is_complete = if exact_instance_only {
        if hierarchy.has_unresolved_type || targets.is_empty() {
            if gaps.is_empty() {
                gaps.push(GapItem::from_reason(
                    UnknownReason::DynamicCallee,
                    format!(
                        "Could not resolve concrete implementation of '{member_name}' on instantiated receiver."
                    ),
                    call_location.cloned(),
                ));
            }
            false
        } else if targets.iter().any(|t| !t.has_body || t.is_external) {
            gaps.push(GapItem::from_reason(
                UnknownReason::MissingBehaviorSummary,
                format!(
                    "Member '{member_name}' on receiver resolves to an external declaration without a behavior summary."
                ),
                call_location.cloned(),
            ));
            false
        } else {
            true
        }
    } else {
        match hierarchy.receiver_kind {
            ClassOrInterfaceKind::Interface => {
                let recv_label = hierarchy.receiver_name.as_deref().unwrap_or("<interface>");
                gaps.push(GapItem::from_reason(
                    UnknownReason::OpenExternalHierarchy,
                    format!(
                        "Interface '{recv_label}' is open and may be implemented by external or unmodeled objects for member '{member_name}'."
                    ),
                    call_location.cloned(),
                ));
                false
            }
            ClassOrInterfaceKind::OtherType => {
                gaps.push(GapItem::from_reason(
                    UnknownReason::DynamicCallee,
                    format!(
                        "Receiver type for '{member_name}' is a structural or dynamic type without a closed class hierarchy."
                    ),
                    call_location.cloned(),
                ));
                false
            }
            ClassOrInterfaceKind::Class => {
                if any_exported_or_external {
                    let recv_label = hierarchy.receiver_name.as_deref().unwrap_or("<class>");
                    gaps.push(GapItem::from_reason(
                        UnknownReason::OpenExternalHierarchy,
                        format!(
                            "Class '{recv_label}' (or a subclass) is exported and may be extended outside the analyzed program for member '{member_name}'."
                        ),
                        call_location.cloned(),
                    ));
                    false
                } else if hierarchy.has_unresolved_type {
                    false
                } else if targets.is_empty() {
                    gaps.push(GapItem::from_reason(
                        UnknownReason::DynamicCallee,
                        format!(
                            "No concrete implementation of '{member_name}' was found in the class hierarchy."
                        ),
                        call_location.cloned(),
                    ));
                    false
                } else if targets.iter().any(|t| !t.has_body || t.is_external) {
                    gaps.push(GapItem::from_reason(
                        UnknownReason::MissingBehaviorSummary,
                        format!(
                            "Member '{member_name}' resolves to an external declaration without a behavior summary."
                        ),
                        call_location.cloned(),
                    ));
                    false
                } else {
                    true
                }
            }
        }
    };

    ClassOverrideResolution {
        targets,
        is_complete,
        gaps,
    }
}

fn find_concrete_member<'a>(
    class_node: &'a TypeHierarchyNode,
    member_name: &str,
    access_kind: MemberAccessKind,
) -> Option<&'a ClassMemberFact> {
    class_node.members.iter().find(|m| {
        !m.is_static
            && m.name == member_name
            && !m.is_abstract
            && matches_access_kind(m, access_kind)
            && (m.has_body || m.callable_syntax.is_some())
    })
}

fn matches_access_kind(member: &ClassMemberFact, access_kind: MemberAccessKind) -> bool {
    match access_kind {
        MemberAccessKind::Getter => {
            member.kind == "GetAccessor" || member.kind == "MethodDeclaration"
        }
        MemberAccessKind::Setter => member.kind == "SetAccessor",
        MemberAccessKind::MethodOrProp => matches!(
            member.kind.as_str(),
            "MethodDeclaration" | "PropertyDeclaration" | "GetAccessor"
        ),
    }
}

fn find_inherited_member<'a>(
    start_class: &'a TypeHierarchyNode,
    member_name: &str,
    access_kind: MemberAccessKind,
    by_symbol: &FxHashMap<SymbolId, &'a TypeHierarchyNode>,
) -> Option<(&'a TypeHierarchyNode, &'a ClassMemberFact)> {
    let mut visited = FxHashSet::default();
    let mut queue = VecDeque::new();
    for &base in &start_class.base_symbols {
        if visited.insert(base) {
            queue.push_back(base);
        }
    }
    while let Some(sym) = queue.pop_front() {
        let Some(node) = by_symbol.get(&sym).copied() else {
            continue;
        };
        if node.kind == ClassOrInterfaceKind::Class
            && let Some(member) = find_concrete_member(node, member_name, access_kind)
        {
            return Some((node, member));
        }
        for &base in &node.base_symbols {
            if visited.insert(base) {
                queue.push_back(base);
            }
        }
    }
    None
}

fn push_member_override(
    class_node: &TypeHierarchyNode,
    member: &ClassMemberFact,
    seen_target_syntax: &mut FxHashSet<SyntaxId>,
    targets: &mut Vec<ResolvedMemberOverride>,
) {
    let target_syntax = member.callable_syntax.unwrap_or(member.member_syntax);
    if !seen_target_syntax.insert(target_syntax) {
        return;
    }
    targets.push(ResolvedMemberOverride {
        class_syntax: class_node.syntax,
        class_symbol: class_node.symbol,
        class_name: class_node.name.clone(),
        member_syntax: member.member_syntax,
        member_symbol: member.symbol,
        target_syntax,
        location: member.location.clone(),
        has_body: member.has_body || member.callable_syntax.is_some(),
        is_external: class_node.is_external,
        is_override: member.is_override,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_method(syn: u64, name: &str, is_abstract: bool, is_override: bool) -> ClassMemberFact {
        ClassMemberFact {
            name: name.to_string(),
            member_syntax: SyntaxId(syn),
            symbol: Some(SymbolId(syn)),
            kind: "MethodDeclaration".to_string(),
            location: None,
            has_body: !is_abstract,
            is_abstract,
            is_static: false,
            is_private_ecma: false,
            is_override,
            initializer_syntax: None,
            callable_syntax: None,
        }
    }

    #[test]
    fn closed_base_and_subclass_overrides_are_complete_and_include_both() {
        let base_sym = SymbolId(1);
        let sub_sym = SymbolId(2);
        let hierarchy = MemberTargetHierarchy {
            receiver_kind: ClassOrInterfaceKind::Class,
            receiver_symbols: vec![base_sym],
            receiver_name: Some("Base".to_string()),
            receiver_is_exported: false,
            receiver_is_abstract: false,
            receiver_is_external: false,
            is_union_or_intersection: false,
            has_unresolved_type: false,
            candidate_classes: vec![
                TypeHierarchyNode {
                    syntax: SyntaxId(10),
                    symbol: base_sym,
                    name: Some("Base".to_string()),
                    kind: ClassOrInterfaceKind::Class,
                    is_exported: false,
                    is_abstract: false,
                    is_external: false,
                    base_symbols: Vec::new(),
                    members: vec![mk_method(11, "run", false, false)],
                    constructor_syntax: None,
                },
                TypeHierarchyNode {
                    syntax: SyntaxId(20),
                    symbol: sub_sym,
                    name: Some("Sub".to_string()),
                    kind: ClassOrInterfaceKind::Class,
                    is_exported: false,
                    is_abstract: false,
                    is_external: false,
                    base_symbols: vec![base_sym],
                    members: vec![mk_method(21, "run", false, true)],
                    constructor_syntax: None,
                },
            ],
        };

        let res = resolve_member_overrides(
            &hierarchy,
            "run",
            MemberAccessKind::MethodOrProp,
            None,
            false,
        );
        assert!(res.is_complete);
        assert_eq!(res.gaps.len(), 0);
        assert_eq!(res.targets.len(), 2);
        assert_eq!(res.targets[0].target_syntax, SyntaxId(11));
        assert_eq!(res.targets[1].target_syntax, SyntaxId(21));
    }

    #[test]
    fn exported_base_class_is_incomplete_with_open_hierarchy_gap() {
        let base_sym = SymbolId(1);
        let hierarchy = MemberTargetHierarchy {
            receiver_kind: ClassOrInterfaceKind::Class,
            receiver_symbols: vec![base_sym],
            receiver_name: Some("ExportedBase".to_string()),
            receiver_is_exported: true,
            receiver_is_abstract: false,
            receiver_is_external: false,
            is_union_or_intersection: false,
            has_unresolved_type: false,
            candidate_classes: vec![TypeHierarchyNode {
                syntax: SyntaxId(10),
                symbol: base_sym,
                name: Some("ExportedBase".to_string()),
                kind: ClassOrInterfaceKind::Class,
                is_exported: true,
                is_abstract: false,
                is_external: false,
                base_symbols: Vec::new(),
                members: vec![mk_method(11, "run", false, false)],
                constructor_syntax: None,
            }],
        };

        let res = resolve_member_overrides(
            &hierarchy,
            "run",
            MemberAccessKind::MethodOrProp,
            None,
            false,
        );
        assert!(!res.is_complete);
        assert_eq!(res.targets.len(), 1);
        assert_eq!(res.gaps.len(), 1);
        assert_eq!(res.gaps[0].kind, "open-external-hierarchy");
    }

    #[test]
    fn exact_instance_of_exported_class_is_closed_and_complete() {
        let base_sym = SymbolId(1);
        let sub_sym = SymbolId(2);
        let hierarchy = MemberTargetHierarchy {
            receiver_kind: ClassOrInterfaceKind::Class,
            receiver_symbols: vec![base_sym],
            receiver_name: Some("ExportedBase".to_string()),
            receiver_is_exported: true,
            receiver_is_abstract: false,
            receiver_is_external: false,
            is_union_or_intersection: false,
            has_unresolved_type: false,
            candidate_classes: vec![
                TypeHierarchyNode {
                    syntax: SyntaxId(10),
                    symbol: base_sym,
                    name: Some("ExportedBase".to_string()),
                    kind: ClassOrInterfaceKind::Class,
                    is_exported: true,
                    is_abstract: false,
                    is_external: false,
                    base_symbols: Vec::new(),
                    members: vec![mk_method(11, "run", false, false)],
                    constructor_syntax: None,
                },
                TypeHierarchyNode {
                    syntax: SyntaxId(20),
                    symbol: sub_sym,
                    name: Some("Sub".to_string()),
                    kind: ClassOrInterfaceKind::Class,
                    is_exported: true,
                    is_abstract: false,
                    is_external: false,
                    base_symbols: vec![base_sym],
                    members: vec![mk_method(21, "run", false, true)],
                    constructor_syntax: None,
                },
            ],
        };

        let res = resolve_member_overrides(
            &hierarchy,
            "run",
            MemberAccessKind::MethodOrProp,
            None,
            true,
        );
        assert!(res.is_complete);
        assert_eq!(res.gaps.len(), 0);
        assert_eq!(res.targets.len(), 1);
        assert_eq!(res.targets[0].target_syntax, SyntaxId(11));
    }
}
