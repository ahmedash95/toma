use std::path::Path;
use thiserror::Error;
use toma_domain::*;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("stored identifier is invalid: {0}")]
    InvalidId(#[from] uuid::Error),
    #[error("storage invariant failed: {0}")]
    Invariant(String),
}

pub type StorageResult<T> = Result<T, StorageError>;

pub trait TomaStore: Send + Sync {
    fn snapshot(&self, workspace_id: WorkspaceId) -> StorageResult<WorkspaceSnapshot>;
    fn save_draft(&self, draft: &Draft) -> StorageResult<()>;
    fn insert_message(&self, message: &Message, attachments: &[Attachment]) -> StorageResult<()>;
    fn insert_thread_bundle(
        &self,
        thread: &TaskThread,
        session: &AgentSession,
        run: &Run,
    ) -> StorageResult<()>;
    fn insert_session_bundle(&self, session: &AgentSession, run: &Run) -> StorageResult<()>;
    fn update_run_status(
        &self,
        run_id: RunId,
        status: WorkStatus,
        at: TimestampMs,
    ) -> StorageResult<()>;
    fn append_raw_history(
        &self,
        run_id: RunId,
        sequence: i64,
        payload: &str,
        at: TimestampMs,
    ) -> StorageResult<()>;
    fn save_worktree(&self, worktree: &WorktreeRecord) -> StorageResult<()>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceSnapshot {
    pub workspace: Option<Workspace>,
    pub channels: Vec<Channel>,
    pub people: Vec<Person>,
    pub agents: Vec<AgentDefinition>,
    pub messages: Vec<Message>,
    pub attachments: Vec<Attachment>,
    pub threads: Vec<TaskThread>,
    pub sessions: Vec<AgentSession>,
    pub runs: Vec<Run>,
    pub drafts: Vec<Draft>,
    pub memories: Vec<ChannelMemory>,
    pub worktrees: Vec<WorktreeRecord>,
}

pub struct SqliteStore;

impl SqliteStore {
    pub fn open(_path: impl AsRef<Path>) -> StorageResult<Self> {
        Err(StorageError::Invariant(
            "SQLite implementation is not installed yet".into(),
        ))
    }
}
