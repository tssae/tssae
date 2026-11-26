/// Monotonically assigned normalized file identifier within an engine session.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FileId(pub u32);

/// File-local syntax node identifier, stable while the parsed source file is unchanged.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct LocalSyntaxId(pub u32);

/// Global syntax handle packing [`FileId`] (high 32 bits) and [`LocalSyntaxId`] (low 32 bits).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SyntaxId(pub u64);

impl SyntaxId {
    #[must_use]
    pub const fn from_parts(file: FileId, local: LocalSyntaxId) -> Self {
        Self(((file.0 as u64) << 32) | (local.0 as u64))
    }

    #[must_use]
    pub const fn file_id(self) -> FileId {
        FileId((self.0 >> 32) as u32)
    }

    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Low 32 bits of SyntaxId store LocalSyntaxId(u32)."
    )]
    pub const fn local_id(self) -> LocalSyntaxId {
        LocalSyntaxId(self.0 as u32)
    }
}

/// Snapshot-scoped canonical symbol handle.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SymbolId(pub u64);

/// Snapshot-scoped type handle (not persisted across snapshot updates).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TypeId(pub u32);

/// Snapshot-scoped signature handle (not persisted across snapshot updates).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SignatureId(pub u32);

/// Function-local lexical binding identifier.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct LocalBindingId(pub u32);

/// Identifier for an analyzable scope (function, method, constructor, accessor, static block, or top-level).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FunctionIrId {
    pub file: FileId,
    pub local_scope: u32,
}

/// Evaluation value slot index within a function IR.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ValueId(pub u32);

/// Control-flow graph vertex index within a function IR.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct VertexId(pub u32);

/// Control-flow branch record index within a function IR.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BranchId(pub u32);

/// Lexical `finally` block identifier within a function IR.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FinallyId(pub u32);

/// Analysis-level instance identity handle.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct InstanceId(pub u32);
