use rustc_hash::FxHashMap;
use serde::Serialize;
use tssae_common::{
    BranchId, DiagnosticLocation, FileId, FunctionIrId, LocalBindingId, LocalSyntaxId, SignatureId,
    SymbolId, SyntaxId, TypeId, UnknownReason, ValueId, VertexId,
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

/// Function parameter metadata in IR.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IrParameter {
    pub name: String,
    pub binding: Option<LocalBindingId>,
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
    SwitchNoMatch { switch_location: DiagnosticLocation },
    LoopBack,
    Break { label: Option<String> },
    Continue { label: Option<String> },
    Return,
    ExplicitThrow,
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
    },
    PropRead {
        object: ValueId,
        key: IrPropKey,
        is_super: bool,
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
    FunctionLiteral {
        syntax: SyntaxId,
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
}

/// Lowered semantic IR and derived CFG for one analyzable function or local block scope.
#[derive(Clone, Debug)]
pub struct FunctionIr {
    pub id: FunctionIrId,
    pub root_syntax: SyntaxId,
    pub scope_kind: IrScopeKind,
    pub scope_location: DiagnosticLocation,
    pub decl_semantics: DeclarationSemantics,
    pub parameters: Vec<IrParameter>,
    pub bindings: Vec<IrLocalBinding>,
    pub vertices: Vec<CfgVertex>,
    pub entry: VertexId,
    pub normal_exit: VertexId,
    pub exceptional_exit: VertexId,
    pub exit: VertexId,
    pub syntax_to_vertex: FxHashMap<SyntaxId, VertexId>,
    pub expr_to_vertex: FxHashMap<LocalSyntaxId, VertexId>,
    pub expr_to_value: FxHashMap<LocalSyntaxId, ValueId>,
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
                if let ExplicitEdgeKind::Branch(branch_id) = edge.kind {
                    let b_idx = usize::try_from(branch_id.0).unwrap_or(usize::MAX);
                    if b_idx >= self.branches.len() {
                        return Err(format!(
                            "Explicit edge references out-of-bounds BranchId({}).",
                            branch_id.0
                        ));
                    }
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

        Ok(())
    }
}

/// Lightweight scope metadata placeholder used for file scope cataloging.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScopeStub {
    pub id: FunctionIrId,
    pub root_syntax: SyntaxId,
    pub kind: IrScopeKind,
}

/// Per-file catalog of analyzable scope stubs indexed by [`FileId`].
#[derive(Default, Debug)]
pub struct FileScopeCatalog {
    scopes_by_file: FxHashMap<FileId, Vec<ScopeStub>>,
}

impl FileScopeCatalog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_scope(&mut self, scope: ScopeStub) {
        self.scopes_by_file
            .entry(scope.id.file)
            .or_default()
            .push(scope);
    }

    #[must_use]
    pub fn scopes_for_file(&self, file: FileId) -> &[ScopeStub] {
        self.scopes_by_file.get(&file).map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn file_count(&self) -> usize {
        self.scopes_by_file.len()
    }
}

/// Snapshot-level IR lowering and cache statistics.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IrCacheStats {
    pub files_cached: usize,
    pub scopes_lowered: usize,
    pub cache_hits: usize,
    pub cache_invalidations: usize,
}

impl IrCacheStats {
    #[must_use]
    pub const fn for_file_count(files_cached: usize) -> Self {
        Self {
            files_cached,
            scopes_lowered: 0,
            cache_hits: 0,
            cache_invalidations: 0,
        }
    }
}

/// Helper constructor for [`FunctionIrId`] anchored to a [`FileId`].
#[must_use]
pub const fn scope_ir_id(file: FileId, local_scope: u32) -> FunctionIrId {
    FunctionIrId { file, local_scope }
}
