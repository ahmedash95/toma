use std::sync::Arc;
use thiserror::Error;
use toma_domain::*;
use toma_runner::{AgentRunner, RunnerError};
use toma_storage::{StorageError, TomaStore, WorkspaceSnapshot};
use toma_worktree::{WorktreeError, WorktreeManager};

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Runner(#[from] RunnerError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error("invalid command: {0}")]
    InvalidCommand(String),
}

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> TimestampMs;
}

pub struct TomaCore {
    pub store: Arc<dyn TomaStore>,
    pub worktrees: Arc<dyn WorktreeManager>,
    pub runners: Vec<Arc<dyn AgentRunner>>,
    pub clock: Arc<dyn Clock>,
}

impl TomaCore {
    pub fn snapshot(&self, workspace_id: WorkspaceId) -> Result<WorkspaceSnapshot, CoreError> {
        Ok(self.store.snapshot(workspace_id)?)
    }

    pub fn dispatch(&self, _command: AppCommand) -> Result<Vec<AppEvent>, CoreError> {
        Err(CoreError::InvalidCommand(
            "orchestration implementation is not installed yet".into(),
        ))
    }
}
