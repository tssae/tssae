use rustc_hash::FxHashMap;
use serde::Serialize;
use tssae_common::{FileId, FunctionIrId, SyntaxId};

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
}

/// Lightweight scope metadata placeholder used until full IR lowering lands.
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
