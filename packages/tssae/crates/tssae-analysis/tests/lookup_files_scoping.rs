use std::{cell::RefCell, collections::HashSet, sync::Arc};

use rustc_hash::{FxHashMap, FxHashSet};
use tssae_analysis::{
    instance_cleanup::{
        CleanupScopeSpec, CleanupTargetSpec, TargetCleanupStatusData,
        evaluate_instance_cleanup_on_ir,
    },
    instance_identity::{InstanceResolutionOutcome, InstanceTargetSpec, resolve_instances_on_ir},
    ir::{
        AssignOp, BindingMutability, CfgVertex, DeclarationSemantics, ExplicitEdge,
        ExplicitEdgeKind, FunctionIr, IrCallArg, IrCallKind, IrLocalBinding, IrOp, IrParameter,
        IrPropKey, IrProvider, IrScopeKind, LiteralKind, ScopeCatalogEntry,
    },
    summaries::BehaviorSummaryIndex,
    value_propagation::{
        ValueEndpointSpec, ValuePropagationTargetOutcome, trace_value_propagation,
    },
};
use tssae_common::{
    BehaviorSummaryEntry, BehaviorSummaryPack, DeclarationIdentity, DeclarationSummaryData,
    DiagnosticLocation, EffectAspectSummary, FileId, FunctionIrId, GapItem, ImplementationFact,
    ImplementationSourceKind, InstanceAspectSummary, InstanceOutcome, LocalBindingId,
    LocalSyntaxId, MemberTargetHierarchy, ResolvedImplementationFact, ResourceOpSpec,
    SemanticModel, SignatureId, SummaryCompleteness, SummaryConfigPayload, SummarySlotRef,
    SummarySource, SummaryTargetMatcher, SymbolId, SyntaxCatalog, SyntaxId, TaggedSummaryPack,
    TypeId, TypeSummaryData, UnknownReason, ValueId, VertexId,
};

#[derive(Default)]
struct ScopedTestEnv {
    scopes_by_file: FxHashMap<FileId, Vec<ScopeCatalogEntry>>,
    irs: FxHashMap<SyntaxId, Arc<FunctionIr>>,
    enclosing: FxHashMap<SyntaxId, SyntaxId>,
    locations: FxHashMap<SyntaxId, DiagnosticLocation>,
    texts: FxHashMap<SyntaxId, String>,
    kinds: FxHashMap<SyntaxId, String>,
    symbols: FxHashMap<SyntaxId, SymbolId>,
    decls: FxHashMap<SyntaxId, DeclarationSummaryData>,
    decls_by_symbol: FxHashMap<SymbolId, DeclarationSummaryData>,
    identities: FxHashMap<SyntaxId, DeclarationIdentity>,
    file_paths: FxHashMap<FileId, String>,
    scopes_in_file_calls: RefCell<Vec<FileId>>,
    function_ir_calls: RefCell<Vec<FileId>>,
    file_scope_ir_calls: RefCell<Vec<FileId>>,
}

impl ScopedTestEnv {
    fn reset_counters(&self) {
        self.scopes_in_file_calls.borrow_mut().clear();
        self.function_ir_calls.borrow_mut().clear();
        self.file_scope_ir_calls.borrow_mut().clear();
    }

    fn assert_no_bg_file_touched(&self, bg_files: &[FileId]) {
        let bg_set: FxHashSet<FileId> = bg_files.iter().copied().collect();
        for f in self.scopes_in_file_calls.borrow().iter() {
            assert!(
                !bg_set.contains(f),
                "IrProvider::scopes_in_file called on background file {f:?}"
            );
        }
        for f in self.function_ir_calls.borrow().iter() {
            assert!(
                !bg_set.contains(f),
                "IrProvider::function_ir called on background file {f:?}"
            );
        }
        for f in self.file_scope_ir_calls.borrow().iter() {
            assert!(
                !bg_set.contains(f),
                "IrProvider::file_scope_ir called on background file {f:?}"
            );
        }
    }
}

impl SyntaxCatalog for ScopedTestEnv {
    fn syntax_location(&self, syntax: SyntaxId) -> Option<DiagnosticLocation> {
        self.locations.get(&syntax).cloned()
    }

    fn syntax_text(&self, syntax: SyntaxId) -> Option<String> {
        self.texts.get(&syntax).cloned()
    }

    fn syntax_kind(&self, syntax: SyntaxId) -> Option<String> {
        self.kinds.get(&syntax).cloned()
    }

    fn file_path(&self, file: FileId) -> Option<&str> {
        self.file_paths.get(&file).map(String::as_str)
    }
}

impl SemanticModel for ScopedTestEnv {
    fn symbol_at_syntax(&self, syntax: SyntaxId) -> Option<SymbolId> {
        self.symbols.get(&syntax).copied()
    }

    fn type_at_syntax(&self, _syntax: SyntaxId) -> Option<TypeSummaryData> {
        None
    }

    fn signature_at_syntax(&self, _syntax: SyntaxId) -> Option<SignatureId> {
        None
    }

    fn resolve_declaration(&self, syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        self.decls.get(&syntax).cloned()
    }

    fn resolve_declaration_by_symbol(&self, symbol: SymbolId) -> Option<DeclarationSummaryData> {
        self.decls_by_symbol.get(&symbol).cloned()
    }

    fn is_assignable_to(&self, _source: TypeId, _target: TypeId) -> bool {
        false
    }

    fn base_types(&self, _ty: TypeId) -> Vec<TypeId> {
        Vec::new()
    }

    fn is_external_syntax(&self, syntax: SyntaxId) -> bool {
        self.decls.get(&syntax).is_some_and(|d| d.is_external)
    }

    fn member_target_hierarchy(
        &self,
        _receiver_syntax: SyntaxId,
        _member_name: Option<&str>,
        _static_receiver: bool,
    ) -> Option<MemberTargetHierarchy> {
        None
    }

    fn class_hierarchy_for_symbol(&self, _class_symbol: SymbolId) -> Option<MemberTargetHierarchy> {
        None
    }

    fn resolve_class_constructor(&self, _class_syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        None
    }

    fn resolve_super_target(&self, _call_syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        None
    }

    fn resolve_implementation_for_syntax(
        &self,
        decl_or_node_syntax: SyntaxId,
        _call_site_syntax: Option<SyntaxId>,
    ) -> ImplementationFact {
        let decl = self.decls.get(&decl_or_node_syntax);
        if decl.is_some_and(|d| d.is_external) {
            return ImplementationFact::External {
                package_name: None,
                declaration_file: None,
                evidence: Vec::new(),
            };
        }
        let file = self
            .file_paths
            .get(&decl_or_node_syntax.file_id())
            .cloned()
            .unwrap_or_else(|| "src/entry.ts".to_string());
        ImplementationFact::Resolved(ResolvedImplementationFact {
            file: file.clone(),
            location: self
                .locations
                .get(&decl_or_node_syntax)
                .cloned()
                .unwrap_or_else(|| loc(&file, 0)),
            declaration_syntax: decl_or_node_syntax,
            declaration_symbol: decl.and_then(|d| d.symbol),
            fn_syntax: Some(decl_or_node_syntax),
            export_name: decl.and_then(|d| d.name.clone()),
            package_name: None,
            resolution_source: ImplementationSourceKind::CurrentProject,
            declaration_file: None,
        })
    }

    fn check_call_missing_package_gap(&self, _call_syntax: SyntaxId) -> Option<GapItem> {
        None
    }

    fn declaration_identity(&self, syntax: SyntaxId) -> Option<DeclarationIdentity> {
        self.identities.get(&syntax).cloned()
    }
}

impl IrProvider for ScopedTestEnv {
    fn function_ir(&self, scope_syntax: SyntaxId) -> Result<Arc<FunctionIr>, GapItem> {
        self.function_ir_calls
            .borrow_mut()
            .push(scope_syntax.file_id());
        self.irs.get(&scope_syntax).cloned().ok_or_else(|| {
            GapItem::from_reason(
                UnknownReason::MissingSourceFile,
                "missing ir".to_string(),
                None,
            )
        })
    }

    fn enclosing_scope_ir(&self, node_syntax: SyntaxId) -> Result<Arc<FunctionIr>, GapItem> {
        let scope_syn = self.enclosing.get(&node_syntax).copied().ok_or_else(|| {
            GapItem::from_reason(
                UnknownReason::NoEnclosingFunction,
                "no enclosing".to_string(),
                None,
            )
        })?;
        self.function_ir(scope_syn)
    }

    fn file_scope_ir(&self, file: FileId) -> Option<Arc<FunctionIr>> {
        self.file_scope_ir_calls.borrow_mut().push(file);
        let root_syn = self
            .scopes_by_file
            .get(&file)?
            .iter()
            .find(|s| s.kind == IrScopeKind::ModuleTopLevel)?
            .root_syntax;
        self.function_ir(root_syn).ok()
    }

    fn scopes_in_file(&self, file: FileId) -> Vec<ScopeCatalogEntry> {
        self.scopes_in_file_calls.borrow_mut().push(file);
        self.scopes_by_file.get(&file).cloned().unwrap_or_default()
    }
}

const fn syn(file: u32, local: u32) -> SyntaxId {
    SyntaxId::from_parts(FileId(file), LocalSyntaxId(local))
}

const fn scope_entry(root: SyntaxId, kind: IrScopeKind, start: u32, end: u32) -> ScopeCatalogEntry {
    ScopeCatalogEntry {
        id: FunctionIrId {
            file: root.file_id(),
            local_scope: root.local_id().0,
        },
        local_syntax: root.local_id(),
        root_syntax: root,
        kind,
        span_start: start,
        span_end: end,
    }
}

fn loc(file: &str, pos: u32) -> DiagnosticLocation {
    DiagnosticLocation {
        file: file.to_string(),
        line: 1,
        column: pos + 1,
        end_line: 1,
        end_column: pos + 6,
        start: pos,
        end: pos + 5,
    }
}

fn mk_vertex(
    id: u32,
    syntax: Option<SyntaxId>,
    file_name: &str,
    value: Option<ValueId>,
    op: IrOp,
    edges: Vec<ExplicitEdge>,
) -> CfgVertex {
    CfgVertex {
        id: VertexId(id),
        syntax,
        local_syntax: syntax.map(SyntaxId::local_id),
        symbol: None,
        ty: None,
        signature: None,
        location: syntax.map(|s| loc(file_name, s.local_id().0 * 10)),
        value,
        op,
        explicit_edges: edges,
        may_throw_edge: None,
        enclosing_finallies: Vec::new(),
    }
}

fn build_summaries() -> BehaviorSummaryIndex {
    let pack = BehaviorSummaryPack {
        id: "test-pool-pack".to_string(),
        version: None,
        description: None,
        entries: vec![
            BehaviorSummaryEntry {
                id: Some("acquire".to_string()),
                matcher: SummaryTargetMatcher {
                    package: None,
                    version: None,
                    export_path: None,
                    module: Some("src/pool.ts".to_string()),
                    member_path: vec!["acquire".to_string()],
                },
                description: None,
                calls: None,
                values: None,
                instances: Some(InstanceAspectSummary {
                    completeness: SummaryCompleteness::Complete,
                    outcome: InstanceOutcome::AllocatesFreshInstance,
                }),
                effects: None,
            },
            BehaviorSummaryEntry {
                id: Some("release".to_string()),
                matcher: SummaryTargetMatcher {
                    package: None,
                    version: None,
                    export_path: None,
                    module: Some("src/entry.ts".to_string()),
                    member_path: vec!["release".to_string()],
                },
                description: None,
                calls: None,
                values: None,
                instances: None,
                effects: Some(EffectAspectSummary {
                    completeness: SummaryCompleteness::Complete,
                    resource_ops: vec![ResourceOpSpec::DisposesTarget {
                        target: SummarySlotRef::Arg { arg_index: 0 },
                    }],
                    mutates_receiver: None,
                }),
            },
        ],
    };
    BehaviorSummaryIndex::from_config(
        &SummaryConfigPayload {
            packs: vec![TaggedSummaryPack {
                source: SummarySource::Local,
                origin_file: None,
                pack,
            }],
            enable_builtin: false,
        },
        &FxHashMap::default(),
    )
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Tests lookupFiles scoping across resolve_instances_on_ir, trace_value_propagation, and evaluate_instance_cleanup_on_ir with 50 background files."
)]
fn lookup_files_never_scans_background_files() {
    let mut env = ScopedTestEnv::default();

    let entry_file = FileId(0);
    let pool_file = FileId(1);
    let mut project_files = vec![entry_file, pool_file];
    let mut bg_files = Vec::new();
    for i in 0..50u32 {
        let fid = FileId(2 + i);
        let path = format!("src/bg_{i}.ts");
        let bg_mod_syn = syn(fid.0, 1);
        let bg_fn_syn = syn(fid.0, 2);
        env.file_paths.insert(fid, path.clone());
        env.locations.insert(bg_mod_syn, loc(&path, 0));
        env.locations.insert(bg_fn_syn, loc(&path, 10));
        env.texts.insert(bg_fn_syn, "function bg() {}".to_string());
        env.scopes_by_file.insert(
            fid,
            vec![
                scope_entry(bg_mod_syn, IrScopeKind::ModuleTopLevel, 0, 100),
                scope_entry(bg_fn_syn, IrScopeKind::Function, 10, 90),
            ],
        );
        project_files.push(fid);
        bg_files.push(fid);
    }

    env.file_paths
        .insert(entry_file, "src/entry.ts".to_string());
    env.file_paths.insert(pool_file, "src/pool.ts".to_string());

    // pool.ts:
    // - module-level `export const sharedToken = "secret";`
    // - `export function attachConn(h) { h.conn = acquire(); }`
    let pool_mod_syn = syn(pool_file.0, 1);
    let pool_token_decl = syn(pool_file.0, 2);
    let pool_token_lit = syn(pool_file.0, 3);
    let pool_attach_fn = syn(pool_file.0, 4);
    let pool_h_param = syn(pool_file.0, 5);
    let pool_h_read = syn(pool_file.0, 6);
    let pool_acq_callee = syn(pool_file.0, 7);
    let pool_acq_call = syn(pool_file.0, 8);
    let pool_prop_write = syn(pool_file.0, 9);
    let pool_token_sym = SymbolId(100);
    let pool_attach_sym = SymbolId(101);
    let acq_sym = SymbolId(102);

    for s in [
        pool_mod_syn,
        pool_token_decl,
        pool_token_lit,
        pool_attach_fn,
        pool_h_param,
        pool_h_read,
        pool_acq_callee,
        pool_acq_call,
        pool_prop_write,
    ] {
        env.locations
            .insert(s, loc("src/pool.ts", s.local_id().0 * 10));
    }
    for s in [pool_token_decl, pool_token_lit] {
        env.enclosing.insert(s, pool_mod_syn);
    }
    for s in [
        pool_h_param,
        pool_h_read,
        pool_acq_callee,
        pool_acq_call,
        pool_prop_write,
    ] {
        env.enclosing.insert(s, pool_attach_fn);
    }
    env.symbols.insert(pool_token_decl, pool_token_sym);
    env.symbols.insert(pool_attach_fn, pool_attach_sym);
    env.symbols.insert(pool_acq_callee, acq_sym);
    env.kinds
        .insert(pool_token_decl, "VariableDeclaration".to_string());
    env.decls.insert(
        pool_token_decl,
        DeclarationSummaryData {
            symbol: Some(pool_token_sym),
            primary_syntax: Some(pool_token_decl),
            name: Some("sharedToken".to_string()),
            container_name: None,
            kind: Some("VariableDeclaration".to_string()),
            location: Some(loc("src/pool.ts", 20)),
            is_external: false,
            is_default_library: false,
            has_body: false,
            is_signature_only: false,
            initializer_syntax: Some(pool_token_lit),
            callable_syntax: None,
        },
    );
    env.decls.insert(
        pool_acq_callee,
        DeclarationSummaryData {
            symbol: Some(acq_sym),
            primary_syntax: Some(pool_acq_callee),
            name: Some("acquire".to_string()),
            container_name: None,
            kind: Some("FunctionDeclaration".to_string()),
            location: Some(loc("src/pool.ts", 70)),
            is_external: true,
            is_default_library: false,
            has_body: false,
            is_signature_only: false,
            initializer_syntax: None,
            callable_syntax: None,
        },
    );
    env.identities.insert(
        pool_acq_call,
        DeclarationIdentity {
            package_name: None,
            package_version: None,
            export_path: None,
            module_path: Some("src/pool.ts".to_string()),
            is_default_library: false,
            member_paths: vec![vec!["acquire".to_string()]],
            is_instance_member: false,
        },
    );
    env.identities.insert(
        pool_acq_callee,
        env.identities.get(&pool_acq_call).unwrap().clone(),
    );
    env.decls.insert(
        pool_attach_fn,
        DeclarationSummaryData {
            symbol: Some(pool_attach_sym),
            primary_syntax: Some(pool_attach_fn),
            name: Some("attachConn".to_string()),
            container_name: None,
            kind: Some("FunctionDeclaration".to_string()),
            location: Some(loc("src/pool.ts", 40)),
            is_external: false,
            is_default_library: false,
            has_body: true,
            is_signature_only: false,
            initializer_syntax: None,
            callable_syntax: Some(pool_attach_fn),
        },
    );
    env.decls_by_symbol.insert(
        pool_attach_sym,
        env.decls.get(&pool_attach_fn).unwrap().clone(),
    );

    let mut pool_mod_expr_to_val = FxHashMap::default();
    pool_mod_expr_to_val.insert(pool_token_lit.local_id(), ValueId(0));
    let pool_mod_ir = Arc::new(FunctionIr {
        id: FunctionIrId {
            file: pool_file,
            local_scope: 1,
        },
        root_syntax: pool_mod_syn,
        scope_kind: IrScopeKind::ModuleTopLevel,
        scope_location: loc("src/pool.ts", 0),
        decl_semantics: DeclarationSemantics::default(),
        type_guard: None,
        parameters: Vec::new(),
        bindings: vec![IrLocalBinding {
            id: LocalBindingId(0),
            name: "sharedToken".to_string(),
            mutability: BindingMutability::Const,
            syntax: pool_token_decl,
            symbol: Some(pool_token_sym),
        }],
        captured_bindings: Vec::new(),
        vertices: vec![
            mk_vertex(
                0,
                None,
                "src/pool.ts",
                None,
                IrOp::Entry,
                vec![ExplicitEdge {
                    target: VertexId(4),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                1,
                None,
                "src/pool.ts",
                None,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(3),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(2, None, "src/pool.ts", None, IrOp::NopMerge, vec![]),
            mk_vertex(3, None, "src/pool.ts", None, IrOp::Exit, vec![]),
            mk_vertex(
                4,
                Some(pool_token_lit),
                "src/pool.ts",
                Some(ValueId(0)),
                IrOp::Literal {
                    kind: LiteralKind::String("secret".to_string()),
                    as_const: false,
                },
                vec![ExplicitEdge {
                    target: VertexId(5),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                5,
                Some(pool_token_decl),
                "src/pool.ts",
                Some(ValueId(1)),
                IrOp::AssignLocal {
                    target: LocalBindingId(0),
                    op: AssignOp::Assign,
                    value: ValueId(0),
                    is_init: true,
                },
                vec![ExplicitEdge {
                    target: VertexId(1),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
        ],
        entry: VertexId(0),
        normal_exit: VertexId(1),
        exceptional_exit: VertexId(2),
        exit: VertexId(3),
        syntax_to_vertex: FxHashMap::default(),
        expr_to_vertex: FxHashMap::default(),
        expr_to_value: pool_mod_expr_to_val,
        value_sources: FxHashMap::default(),
        branches: Vec::new(),
        value_count: 2,
    });

    let mut pool_attach_expr_to_val = FxHashMap::default();
    pool_attach_expr_to_val.insert(pool_h_read.local_id(), ValueId(0));
    pool_attach_expr_to_val.insert(pool_acq_call.local_id(), ValueId(2));
    let pool_attach_ir = Arc::new(FunctionIr {
        id: FunctionIrId {
            file: pool_file,
            local_scope: 4,
        },
        root_syntax: pool_attach_fn,
        scope_kind: IrScopeKind::Function,
        scope_location: loc("src/pool.ts", 40),
        decl_semantics: DeclarationSemantics {
            is_exported: true,
            ..DeclarationSemantics::default()
        },
        type_guard: None,
        parameters: vec![IrParameter {
            name: "h".to_string(),
            binding: Some(LocalBindingId(0)),
            raw_value: None,
            syntax: pool_h_param,
            has_default: false,
            is_rest: false,
        }],
        bindings: vec![IrLocalBinding {
            id: LocalBindingId(0),
            name: "h".to_string(),
            mutability: BindingMutability::Parameter,
            syntax: pool_h_param,
            symbol: None,
        }],
        captured_bindings: Vec::new(),
        vertices: vec![
            mk_vertex(
                0,
                None,
                "src/pool.ts",
                None,
                IrOp::Entry,
                vec![ExplicitEdge {
                    target: VertexId(4),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                1,
                None,
                "src/pool.ts",
                None,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(3),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(2, None, "src/pool.ts", None, IrOp::NopMerge, vec![]),
            mk_vertex(3, None, "src/pool.ts", None, IrOp::Exit, vec![]),
            mk_vertex(
                4,
                Some(pool_h_read),
                "src/pool.ts",
                Some(ValueId(0)),
                IrOp::ReadLocal {
                    binding: LocalBindingId(0),
                },
                vec![ExplicitEdge {
                    target: VertexId(5),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                5,
                Some(pool_acq_callee),
                "src/pool.ts",
                Some(ValueId(1)),
                IrOp::ReadSymbol {
                    ident_syntax: pool_acq_callee,
                },
                vec![ExplicitEdge {
                    target: VertexId(6),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                6,
                Some(pool_acq_call),
                "src/pool.ts",
                Some(ValueId(2)),
                IrOp::Call {
                    call_kind: IrCallKind::Explicit,
                    callee: ValueId(1),
                    receiver: None,
                    args: Vec::new(),
                },
                vec![ExplicitEdge {
                    target: VertexId(7),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                7,
                Some(pool_prop_write),
                "src/pool.ts",
                None,
                IrOp::PropWrite {
                    object: ValueId(0),
                    key: IrPropKey::Named("conn".to_string()),
                    op: AssignOp::Assign,
                    value: ValueId(2),
                    is_super: false,
                    implicit_call_kind: None,
                },
                vec![ExplicitEdge {
                    target: VertexId(1),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
        ],
        entry: VertexId(0),
        normal_exit: VertexId(1),
        exceptional_exit: VertexId(2),
        exit: VertexId(3),
        syntax_to_vertex: FxHashMap::default(),
        expr_to_vertex: FxHashMap::default(),
        expr_to_value: pool_attach_expr_to_val,
        value_sources: FxHashMap::default(),
        branches: Vec::new(),
        value_count: 3,
    });

    env.irs.insert(pool_mod_syn, pool_mod_ir);
    env.irs.insert(pool_attach_fn, pool_attach_ir);
    env.scopes_by_file.insert(
        pool_file,
        vec![
            scope_entry(pool_mod_syn, IrScopeKind::ModuleTopLevel, 0, 30),
            scope_entry(pool_attach_fn, IrScopeKind::Function, 35, 200),
        ],
    );

    // entry.ts:
    // - `useToken()` reads `sharedToken` into `sink`
    // - `useConn(p)` reads `p.conn` and passes it to `release(p.conn)`
    // - `caller()` allocates `const h = {}; attachConn(h); useConn(h);`
    let entry_mod_syn = syn(entry_file.0, 1);
    let entry_use_token_fn = syn(entry_file.0, 2);
    let entry_read_token = syn(entry_file.0, 3);
    let entry_sink_decl = syn(entry_file.0, 4);
    let entry_use_conn_fn = syn(entry_file.0, 5);
    let entry_p_param = syn(entry_file.0, 6);
    let entry_p_read = syn(entry_file.0, 7);
    let entry_conn_read = syn(entry_file.0, 8);
    let entry_rel_callee = syn(entry_file.0, 9);
    let entry_rel_call = syn(entry_file.0, 10);
    let entry_caller_fn = syn(entry_file.0, 11);
    let entry_h_alloc = syn(entry_file.0, 12);
    let entry_h_decl = syn(entry_file.0, 13);
    let entry_attach_callee = syn(entry_file.0, 14);
    let entry_attach_arg = syn(entry_file.0, 15);
    let entry_attach_call = syn(entry_file.0, 16);
    let entry_use_conn_callee = syn(entry_file.0, 17);
    let entry_use_conn_arg = syn(entry_file.0, 18);
    let entry_use_conn_call = syn(entry_file.0, 19);
    let entry_use_conn_sym = SymbolId(200);
    let rel_sym = SymbolId(201);

    for s in [
        entry_mod_syn,
        entry_use_token_fn,
        entry_read_token,
        entry_sink_decl,
        entry_use_conn_fn,
        entry_p_param,
        entry_p_read,
        entry_conn_read,
        entry_rel_callee,
        entry_rel_call,
        entry_caller_fn,
        entry_h_alloc,
        entry_h_decl,
        entry_attach_callee,
        entry_attach_arg,
        entry_attach_call,
        entry_use_conn_callee,
        entry_use_conn_arg,
        entry_use_conn_call,
    ] {
        env.locations
            .insert(s, loc("src/entry.ts", s.local_id().0 * 10));
    }
    for s in [entry_read_token, entry_sink_decl] {
        env.enclosing.insert(s, entry_use_token_fn);
    }
    for s in [
        entry_p_param,
        entry_p_read,
        entry_conn_read,
        entry_rel_callee,
        entry_rel_call,
    ] {
        env.enclosing.insert(s, entry_use_conn_fn);
    }
    for s in [
        entry_h_alloc,
        entry_h_decl,
        entry_attach_callee,
        entry_attach_arg,
        entry_attach_call,
        entry_use_conn_callee,
        entry_use_conn_arg,
        entry_use_conn_call,
    ] {
        env.enclosing.insert(s, entry_caller_fn);
    }

    env.texts
        .insert(entry_use_token_fn, "const sink = sharedToken;".to_string());
    env.kinds
        .insert(entry_sink_decl, "VariableDeclaration".to_string());
    env.symbols.insert(entry_read_token, pool_token_sym);
    env.symbols.insert(entry_use_conn_fn, entry_use_conn_sym);
    env.symbols
        .insert(entry_use_conn_callee, entry_use_conn_sym);
    env.symbols.insert(entry_attach_callee, pool_attach_sym);
    env.symbols.insert(entry_rel_callee, rel_sym);

    env.decls.insert(
        entry_read_token,
        env.decls.get(&pool_token_decl).unwrap().clone(),
    );
    env.decls.insert(
        entry_attach_callee,
        env.decls.get(&pool_attach_fn).unwrap().clone(),
    );
    env.decls.insert(
        entry_use_conn_fn,
        DeclarationSummaryData {
            symbol: Some(entry_use_conn_sym),
            primary_syntax: Some(entry_use_conn_fn),
            name: Some("useConn".to_string()),
            container_name: None,
            kind: Some("FunctionDeclaration".to_string()),
            location: Some(loc("src/entry.ts", 50)),
            is_external: false,
            is_default_library: false,
            has_body: true,
            is_signature_only: false,
            initializer_syntax: None,
            callable_syntax: Some(entry_use_conn_fn),
        },
    );
    env.decls.insert(
        entry_use_conn_callee,
        env.decls.get(&entry_use_conn_fn).unwrap().clone(),
    );
    env.decls.insert(
        entry_rel_callee,
        DeclarationSummaryData {
            symbol: Some(rel_sym),
            primary_syntax: Some(entry_rel_callee),
            name: Some("release".to_string()),
            container_name: None,
            kind: Some("FunctionDeclaration".to_string()),
            location: Some(loc("src/entry.ts", 90)),
            is_external: true,
            is_default_library: false,
            has_body: false,
            is_signature_only: false,
            initializer_syntax: None,
            callable_syntax: None,
        },
    );
    env.identities.insert(
        entry_rel_call,
        DeclarationIdentity {
            package_name: None,
            package_version: None,
            export_path: None,
            module_path: Some("src/entry.ts".to_string()),
            is_default_library: false,
            member_paths: vec![vec!["release".to_string()]],
            is_instance_member: false,
        },
    );
    env.identities.insert(
        entry_rel_callee,
        env.identities.get(&entry_rel_call).unwrap().clone(),
    );

    let entry_mod_ir = Arc::new(FunctionIr {
        id: FunctionIrId {
            file: entry_file,
            local_scope: 1,
        },
        root_syntax: entry_mod_syn,
        scope_kind: IrScopeKind::ModuleTopLevel,
        scope_location: loc("src/entry.ts", 0),
        decl_semantics: DeclarationSemantics::default(),
        type_guard: None,
        parameters: Vec::new(),
        bindings: Vec::new(),
        captured_bindings: Vec::new(),
        vertices: vec![
            mk_vertex(
                0,
                None,
                "src/entry.ts",
                None,
                IrOp::Entry,
                vec![ExplicitEdge {
                    target: VertexId(1),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                1,
                None,
                "src/entry.ts",
                None,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(3),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(2, None, "src/entry.ts", None, IrOp::NopMerge, vec![]),
            mk_vertex(3, None, "src/entry.ts", None, IrOp::Exit, vec![]),
        ],
        entry: VertexId(0),
        normal_exit: VertexId(1),
        exceptional_exit: VertexId(2),
        exit: VertexId(3),
        syntax_to_vertex: FxHashMap::default(),
        expr_to_vertex: FxHashMap::default(),
        expr_to_value: FxHashMap::default(),
        value_sources: FxHashMap::default(),
        branches: Vec::new(),
        value_count: 0,
    });

    let mut use_token_expr_to_val = FxHashMap::default();
    use_token_expr_to_val.insert(entry_read_token.local_id(), ValueId(0));
    let entry_use_token_ir = Arc::new(FunctionIr {
        id: FunctionIrId {
            file: entry_file,
            local_scope: 2,
        },
        root_syntax: entry_use_token_fn,
        scope_kind: IrScopeKind::Function,
        scope_location: loc("src/entry.ts", 20),
        decl_semantics: DeclarationSemantics::default(),
        type_guard: None,
        parameters: Vec::new(),
        bindings: vec![IrLocalBinding {
            id: LocalBindingId(0),
            name: "sink".to_string(),
            mutability: BindingMutability::Const,
            syntax: entry_sink_decl,
            symbol: None,
        }],
        captured_bindings: Vec::new(),
        vertices: vec![
            mk_vertex(
                0,
                None,
                "src/entry.ts",
                None,
                IrOp::Entry,
                vec![ExplicitEdge {
                    target: VertexId(4),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                1,
                None,
                "src/entry.ts",
                None,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(3),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(2, None, "src/entry.ts", None, IrOp::NopMerge, vec![]),
            mk_vertex(3, None, "src/entry.ts", None, IrOp::Exit, vec![]),
            mk_vertex(
                4,
                Some(entry_read_token),
                "src/entry.ts",
                Some(ValueId(0)),
                IrOp::ReadSymbol {
                    ident_syntax: entry_read_token,
                },
                vec![ExplicitEdge {
                    target: VertexId(5),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                5,
                Some(entry_sink_decl),
                "src/entry.ts",
                Some(ValueId(1)),
                IrOp::AssignLocal {
                    target: LocalBindingId(0),
                    op: AssignOp::Assign,
                    value: ValueId(0),
                    is_init: true,
                },
                vec![ExplicitEdge {
                    target: VertexId(1),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
        ],
        entry: VertexId(0),
        normal_exit: VertexId(1),
        exceptional_exit: VertexId(2),
        exit: VertexId(3),
        syntax_to_vertex: FxHashMap::default(),
        expr_to_vertex: FxHashMap::default(),
        expr_to_value: use_token_expr_to_val,
        value_sources: FxHashMap::default(),
        branches: Vec::new(),
        value_count: 2,
    });

    let mut use_conn_expr_to_val = FxHashMap::default();
    use_conn_expr_to_val.insert(entry_p_read.local_id(), ValueId(0));
    use_conn_expr_to_val.insert(entry_conn_read.local_id(), ValueId(1));
    let mut use_conn_expr_to_vertex = FxHashMap::default();
    use_conn_expr_to_vertex.insert(entry_conn_read.local_id(), VertexId(5));
    let entry_use_conn_ir = Arc::new(FunctionIr {
        id: FunctionIrId {
            file: entry_file,
            local_scope: 5,
        },
        root_syntax: entry_use_conn_fn,
        scope_kind: IrScopeKind::Function,
        scope_location: loc("src/entry.ts", 50),
        decl_semantics: DeclarationSemantics {
            is_exported: true,
            ..DeclarationSemantics::default()
        },
        type_guard: None,
        parameters: vec![IrParameter {
            name: "p".to_string(),
            binding: Some(LocalBindingId(0)),
            raw_value: None,
            syntax: entry_p_param,
            has_default: false,
            is_rest: false,
        }],
        bindings: vec![IrLocalBinding {
            id: LocalBindingId(0),
            name: "p".to_string(),
            mutability: BindingMutability::Parameter,
            syntax: entry_p_param,
            symbol: None,
        }],
        captured_bindings: Vec::new(),
        vertices: vec![
            mk_vertex(
                0,
                None,
                "src/entry.ts",
                None,
                IrOp::Entry,
                vec![ExplicitEdge {
                    target: VertexId(4),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                1,
                None,
                "src/entry.ts",
                None,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(3),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(2, None, "src/entry.ts", None, IrOp::NopMerge, vec![]),
            mk_vertex(3, None, "src/entry.ts", None, IrOp::Exit, vec![]),
            mk_vertex(
                4,
                Some(entry_p_read),
                "src/entry.ts",
                Some(ValueId(0)),
                IrOp::ReadLocal {
                    binding: LocalBindingId(0),
                },
                vec![ExplicitEdge {
                    target: VertexId(5),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                5,
                Some(entry_conn_read),
                "src/entry.ts",
                Some(ValueId(1)),
                IrOp::PropRead {
                    object: ValueId(0),
                    key: IrPropKey::Named("conn".to_string()),
                    is_super: false,
                    implicit_call_kind: None,
                },
                vec![ExplicitEdge {
                    target: VertexId(6),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                6,
                Some(entry_rel_callee),
                "src/entry.ts",
                Some(ValueId(2)),
                IrOp::ReadSymbol {
                    ident_syntax: entry_rel_callee,
                },
                vec![ExplicitEdge {
                    target: VertexId(7),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                7,
                Some(entry_rel_call),
                "src/entry.ts",
                Some(ValueId(3)),
                IrOp::Call {
                    call_kind: IrCallKind::Explicit,
                    callee: ValueId(2),
                    receiver: None,
                    args: vec![IrCallArg::Positional(ValueId(1))],
                },
                vec![ExplicitEdge {
                    target: VertexId(1),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
        ],
        entry: VertexId(0),
        normal_exit: VertexId(1),
        exceptional_exit: VertexId(2),
        exit: VertexId(3),
        syntax_to_vertex: FxHashMap::default(),
        expr_to_vertex: use_conn_expr_to_vertex,
        expr_to_value: use_conn_expr_to_val,
        value_sources: FxHashMap::default(),
        branches: Vec::new(),
        value_count: 4,
    });

    let mut caller_expr_to_val = FxHashMap::default();
    caller_expr_to_val.insert(entry_h_alloc.local_id(), ValueId(0));
    caller_expr_to_val.insert(entry_attach_arg.local_id(), ValueId(2));
    caller_expr_to_val.insert(entry_attach_call.local_id(), ValueId(3));
    caller_expr_to_val.insert(entry_use_conn_arg.local_id(), ValueId(5));
    caller_expr_to_val.insert(entry_use_conn_call.local_id(), ValueId(6));
    let entry_caller_ir = Arc::new(FunctionIr {
        id: FunctionIrId {
            file: entry_file,
            local_scope: 11,
        },
        root_syntax: entry_caller_fn,
        scope_kind: IrScopeKind::Function,
        scope_location: loc("src/entry.ts", 110),
        decl_semantics: DeclarationSemantics::default(),
        type_guard: None,
        parameters: Vec::new(),
        bindings: vec![IrLocalBinding {
            id: LocalBindingId(0),
            name: "h".to_string(),
            mutability: BindingMutability::Const,
            syntax: entry_h_decl,
            symbol: None,
        }],
        captured_bindings: Vec::new(),
        vertices: vec![
            mk_vertex(
                0,
                None,
                "src/entry.ts",
                None,
                IrOp::Entry,
                vec![ExplicitEdge {
                    target: VertexId(4),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                1,
                None,
                "src/entry.ts",
                None,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(3),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(2, None, "src/entry.ts", None, IrOp::NopMerge, vec![]),
            mk_vertex(3, None, "src/entry.ts", None, IrOp::Exit, vec![]),
            mk_vertex(
                4,
                Some(entry_h_alloc),
                "src/entry.ts",
                Some(ValueId(0)),
                IrOp::ObjectAlloc {
                    props: Vec::new(),
                    as_const: false,
                },
                vec![ExplicitEdge {
                    target: VertexId(5),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                5,
                Some(entry_h_decl),
                "src/entry.ts",
                None,
                IrOp::AssignLocal {
                    target: LocalBindingId(0),
                    op: AssignOp::Assign,
                    value: ValueId(0),
                    is_init: true,
                },
                vec![ExplicitEdge {
                    target: VertexId(6),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                6,
                Some(entry_attach_callee),
                "src/entry.ts",
                Some(ValueId(1)),
                IrOp::ReadSymbol {
                    ident_syntax: entry_attach_callee,
                },
                vec![ExplicitEdge {
                    target: VertexId(7),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                7,
                Some(entry_attach_arg),
                "src/entry.ts",
                Some(ValueId(2)),
                IrOp::ReadLocal {
                    binding: LocalBindingId(0),
                },
                vec![ExplicitEdge {
                    target: VertexId(8),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                8,
                Some(entry_attach_call),
                "src/entry.ts",
                Some(ValueId(3)),
                IrOp::Call {
                    call_kind: IrCallKind::Explicit,
                    callee: ValueId(1),
                    receiver: None,
                    args: vec![IrCallArg::Positional(ValueId(2))],
                },
                vec![ExplicitEdge {
                    target: VertexId(9),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                9,
                Some(entry_use_conn_callee),
                "src/entry.ts",
                Some(ValueId(4)),
                IrOp::ReadSymbol {
                    ident_syntax: entry_use_conn_callee,
                },
                vec![ExplicitEdge {
                    target: VertexId(10),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                10,
                Some(entry_use_conn_arg),
                "src/entry.ts",
                Some(ValueId(5)),
                IrOp::ReadLocal {
                    binding: LocalBindingId(0),
                },
                vec![ExplicitEdge {
                    target: VertexId(11),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                11,
                Some(entry_use_conn_call),
                "src/entry.ts",
                Some(ValueId(6)),
                IrOp::Call {
                    call_kind: IrCallKind::Explicit,
                    callee: ValueId(4),
                    receiver: None,
                    args: vec![IrCallArg::Positional(ValueId(5))],
                },
                vec![ExplicitEdge {
                    target: VertexId(1),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
        ],
        entry: VertexId(0),
        normal_exit: VertexId(1),
        exceptional_exit: VertexId(2),
        exit: VertexId(3),
        syntax_to_vertex: FxHashMap::default(),
        expr_to_vertex: FxHashMap::default(),
        expr_to_value: caller_expr_to_val,
        value_sources: FxHashMap::default(),
        branches: Vec::new(),
        value_count: 7,
    });

    env.irs.insert(entry_mod_syn, entry_mod_ir);
    env.irs.insert(entry_use_token_fn, entry_use_token_ir);
    env.irs.insert(entry_use_conn_fn, entry_use_conn_ir);
    env.irs.insert(entry_caller_fn, entry_caller_ir);
    env.scopes_by_file.insert(
        entry_file,
        vec![
            scope_entry(entry_mod_syn, IrScopeKind::ModuleTopLevel, 0, 15),
            scope_entry(entry_use_token_fn, IrScopeKind::Function, 20, 45),
            scope_entry(entry_use_conn_fn, IrScopeKind::Function, 50, 105),
            scope_entry(entry_caller_fn, IrScopeKind::Function, 110, 160),
        ],
    );

    let allowed: HashSet<FileId> = [entry_file, pool_file].into_iter().collect();
    let summaries = build_summaries();

    // 1. resolve_instances_on_ir: exercises call_graph() + ensure_field_index().
    env.reset_counters();
    let (inst_outcome, inst_graph, inst_field_idx) = resolve_instances_on_ir(
        &[InstanceTargetSpec {
            syntax: entry_conn_read,
            symbol: None,
        }],
        &project_files,
        Some(&allowed),
        &env,
        &env,
        &env,
        &summaries,
        None,
        None,
    );
    assert!(
        inst_graph.is_none(),
        "scoped call graph must not be returned for global cache"
    );
    assert!(
        inst_field_idx.is_none(),
        "scoped field write index must not be returned for global cache"
    );
    let proved_origin = match &inst_outcome.results[0] {
        InstanceResolutionOutcome::Proved { origins, .. } => {
            assert!(
                origins
                    .iter()
                    .any(|o| o.origin_syntax == Some(pool_acq_call)),
                "expected Proved origin from pool.ts acquire(), got {origins:?}"
            );
            origins[0].clone()
        }
        other => panic!("expected Proved origin from pool.ts acquire(), got {other:?}"),
    };
    env.assert_no_bg_file_touched(&bg_files);

    // 2. trace_value_propagation: exercises propagate_decl_reads_in_nested_scopes + call_graph().
    env.reset_counters();
    let (prop_outcome, prop_graph) = trace_value_propagation(
        &[ValueEndpointSpec {
            syntax: pool_token_lit,
            symbol: None,
        }],
        &[ValueEndpointSpec {
            syntax: entry_sink_decl,
            symbol: None,
        }],
        false,
        &project_files,
        Some(&allowed),
        &env,
        &env,
        &env,
        &summaries,
        None,
    );
    assert!(
        prop_graph.is_none(),
        "scoped call graph must not be returned for global cache"
    );
    assert!(
        matches!(
            &prop_outcome.results[0],
            ValuePropagationTargetOutcome::Proved { .. }
        ),
        "expected Proved value propagation from pool.ts to entry.ts, got {:?}",
        prop_outcome.results[0]
    );
    env.assert_no_bg_file_touched(&bg_files);

    // 3. evaluate_instance_cleanup_on_ir: exercises ensure_cleanup_index + trace_val_hosts (call_graph + field_index).
    env.reset_counters();
    let (cleanup_outcome, cleanup_graph, cleanup_field_idx) = evaluate_instance_cleanup_on_ir(
        &[CleanupScopeSpec {
            syntax: entry_use_conn_fn,
        }],
        &[CleanupTargetSpec {
            instance_key: proved_origin.instance_key,
            origin_kind: proved_origin.origin_kind,
            allocation_location: proved_origin.allocation_location,
            origin_syntax: proved_origin.origin_syntax,
            summary_id: proved_origin.summary_id,
            summary_source: proved_origin.summary_source,
            evidence: proved_origin.evidence,
        }],
        &project_files,
        Some(&allowed),
        &env,
        &env,
        &env,
        &summaries,
        None,
        None,
    );
    assert!(
        cleanup_graph.is_none(),
        "scoped call graph must not be returned for global cache"
    );
    assert!(
        cleanup_field_idx.is_none(),
        "scoped field write index must not be returned for global cache"
    );
    assert!(
        matches!(
            &cleanup_outcome.results[0].target_results[0].status,
            TargetCleanupStatusData::Proved { .. }
        ),
        "expected Proved cleanup in entry.ts, got {:?}",
        cleanup_outcome.results[0].target_results[0].status
    );
    env.assert_no_bg_file_touched(&bg_files);
}
