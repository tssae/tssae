use std::sync::{Arc, OnceLock, RwLock};

use rustc_hash::FxHashMap;
use serde::Serialize;
use tssae_common::{
    BranchId, DiagnosticLocation, FileId, FinallyId, FunctionIrId, GapItem, LocalBindingId,
    LocalSyntaxId, SignatureId, SymbolId, SyntaxId, TypeId, UnknownReason, ValueId, VertexId,
};

/// Kind of analyzable scope represented by an IR unit.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum IrScopeKind {
    Function,
    Method,
    Constructor,
    Getter,
    Setter,
    ClassStaticInit,
    ModuleTopLevel,
    LocalBlock,
}

/// Mutability classification of a lexical binding extracted at lowering time.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BindingMutability {
    Const,
    Let,
    Var,
    Using,
    AwaitUsing,
    Parameter,
    CatchBinding,
    ImmutableDecl,
}

/// Visibility classification for class members.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MemberVisibility {
    Public,
    Protected,
    TsPrivate,
    EcmaPrivate,
}

/// Declaration semantic flags retained on lowered IR units.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Retains independent TypeScript declaration modifiers in one compact value."
)]
pub struct DeclarationSemantics {
    pub is_exported: bool,
    pub is_default_export: bool,
    pub is_readonly: bool,
    pub is_override: bool,
    pub is_static: bool,
    pub is_abstract: bool,
    pub is_async: bool,
    pub is_generator: bool,
    pub visibility: Option<MemberVisibility>,
}

/// Function-local lexical binding metadata.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IrLocalBinding {
    pub id: LocalBindingId,
    pub name: String,
    pub mutability: BindingMutability,
    pub syntax: SyntaxId,
    pub symbol: Option<SymbolId>,
}

/// Implicit accessor kind detected during lowering for property read/write sites.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ImplicitAccessorKind {
    Getter,
    Setter,
}

/// User-defined type guard or assertion predicate extracted from a function signature.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TypeGuardSemantics {
    ThisPredicate {
        is_asserts: bool,
        narrowed_type_text: Option<String>,
    },
    ParamPredicate {
        param_index: usize,
        param_name: String,
        is_asserts: bool,
        narrowed_type_text: Option<String>,
    },
}

/// Outer lexical binding captured by a nested function literal or closure scope.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CapturedBinding {
    pub name: String,
    pub mutability: BindingMutability,
    pub outer_binding: Option<LocalBindingId>,
    pub decl_syntax: SyntaxId,
    pub is_written_in_closure: bool,
    pub is_written_in_outer_after_capture: bool,
}

/// Metadata for a lowered class field or parameter property initializer.
#[derive(Clone, PartialEq, Eq, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Represents independent ECMAScript/TypeScript class field modifier flags."
)]
pub struct ClassFieldInit {
    pub name: String,
    pub syntax: SyntaxId,
    pub is_static: bool,
    pub is_readonly: bool,
    pub is_override: bool,
    pub visibility: Option<MemberVisibility>,
    pub has_initializer: bool,
}

/// Class-level IR summarizing field initializers, constructor, and static initialization scopes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ClassIr {
    pub syntax: SyntaxId,
    pub name: Option<String>,
    pub decl_semantics: DeclarationSemantics,
    pub has_extends: bool,
    pub instance_fields: Vec<ClassFieldInit>,
    pub static_fields: Vec<ClassFieldInit>,
    pub has_explicit_constructor: bool,
    pub has_static_init: bool,
}

/// Function parameter metadata in IR.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IrParameter {
    pub name: String,
    pub binding: Option<LocalBindingId>,
    pub raw_value: Option<ValueId>,
    pub syntax: SyntaxId,
    pub has_default: bool,
    pub is_rest: bool,
}

/// Property key lowered from member/element access or object literal properties.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum IrPropKey {
    Named(String),
    PrivateIdent(String),
    Computed {
        key: ValueId,
        static_name: Option<String>,
    },
}

/// Assignment operator lowered into unified assignment nodes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AssignOp {
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
    ExpAssign,
    BitAndAssign,
    BitOrAssign,
    BitXorAssign,
    ShlAssign,
    ShrAssign,
    UShrAssign,
    Increment,
    Decrement,
}

/// Unary expression operator.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum UnaryOp {
    LogicalNot,
    BitwiseNot,
    Plus,
    Minus,
    TypeOf,
    Void,
    Delete,
    Increment,
    Decrement,
}

/// Non-short-circuiting binary expression operator.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Eq,
    Neq,
    StrictEq,
    StrictNeq,
    Lt,
    Lte,
    Gt,
    Gte,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    In,
    InstanceOf,
    Comma,
}

/// Literal category lowered from AST literal nodes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LiteralKind {
    Number(String),
    BigInt(String),
    String(String),
    Boolean(bool),
    Null,
    Undefined,
    RegExp(String),
}

/// Call form represented in IR.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IrCallKind {
    Explicit,
    TaggedTemplate,
    SuperCall,
    ImplicitGetter,
    ImplicitSetter,
    ImplicitDispose { is_async: bool },
    ImplicitIteratorMethod,
    DecoratorApply,
}

/// Argument passed to a call or constructor vertex.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IrCallArg {
    Positional(ValueId),
    Spread(ValueId),
}

/// Property entry inside an object literal allocation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum IrObjectProp {
    KeyValue { key: IrPropKey, value: ValueId },
    Spread(ValueId),
}

/// Element entry inside an array literal allocation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IrArrayElem {
    Value(ValueId),
    Spread(ValueId),
    Elision,
}

/// Branch classification exposed to `listBranches` and `branchCoverage`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IrBranchKind {
    If,
    SwitchCase,
    SwitchDefault,
    Conditional,
    LogicalAnd,
    LogicalOr,
    NullishCoalescing,
    OptionalChain,
    Loop,
}

impl IrBranchKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::If => "if",
            Self::SwitchCase => "switchCase",
            Self::SwitchDefault => "switchDefault",
            Self::Conditional => "conditional",
            Self::LogicalAnd => "logicalAnd",
            Self::LogicalOr => "logicalOr",
            Self::NullishCoalescing => "nullishCoalescing",
            Self::OptionalChain => "optionalChain",
            Self::Loop => "loop",
        }
    }
}

/// Branch edge direction exposed to `listBranches`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IrBranchDirection {
    True,
    False,
    Case,
    Default,
    ShortCircuit,
    Continue,
}

impl IrBranchDirection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::True => "true",
            Self::False => "false",
            Self::Case => "case",
            Self::Default => "default",
            Self::ShortCircuit => "shortCircuit",
            Self::Continue => "continue",
        }
    }
}

/// Metadata for one selectable branch edge in a lowered CFG.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IrBranchRecord {
    pub id: BranchId,
    pub kind: IrBranchKind,
    pub direction: IrBranchDirection,
    pub condition_syntax: SyntaxId,
    pub condition_text: String,
    pub location: DiagnosticLocation,
    pub statement_location: DiagnosticLocation,
}

/// Kind of explicit control-flow edge between CFG vertices.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ExplicitEdgeKind {
    Fallthrough,
    Branch(BranchId),
    SwitchNoMatch {
        switch_location: DiagnosticLocation,
    },
    LoopBack,
    Break {
        label: Option<String>,
    },
    Continue {
        label: Option<String>,
    },
    Return,
    ExplicitThrow,
    FinallyEnter {
        finally_id: FinallyId,
        resume: VertexId,
    },
    FinallyResume {
        finally_id: FinallyId,
        resume: VertexId,
    },
    ExitMerge,
}

/// Explicit control-flow edge in the IR-derived CFG.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExplicitEdge {
    pub target: VertexId,
    pub kind: ExplicitEdgeKind,
}

/// Reason an evaluation vertex carries an implicit `mayThrow` edge.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MayThrowReason {
    Call,
    Construct,
    PropertyAccess,
    Await,
    Yield,
    DynamicOperator,
    IteratorStep,
}

/// Implicit exception edge attached to potentially throwing operations.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MayThrowEdge {
    pub target: VertexId,
    pub site_syntax: SyntaxId,
    pub reason: MayThrowReason,
}

/// Atomic IR operation stored on a single CFG vertex.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum IrOp {
    Entry,
    Exit,
    NopMerge,
    Literal {
        kind: LiteralKind,
        as_const: bool,
    },
    TemplateString {
        spans: Vec<ValueId>,
    },
    ReadLocal {
        binding: LocalBindingId,
    },
    ReadSymbol {
        ident_syntax: SyntaxId,
    },
    ThisRef,
    SuperRef,
    MetaProp,
    Unary {
        op: UnaryOp,
        operand: ValueId,
    },
    Binary {
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
    },
    ValueForward {
        source: ValueId,
        as_const: bool,
        non_null_assert: bool,
    },
    AssignLocal {
        target: LocalBindingId,
        op: AssignOp,
        value: ValueId,
        is_init: bool,
    },
    AssignSymbol {
        target_syntax: SyntaxId,
        op: AssignOp,
        value: ValueId,
        is_init: bool,
    },
    PropWrite {
        object: ValueId,
        key: IrPropKey,
        op: AssignOp,
        value: ValueId,
        is_super: bool,
        implicit_call_kind: Option<ImplicitAccessorKind>,
    },
    PropRead {
        object: ValueId,
        key: IrPropKey,
        is_super: bool,
        implicit_call_kind: Option<ImplicitAccessorKind>,
    },
    ObjectRest {
        source: ValueId,
        omitted_static_keys: Vec<String>,
        has_dynamic_omitted: bool,
    },
    Call {
        call_kind: IrCallKind,
        callee: ValueId,
        receiver: Option<ValueId>,
        args: Vec<IrCallArg>,
    },
    Construct {
        callee: ValueId,
        args: Vec<IrCallArg>,
        is_new_target: bool,
    },
    ObjectAlloc {
        props: Vec<IrObjectProp>,
        as_const: bool,
    },
    ArrayAlloc {
        elems: Vec<IrArrayElem>,
        as_const: bool,
    },
    JsxElement {
        tag_name: Option<String>,
        tag_expr: Option<ValueId>,
        props: Vec<IrObjectProp>,
        children: Vec<ValueId>,
    },
    Branch {
        condition: ValueId,
    },
    SwitchDispatch {
        discriminant: ValueId,
    },
    Return {
        value: Option<ValueId>,
    },
    Throw {
        exception: ValueId,
    },
    CatchParamBind {
        binding: Option<LocalBindingId>,
    },
    Await {
        promise: ValueId,
    },
    Yield {
        value: Option<ValueId>,
        is_delegate: bool,
    },
    IteratorInit {
        iterable: ValueId,
        is_async: bool,
    },
    IteratorNext {
        iterator: ValueId,
        is_async: bool,
    },
    IteratorRest {
        iterator: ValueId,
        is_async: bool,
    },
    IteratorClose {
        iterator: ValueId,
        is_async: bool,
    },
    FunctionLiteral {
        syntax: SyntaxId,
        captures: Vec<CapturedBinding>,
    },
    ClassLiteral {
        syntax: SyntaxId,
    },
    Unknown {
        reason: UnknownReason,
        message: String,
        location: DiagnosticLocation,
        evaluated_children: Vec<ValueId>,
    },
}

/// Single CFG vertex carrying an IR operation, back-references, and explicit/implicit edges.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CfgVertex {
    pub id: VertexId,
    pub syntax: Option<SyntaxId>,
    pub local_syntax: Option<LocalSyntaxId>,
    pub symbol: Option<SymbolId>,
    pub ty: Option<TypeId>,
    pub signature: Option<SignatureId>,
    pub location: Option<DiagnosticLocation>,
    pub value: Option<ValueId>,
    pub op: IrOp,
    pub explicit_edges: Vec<ExplicitEdge>,
    pub may_throw_edge: Option<MayThrowEdge>,
    pub enclosing_finallies: Vec<FinallyId>,
}

/// Lowered semantic IR and derived CFG for one analyzable function or local block scope.
#[derive(Clone, Debug)]
pub struct FunctionIr {
    pub id: FunctionIrId,
    pub root_syntax: SyntaxId,
    pub scope_kind: IrScopeKind,
    pub scope_location: DiagnosticLocation,
    pub decl_semantics: DeclarationSemantics,
    pub type_guard: Option<TypeGuardSemantics>,
    pub parameters: Vec<IrParameter>,
    pub bindings: Vec<IrLocalBinding>,
    pub captured_bindings: Vec<CapturedBinding>,
    pub vertices: Vec<CfgVertex>,
    pub entry: VertexId,
    pub normal_exit: VertexId,
    pub exceptional_exit: VertexId,
    pub exit: VertexId,
    pub syntax_to_vertex: FxHashMap<SyntaxId, VertexId>,
    pub expr_to_vertex: FxHashMap<LocalSyntaxId, VertexId>,
    pub expr_to_value: FxHashMap<LocalSyntaxId, ValueId>,
    pub value_sources: FxHashMap<ValueId, Vec<ValueId>>,
    pub branches: Vec<IrBranchRecord>,
    pub value_count: u32,
}

impl FunctionIr {
    /// Verifies structural invariants required of every lowered [`FunctionIr`]:
    ///
    /// 1. Special boundary vertices (`entry`, `normal_exit`, `exceptional_exit`, `exit`) are in bounds.
    /// 2. Every vertex except the unique `exit` vertex has at least one outgoing explicit edge,
    ///    and `exit` has zero outgoing explicit edges.
    /// 3. All explicit edge targets, `may_throw_edge` targets, `syntax_to_vertex`,
    ///    `expr_to_vertex`, and `expr_to_value` indices are in bounds.
    ///
    /// # Errors
    /// Returns a human-readable invariant violation description if any check fails.
    pub fn validate_invariants(&self) -> Result<(), String> {
        let len = self.vertices.len();
        if len == 0 {
            return Err("FunctionIr has zero vertices.".to_string());
        }
        let check_vid = |label: &str, vid: VertexId| -> Result<(), String> {
            let idx = usize::try_from(vid.0).unwrap_or(usize::MAX);
            if idx >= len {
                return Err(format!(
                    "{label} vertex {:?} is out of bounds (len={len}).",
                    vid.0
                ));
            }
            Ok(())
        };

        check_vid("entry", self.entry)?;
        check_vid("normal_exit", self.normal_exit)?;
        check_vid("exceptional_exit", self.exceptional_exit)?;
        check_vid("exit", self.exit)?;

        for (index, vertex) in self.vertices.iter().enumerate() {
            let expected_id = VertexId(u32::try_from(index).unwrap_or(u32::MAX));
            if vertex.id != expected_id {
                return Err(format!(
                    "Vertex at slot {index} has mismatched id {:?}.",
                    vertex.id
                ));
            }
            if vertex.id == self.exit {
                if !vertex.explicit_edges.is_empty() {
                    return Err(format!(
                        "Unique exit vertex {:?} must have no outgoing explicit edges.",
                        self.exit
                    ));
                }
            } else if vertex.explicit_edges.is_empty() {
                return Err(format!(
                    "Non-exit vertex {:?} ({:?}) has zero outgoing explicit edges.",
                    vertex.id, vertex.op
                ));
            }
            for edge in &vertex.explicit_edges {
                check_vid("explicit edge target", edge.target)?;
                match edge.kind {
                    ExplicitEdgeKind::Branch(branch_id) => {
                        let b_idx = usize::try_from(branch_id.0).unwrap_or(usize::MAX);
                        if b_idx >= self.branches.len() {
                            return Err(format!(
                                "Explicit edge references out-of-bounds BranchId({}).",
                                branch_id.0
                            ));
                        }
                    }
                    ExplicitEdgeKind::FinallyEnter { resume, .. }
                    | ExplicitEdgeKind::FinallyResume { resume, .. } => {
                        check_vid("finally completion resume", resume)?;
                    }
                    ExplicitEdgeKind::Fallthrough
                    | ExplicitEdgeKind::SwitchNoMatch { .. }
                    | ExplicitEdgeKind::LoopBack
                    | ExplicitEdgeKind::Break { .. }
                    | ExplicitEdgeKind::Continue { .. }
                    | ExplicitEdgeKind::Return
                    | ExplicitEdgeKind::ExplicitThrow
                    | ExplicitEdgeKind::ExitMerge => {}
                }
            }
            if let Some(may_throw) = vertex.may_throw_edge {
                check_vid("mayThrow edge target", may_throw.target)?;
            }
        }

        for (&syntax_id, &vid) in &self.syntax_to_vertex {
            let idx = usize::try_from(vid.0).unwrap_or(usize::MAX);
            if idx >= len {
                return Err(format!(
                    "syntax_to_vertex entry {syntax_id:?} points out of bounds to {vid:?}."
                ));
            }
        }
        for (&local_id, &vid) in &self.expr_to_vertex {
            let idx = usize::try_from(vid.0).unwrap_or(usize::MAX);
            if idx >= len {
                return Err(format!(
                    "expr_to_vertex entry {local_id:?} points out of bounds to {vid:?}."
                ));
            }
        }
        for (&local_id, &val_id) in &self.expr_to_value {
            if val_id.0 >= self.value_count {
                return Err(format!(
                    "expr_to_value entry {local_id:?} points out of bounds to {val_id:?} (count={}).",
                    self.value_count
                ));
            }
        }
        self.validate_value_sources_invariants()
    }

    fn validate_value_sources_invariants(&self) -> Result<(), String> {
        for (&val_id, sources) in &self.value_sources {
            if val_id.0 >= self.value_count {
                return Err(format!(
                    "value_sources key {val_id:?} is out of bounds (count={}).",
                    self.value_count
                ));
            }
            for &src in sources {
                if src.0 >= self.value_count {
                    return Err(format!(
                        "value_sources entry for {val_id:?} points out of bounds to {src:?} (count={}).",
                        self.value_count
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Lightweight scope metadata entry recorded in a file's [`FileIrEntry`] catalog.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScopeCatalogEntry {
    pub id: FunctionIrId,
    pub local_syntax: LocalSyntaxId,
    pub root_syntax: SyntaxId,
    pub kind: IrScopeKind,
    pub span_start: u32,
    pub span_end: u32,
}

/// Lazy IR provider implemented by the snapshot service layer for cross-scope analysis passes.
pub trait IrProvider {
    /// Returns the lowered [`FunctionIr`] for the scope rooted at `scope_syntax` (building and caching lazily).
    ///
    /// # Errors
    /// Returns a [`GapItem`] if `scope_syntax` has no body or cannot be lowered.
    fn function_ir(&self, scope_syntax: SyntaxId) -> Result<Arc<FunctionIr>, GapItem>;

    /// Returns the lowered [`FunctionIr`] enclosing `node_syntax` (including `ModuleTopLevel`).
    ///
    /// # Errors
    /// Returns a [`GapItem`] if the enclosing scope cannot be resolved or lowered.
    fn enclosing_scope_ir(&self, node_syntax: SyntaxId) -> Result<Arc<FunctionIr>, GapItem>;

    /// Returns the module top-level [`FunctionIr`] for `file`, if available.
    fn file_scope_ir(&self, file: FileId) -> Option<Arc<FunctionIr>>;

    /// Lists all cataloged analyzable scopes in `file` without forcing eager lowering.
    fn scopes_in_file(&self, file: FileId) -> Vec<ScopeCatalogEntry>;
}

type CachedScopeOutcome = Result<Arc<FunctionIr>, GapItem>;

/// Per-file lazy IR cache entry holding a snapshot-independent scope catalog and `OnceLock` slots.
///
/// Never stores `tsrs` arena pointers or transient type/signature handles, allowing safe reuse
/// across `snapshot.update` whenever the underlying `SourceFile` pointer is unchanged.
#[derive(Debug)]
pub struct FileIrEntry {
    pub file_id: FileId,
    pub rel_file: String,
    pub source_file_addr: usize,
    pub scope_catalog: Vec<ScopeCatalogEntry>,
    scope_by_local_syntax: FxHashMap<LocalSyntaxId, usize>,
    lowered_scopes: Box<[OnceLock<CachedScopeOutcome>]>,
    extra_scopes: RwLock<FxHashMap<LocalSyntaxId, CachedScopeOutcome>>,
    statement_scopes: RwLock<FxHashMap<LocalSyntaxId, CachedScopeOutcome>>,
}

impl FileIrEntry {
    #[must_use]
    pub fn new(
        file_id: FileId,
        rel_file: String,
        source_file_addr: usize,
        scope_catalog: Vec<ScopeCatalogEntry>,
    ) -> Self {
        let mut scope_by_local_syntax = FxHashMap::default();
        for (slot, entry) in scope_catalog.iter().enumerate() {
            scope_by_local_syntax.insert(entry.local_syntax, slot);
        }
        let lowered_scopes = (0..scope_catalog.len())
            .map(|_| OnceLock::new())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            file_id,
            rel_file,
            source_file_addr,
            scope_catalog,
            scope_by_local_syntax,
            lowered_scopes,
            extra_scopes: RwLock::new(FxHashMap::default()),
            statement_scopes: RwLock::new(FxHashMap::default()),
        }
    }

    #[must_use]
    pub fn slot_for_local_syntax(&self, local: LocalSyntaxId) -> Option<usize> {
        self.scope_by_local_syntax.get(&local).copied()
    }

    #[must_use]
    pub fn lowered_scope_count(&self) -> usize {
        self.lowered_scopes
            .iter()
            .filter(|slot| slot.get().is_some())
            .count()
            + self.extra_scopes.read().map_or(0, |guard| guard.len())
            + self.statement_scopes.read().map_or(0, |guard| guard.len())
    }

    /// Returns the cached callable, static-init, default-constructor, or module IR for `local`,
    /// lazily invoking `lower` only on cache miss.
    ///
    /// Returns `(result, is_cache_hit)`.
    pub fn get_or_init_scope(
        &self,
        local: LocalSyntaxId,
        lower: impl FnOnce(u32) -> Result<Arc<FunctionIr>, GapItem>,
    ) -> (Result<Arc<FunctionIr>, GapItem>, bool) {
        if let Some(slot) = self.slot_for_local_syntax(local)
            && let Some(lock) = self.lowered_scopes.get(slot)
        {
            if let Some(existing) = lock.get() {
                return (existing.clone(), true);
            }
            let scope_id = u32::try_from(slot).unwrap_or(u32::MAX);
            let mut lowered_here = false;
            let value = lock.get_or_init(|| {
                lowered_here = true;
                lower(scope_id)
            });
            return (value.clone(), !lowered_here);
        }

        if let Ok(guard) = self.extra_scopes.read()
            && let Some(existing) = guard.get(&local)
        {
            return (existing.clone(), true);
        }
        let scope_id = 0x8000_0000_u32 | local.0;
        let built = lower(scope_id);
        if let Ok(mut guard) = self.extra_scopes.write() {
            if let Some(existing) = guard.get(&local) {
                return (existing.clone(), true);
            }
            guard.insert(local, built.clone());
        }
        (built, false)
    }

    /// Returns the cached statement or local-block IR (`IrScopeKind::LocalBlock`) for `local`
    /// without colliding with cataloged or extra callable/default-constructor scopes sharing the
    /// same AST node (such as a `ClassDeclaration` without an explicit `constructor`).
    ///
    /// Returns `(result, is_cache_hit)`.
    pub fn get_or_init_statement_scope(
        &self,
        local: LocalSyntaxId,
        lower: impl FnOnce(u32) -> Result<Arc<FunctionIr>, GapItem>,
    ) -> (Result<Arc<FunctionIr>, GapItem>, bool) {
        if let Ok(guard) = self.statement_scopes.read()
            && let Some(existing) = guard.get(&local)
        {
            return (existing.clone(), true);
        }
        let scope_id = 0x4000_0000_u32 | local.0;
        let built = lower(scope_id);
        if let Ok(mut guard) = self.statement_scopes.write() {
            if let Some(existing) = guard.get(&local) {
                return (existing.clone(), true);
            }
            guard.insert(local, built.clone());
        }
        (built, false)
    }
}

/// Snapshot-level lazy IR cache indexed by session-stable [`FileId`].
#[derive(Default, Debug)]
pub struct SnapshotIrCache {
    files: RwLock<FxHashMap<FileId, Arc<FileIrEntry>>>,
}

impl SnapshotIrCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn get_file(&self, file_id: FileId) -> Option<Arc<FileIrEntry>> {
        self.files
            .read()
            .ok()
            .and_then(|guard| guard.get(&file_id).cloned())
    }

    pub fn get_or_insert_file(
        &self,
        file_id: FileId,
        build: impl FnOnce() -> FileIrEntry,
    ) -> Arc<FileIrEntry> {
        if let Some(existing) = self.get_file(file_id) {
            return existing;
        }
        let created = Arc::new(build());
        if let Ok(mut guard) = self.files.write() {
            if let Some(existing) = guard.get(&file_id) {
                return Arc::clone(existing);
            }
            guard.insert(file_id, Arc::clone(&created));
        }
        created
    }

    /// Inherits unchanged [`FileIrEntry`] values from `old` into a new snapshot cache.
    ///
    /// Returns `(new_cache, files_reused, files_invalidated)`.
    #[must_use]
    pub fn inherit_from(
        old: &Self,
        mut is_file_reused: impl FnMut(&FileIrEntry) -> bool,
    ) -> (Self, usize, usize) {
        let mut next_files = FxHashMap::default();
        let mut files_reused = 0usize;
        let mut files_invalidated = 0usize;

        if let Ok(guard) = old.files.read() {
            for (&file_id, entry) in guard.iter() {
                if is_file_reused(entry) {
                    next_files.insert(file_id, Arc::clone(entry));
                    files_reused += 1;
                } else {
                    files_invalidated += 1;
                }
            }
        }

        (
            Self {
                files: RwLock::new(next_files),
            },
            files_reused,
            files_invalidated,
        )
    }

    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.read().map_or(0, |guard| guard.len())
    }

    #[must_use]
    pub fn total_lowered_scopes(&self) -> usize {
        self.files.read().map_or(0, |guard| {
            guard
                .values()
                .map(|entry| entry.lowered_scope_count())
                .sum()
        })
    }
}

/// Helper constructor for [`FunctionIrId`] anchored to a [`FileId`].
#[must_use]
pub const fn scope_ir_id(file: FileId, local_scope: u32) -> FunctionIrId {
    FunctionIrId { file, local_scope }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_ir(file: FileId, local_scope: u32, kind: IrScopeKind) -> Arc<FunctionIr> {
        Arc::new(FunctionIr {
            id: scope_ir_id(file, local_scope),
            root_syntax: SyntaxId::from_parts(file, LocalSyntaxId(1)),
            scope_kind: kind,
            scope_location: DiagnosticLocation {
                file: "test.ts".to_string(),
                line: 1,
                column: 1,
                end_line: 1,
                end_column: 10,
                start: 0,
                end: 9,
            },
            decl_semantics: DeclarationSemantics::default(),
            type_guard: None,
            parameters: Vec::new(),
            bindings: Vec::new(),
            captured_bindings: Vec::new(),
            vertices: vec![CfgVertex {
                id: VertexId(0),
                syntax: None,
                local_syntax: None,
                symbol: None,
                ty: None,
                signature: None,
                location: None,
                value: None,
                op: IrOp::Exit,
                explicit_edges: Vec::new(),
                may_throw_edge: None,
                enclosing_finallies: Vec::new(),
            }],
            entry: VertexId(0),
            normal_exit: VertexId(0),
            exceptional_exit: VertexId(0),
            exit: VertexId(0),
            syntax_to_vertex: FxHashMap::default(),
            expr_to_vertex: FxHashMap::default(),
            expr_to_value: FxHashMap::default(),
            value_sources: FxHashMap::default(),
            branches: Vec::new(),
            value_count: 0,
        })
    }

    #[test]
    fn separates_statement_scope_cache_from_catalog_and_extra_scopes() {
        let file = FileId(3);
        let catalog_local = LocalSyntaxId(7);
        let extra_local = LocalSyntaxId(19);
        let entry = FileIrEntry::new(
            file,
            "test.ts".to_string(),
            0x1234,
            vec![ScopeCatalogEntry {
                id: scope_ir_id(file, 0),
                local_syntax: catalog_local,
                root_syntax: SyntaxId::from_parts(file, catalog_local),
                kind: IrScopeKind::Constructor,
                span_start: 0,
                span_end: 10,
            }],
        );

        let (stmt_res, stmt_hit) = entry.get_or_init_statement_scope(catalog_local, |sid| {
            Ok(dummy_ir(file, sid, IrScopeKind::LocalBlock))
        });
        assert!(!stmt_hit);
        let stmt_ir = stmt_res.unwrap();
        assert_eq!(stmt_ir.scope_kind, IrScopeKind::LocalBlock);

        let (ctor_res, ctor_hit) = entry.get_or_init_scope(catalog_local, |sid| {
            Ok(dummy_ir(file, sid, IrScopeKind::Constructor))
        });
        assert!(!ctor_hit);
        let ctor_ir = ctor_res.unwrap();
        assert_eq!(ctor_ir.scope_kind, IrScopeKind::Constructor);
        assert_ne!(stmt_ir.id, ctor_ir.id);

        let (extra_ctor, _) = entry.get_or_init_scope(extra_local, |sid| {
            Ok(dummy_ir(file, sid, IrScopeKind::Constructor))
        });
        let (extra_stmt, _) = entry.get_or_init_statement_scope(extra_local, |sid| {
            Ok(dummy_ir(file, sid, IrScopeKind::LocalBlock))
        });
        assert_ne!(extra_ctor.unwrap().id, extra_stmt.unwrap().id);
        assert_eq!(entry.lowered_scope_count(), 4);
    }
}
