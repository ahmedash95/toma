use std::path::{Path, PathBuf};
use thiserror::Error;
use toma_domain::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeRequest<'a> {
    pub repository_path: &'a Path,
    pub worktree_root: &'a Path,
    pub thread_id: ThreadId,
    pub base_ref: &'a str,
    pub created_at: TimestampMs,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CleanupDecision {
    Safe,
    Retain { reasons: Vec<String> },
}

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("git command failed: {0}")]
    Git(String),
    #[error("worktree I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("write lock is held by {holder}")]
    WriteLocked { holder: String },
}

pub trait WorktreeManager: Send + Sync {
    fn ensure(&self, request: WorktreeRequest<'_>) -> Result<WorktreeRecord, WorktreeError>;
    fn acquire_write_lock(
        &self,
        thread_id: ThreadId,
        owner: &str,
    ) -> Result<WriteLease, WorktreeError>;
    fn cleanup_decision(
        &self,
        repository_path: &Path,
        record: &WorktreeRecord,
    ) -> Result<CleanupDecision, WorktreeError>;
}

#[derive(Debug)]
pub struct WriteLease {
    pub thread_id: ThreadId,
    pub owner: String,
    pub lock_path: PathBuf,
}

pub struct GitWorktreeManager;
