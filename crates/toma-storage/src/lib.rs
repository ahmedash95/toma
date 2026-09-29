use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use thiserror::Error;
use toma_domain::*;

macro_rules! to_json {
    ($value:expr) => {
        serde_json::to_string($value)
    };
}

macro_rules! from_json {
    ($value:expr) => {
        serde_json::from_str($value).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    };
}

const SCHEMA_VERSION: i64 = 5;
const MIGRATION_1: &str = include_str!("migration_1.sql");

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
    fn insert_run(&self, run: &Run) -> StorageResult<()>;
    fn insert_channel(&self, channel: &Channel) -> StorageResult<()>;
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
    /// Every raw line a run produced, in order.
    fn raw_history(&self, run_id: RunId) -> StorageResult<Vec<RawHistoryEntry>>;
    fn save_worktree(&self, worktree: &WorktreeRecord) -> StorageResult<()>;
    fn save_run_usage(&self, usage: &RunUsage) -> StorageResult<()>;
    fn set_permission_mode(&self, thread_id: ThreadId, mode: PermissionMode) -> StorageResult<()>;
    fn set_provider_session_id(
        &self,
        session_id: SessionId,
        provider_session_id: &str,
    ) -> StorageResult<()>;
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
    pub usages: Vec<RunUsage>,
    /// Live-only; never persisted because a restart ends the runs that asked.
    pub permission_requests: Vec<PermissionRequest>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawHistoryEntry {
    pub run_id: RunId,
    pub sequence: i64,
    pub payload: String,
    pub created_at: TimestampMs,
}

pub struct SqliteStore {
    connection: Mutex<Connection>,
}

impl SqliteStore {
    /// Opens an existing database or creates and migrates a new one.
    pub fn open(path: impl AsRef<Path>) -> StorageResult<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> StorageResult<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut connection: Connection) -> StorageResult<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn connection(&self) -> StorageResult<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| StorageError::Invariant("database mutex was poisoned".into()))
    }

    pub fn insert_workspace(&self, workspace: &Workspace) -> StorageResult<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO workspaces (id, name, repository_path, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![workspace.id.to_string(), workspace.name, to_json!(&workspace.repository_path)?, workspace.created_at],
        )?;
        Ok(())
    }

    pub fn insert_person(&self, person: &Person) -> StorageResult<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO people (id, workspace_id, display_name) VALUES (?1, ?2, ?3)",
            params![
                person.id.to_string(),
                person.workspace_id.to_string(),
                person.display_name
            ],
        )?;
        Ok(())
    }

    pub fn insert_agent(&self, agent: &AgentDefinition) -> StorageResult<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO agents (id, workspace_id, name, role, instructions, provider, enabled) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![agent.id.to_string(), agent.workspace_id.to_string(), agent.name, agent.role, agent.instructions, to_json!(&agent.provider)?, agent.enabled],
        )?;
        Ok(())
    }

    pub fn insert_memory(&self, memory: &ChannelMemory) -> StorageResult<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO memories (id, channel_id, fact, source_message_id, supersedes_id, active, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![memory.id.to_string(), memory.channel_id.to_string(), memory.fact, memory.source_message_id.to_string(), optional_id(memory.supersedes_id), memory.active, memory.created_at],
        )?;
        Ok(())
    }

    /// Seeds a stable workspace shell for demos. Calling it repeatedly is idempotent.
    pub fn seed_demo_workspace(
        &self,
        repository_path: impl Into<PathBuf>,
    ) -> StorageResult<WorkspaceId> {
        let workspace_id: WorkspaceId = parse_id("018f0000-0000-7000-8000-000000000001")?;
        let general_id: ChannelId = parse_id("018f0000-0000-7000-8000-000000000002")?;
        let work_id: ChannelId = parse_id("018f0000-0000-7000-8000-000000000003")?;
        let codex_id: AgentId = parse_id("018f0000-0000-7000-8000-000000000004")?;
        let claude_id: AgentId = parse_id("018f0000-0000-7000-8000-000000000005")?;
        let person_id: PersonId = parse_id("018f0000-0000-7000-8000-000000000006")?;
        let cursor_id: AgentId = parse_id("018f0000-0000-7000-8000-000000000007")?;
        let repository_path = repository_path.into();
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        tx.execute(
            "INSERT INTO workspaces (id, name, repository_path, created_at) VALUES (?1, 'Toma', ?2, 0) ON CONFLICT(id) DO NOTHING",
            params![workspace_id.to_string(), to_json!(&repository_path)?],
        )?;
        tx.execute(
            "INSERT INTO people (id, workspace_id, display_name) VALUES (?1, ?2, 'You') ON CONFLICT(id) DO NOTHING",
            params![person_id.to_string(), workspace_id.to_string()],
        )?;
        for (id, name, position) in [(general_id, "general", 0), (work_id, "work", 1)] {
            tx.execute(
                "INSERT INTO channels (id, workspace_id, name, position) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(id) DO NOTHING",
                params![id.to_string(), workspace_id.to_string(), name, position],
            )?;
        }
        for (id, name, role, provider) in [
            (codex_id, "Codex", "coding agent", RunnerProvider::CodexCli),
            (
                claude_id,
                "Claude",
                "coding agent",
                RunnerProvider::ClaudeCodeCli,
            ),
            (
                cursor_id,
                "Cursor",
                "coding agent",
                RunnerProvider::CursorCli,
            ),
        ] {
            tx.execute(
                "INSERT INTO agents (id, workspace_id, name, role, instructions, provider, enabled) VALUES (?1, ?2, ?3, ?4, '', ?5, 1) ON CONFLICT(id) DO NOTHING",
                params![id.to_string(), workspace_id.to_string(), name, role, to_json!(&provider)?],
            )?;
        }
        tx.commit()?;
        Ok(workspace_id)
    }
}

impl TomaStore for SqliteStore {
    fn snapshot(&self, workspace_id: WorkspaceId) -> StorageResult<WorkspaceSnapshot> {
        let connection = self.connection()?;
        let workspace_key = workspace_id.to_string();
        let workspace = connection
            .query_row(
                "SELECT id, name, repository_path, created_at FROM workspaces WHERE id = ?1",
                [&workspace_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get(1)?,
                        row.get::<_, String>(2)?,
                        row.get(3)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(id, name, repository_path, created_at)| -> StorageResult<Workspace> {
                    Ok(Workspace {
                        id: parse_id(&id)?,
                        name,
                        repository_path: from_json!(&repository_path)?,
                        created_at,
                    })
                },
            )
            .transpose()?;

        if workspace.is_none() {
            return Ok(WorkspaceSnapshot::default());
        }

        Ok(WorkspaceSnapshot {
            workspace,
            channels: query_channels(&connection, &workspace_key)?,
            people: query_people(&connection, &workspace_key)?,
            agents: query_agents(&connection, &workspace_key)?,
            messages: query_messages(&connection, &workspace_key)?,
            attachments: query_attachments(&connection, &workspace_key)?,
            threads: query_threads(&connection, &workspace_key)?,
            sessions: query_sessions(&connection, &workspace_key)?,
            runs: query_runs(&connection, &workspace_key)?,
            drafts: query_drafts(&connection, &workspace_key)?,
            memories: query_memories(&connection, &workspace_key)?,
            worktrees: query_worktrees(&connection, &workspace_key)?,
            usages: query_usages(&connection, &workspace_key)?,
            permission_requests: Vec::new(),
        })
    }

    fn save_draft(&self, draft: &Draft) -> StorageResult<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO drafts (channel_id, thread_id, body, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
            params![
                draft.channel_id.to_string(),
                optional_id(draft.thread_id),
                draft.body,
                draft.updated_at
            ],
        )?;
        Ok(())
    }

    fn insert_message(&self, message: &Message, attachments: &[Attachment]) -> StorageResult<()> {
        if attachments
            .iter()
            .any(|attachment| attachment.message_id != message.id)
        {
            return Err(StorageError::Invariant(
                "attachment does not belong to the supplied message".into(),
            ));
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        insert_message_row(&tx, message)?;
        for attachment in attachments {
            tx.execute(
                "INSERT INTO attachments (id, message_id, target) VALUES (?1, ?2, ?3)",
                params![
                    attachment.id.to_string(),
                    attachment.message_id.to_string(),
                    to_json!(&attachment.target)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn insert_thread_bundle(
        &self,
        thread: &TaskThread,
        session: &AgentSession,
        run: &Run,
    ) -> StorageResult<()> {
        if session.thread_id != thread.id || run.session_id != session.id {
            return Err(StorageError::Invariant(
                "thread, session, and run bundle identifiers do not match".into(),
            ));
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        insert_thread_row(&tx, thread)?;
        insert_session_row(&tx, session)?;
        insert_run_row(&tx, run)?;
        tx.commit()?;
        Ok(())
    }

    fn insert_channel(&self, channel: &Channel) -> StorageResult<()> {
        self.connection()?.execute(
            "INSERT INTO channels (id, workspace_id, name, position, repository_path) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                channel.id.to_string(),
                channel.workspace_id.to_string(),
                channel.name,
                channel.position,
                channel
                    .repository_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
            ],
        )?;
        Ok(())
    }

    fn insert_run(&self, run: &Run) -> StorageResult<()> {
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        insert_run_row(&tx, run)?;
        tx.commit()?;
        Ok(())
    }

    fn insert_session_bundle(&self, session: &AgentSession, run: &Run) -> StorageResult<()> {
        if run.session_id != session.id {
            return Err(StorageError::Invariant(
                "session and run bundle identifiers do not match".into(),
            ));
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        insert_session_row(&tx, session)?;
        insert_run_row(&tx, run)?;
        tx.commit()?;
        Ok(())
    }

    fn update_run_status(
        &self,
        run_id: RunId,
        status: WorkStatus,
        at: TimestampMs,
    ) -> StorageResult<()> {
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        let current: Option<(String, String)> = tx
            .query_row(
                "SELECT status, session_id FROM runs WHERE id = ?1",
                [run_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (current, session_id) = current
            .ok_or_else(|| StorageError::Invariant(format!("run {run_id} does not exist")))?;
        let current: WorkStatus = from_json!(&current)?;
        current
            .transition_to(status)
            .map_err(|error| StorageError::Invariant(error.to_string()))?;
        tx.execute(
            "UPDATE runs SET status = ?2,
                started_at = CASE WHEN ?3 = 1 THEN COALESCE(started_at, ?4) ELSE started_at END,
                finished_at = CASE WHEN ?5 = 1 THEN COALESCE(finished_at, ?4) ELSE finished_at END
             WHERE id = ?1",
            params![
                run_id.to_string(),
                to_json!(&status)?,
                status != WorkStatus::Queued,
                at,
                status.is_terminal()
            ],
        )?;
        tx.execute(
            "UPDATE sessions SET status = ?2 WHERE id = ?1",
            params![session_id, to_json!(&status)?],
        )?;
        tx.execute(
            "UPDATE threads SET status = ?2, updated_at = ?3 WHERE id = (SELECT thread_id FROM sessions WHERE id = ?1)",
            params![session_id, to_json!(&status)?, at],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn raw_history(&self, run_id: RunId) -> StorageResult<Vec<RawHistoryEntry>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT run_id, sequence, payload, created_at FROM raw_history WHERE run_id = ?1 ORDER BY sequence",
        )?;
        let rows = statement.query_map([run_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })?;
        collect_rows(rows)?
            .into_iter()
            .map(|(run_id, sequence, payload, created_at)| {
                Ok(RawHistoryEntry {
                    run_id: parse_id(&run_id)?,
                    sequence,
                    payload,
                    created_at,
                })
            })
            .collect()
    }

    fn append_raw_history(
        &self,
        run_id: RunId,
        sequence: i64,
        payload: &str,
        at: TimestampMs,
    ) -> StorageResult<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO raw_history (run_id, sequence, payload, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![run_id.to_string(), sequence, payload, at],
        )?;
        Ok(())
    }

    fn set_provider_session_id(
        &self,
        session_id: SessionId,
        provider_session_id: &str,
    ) -> StorageResult<()> {
        self.connection()?.execute(
            "UPDATE sessions SET provider_session_id = ?2 WHERE id = ?1",
            params![session_id.to_string(), provider_session_id],
        )?;
        Ok(())
    }

    fn set_permission_mode(&self, thread_id: ThreadId, mode: PermissionMode) -> StorageResult<()> {
        self.connection()?.execute(
            "UPDATE threads SET permission_mode = ?2 WHERE id = ?1",
            params![thread_id.to_string(), to_json!(&mode)?],
        )?;
        Ok(())
    }

    fn save_run_usage(&self, usage: &RunUsage) -> StorageResult<()> {
        self.connection()?.execute(
            "INSERT INTO run_usage (run_id, input_tokens, output_tokens, cached_tokens, cost_micros, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(run_id) DO UPDATE SET input_tokens = excluded.input_tokens,
                output_tokens = excluded.output_tokens, cached_tokens = excluded.cached_tokens,
                cost_micros = excluded.cost_micros, duration_ms = excluded.duration_ms",
            params![
                usage.run_id.to_string(),
                usage.input_tokens,
                usage.output_tokens,
                usage.cached_tokens,
                usage.cost_micros,
                usage.duration_ms
            ],
        )?;
        Ok(())
    }

    fn save_worktree(&self, worktree: &WorktreeRecord) -> StorageResult<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO worktrees (thread_id, path, branch, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(thread_id) DO UPDATE SET path = excluded.path, branch = excluded.branch, created_at = excluded.created_at",
            params![worktree.thread_id.to_string(), to_json!(&worktree.path)?, worktree.branch, worktree.created_at],
        )?;
        Ok(())
    }
}

fn migrate(connection: &mut Connection) -> StorageResult<()> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StorageError::Invariant(format!(
            "database schema version {version} is newer than supported version {SCHEMA_VERSION}"
        )));
    }
    if version < 1 {
        let tx = connection.transaction()?;
        tx.execute_batch(MIGRATION_1)?;
        tx.pragma_update(None, "user_version", 1)?;
        tx.commit()?;
    }
    if version < 2 {
        let tx = connection.transaction()?;
        tx.execute_batch("ALTER TABLE channels ADD COLUMN repository_path TEXT")?;
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
    }
    if version < 3 {
        let tx = connection.transaction()?;
        tx.execute_batch(
            "CREATE TABLE run_usage (
                run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cached_tokens INTEGER NOT NULL,
                cost_micros INTEGER,
                duration_ms INTEGER
            )",
        )?;
        tx.pragma_update(None, "user_version", 3)?;
        tx.commit()?;
    }
    if version < 4 {
        let tx = connection.transaction()?;
        tx.execute_batch(
            "ALTER TABLE threads ADD COLUMN permission_mode TEXT NOT NULL DEFAULT '\"ask\"'",
        )?;
        tx.pragma_update(None, "user_version", 4)?;
        tx.commit()?;
    }
    if version < 5 {
        let tx = connection.transaction()?;
        tx.execute_batch("ALTER TABLE messages ADD COLUMN run_id TEXT REFERENCES runs(id)")?;
        tx.pragma_update(None, "user_version", 5)?;
        tx.commit()?;
    }
    Ok(())
}

fn parse_id<T: FromStr<Err = uuid::Error>>(value: &str) -> rusqlite::Result<T> {
    value.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}
fn optional_id<T: ToString>(value: Option<T>) -> Option<String> {
    value.map(|id| id.to_string())
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> StorageResult<Vec<T>> {
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn insert_message_row(tx: &Transaction<'_>, message: &Message) -> StorageResult<()> {
    tx.execute(
        "INSERT INTO messages (id, channel_id, thread_id, author, body, created_at, run_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![message.id.to_string(), message.channel_id.to_string(), optional_id(message.thread_id), to_json!(&message.author)?, message.body, message.created_at, optional_id(message.run_id)],
    )?;
    Ok(())
}

fn insert_thread_row(tx: &Transaction<'_>, thread: &TaskThread) -> StorageResult<()> {
    tx.execute(
        "INSERT INTO threads (id, channel_id, root_message_id, title, status, created_at, updated_at, permission_mode) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![thread.id.to_string(), thread.channel_id.to_string(), thread.root_message_id.to_string(), thread.title, to_json!(&thread.status)?, thread.created_at, thread.updated_at, to_json!(&thread.permission_mode)?],
    )?;
    Ok(())
}

fn insert_session_row(tx: &Transaction<'_>, session: &AgentSession) -> StorageResult<()> {
    tx.execute(
        "INSERT INTO sessions (id, thread_id, agent_id, provider_session_id, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![session.id.to_string(), session.thread_id.to_string(), session.agent_id.to_string(), session.provider_session_id, to_json!(&session.status)?, session.created_at],
    )?;
    Ok(())
}

fn insert_run_row(tx: &Transaction<'_>, run: &Run) -> StorageResult<()> {
    tx.execute(
        "INSERT INTO runs (id, session_id, sequence, status, started_at, finished_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![run.id.to_string(), run.session_id.to_string(), run.sequence, to_json!(&run.status)?, run.started_at, run.finished_at],
    )?;
    Ok(())
}

macro_rules! query_entities {
    ($name:ident, $ty:ty, $sql:expr, $map:expr) => {
        fn $name(connection: &Connection, workspace_id: &str) -> StorageResult<Vec<$ty>> {
            let mut statement = connection.prepare($sql)?;
            let rows = collect_rows(statement.query_map([workspace_id], $map)?)?;
            rows.into_iter().collect()
        }
    };
}

query_entities!(
    query_channels,
    Channel,
    "SELECT c.id, c.workspace_id, c.name, c.position, c.repository_path FROM channels c WHERE c.workspace_id = ?1 ORDER BY c.position, c.id",
    |row| -> rusqlite::Result<StorageResult<Channel>> {
        Ok(Ok(Channel {
            id: parse_id(&row.get::<_, String>(0)?)?,
            workspace_id: parse_id(&row.get::<_, String>(1)?)?,
            name: row.get(2)?,
            position: row.get(3)?,
            repository_path: row.get::<_, Option<String>>(4)?.map(PathBuf::from),
        }))
    }
);
query_entities!(
    query_people,
    Person,
    "SELECT p.id, p.workspace_id, p.display_name FROM people p WHERE p.workspace_id = ?1 ORDER BY p.display_name, p.id",
    |row| -> rusqlite::Result<StorageResult<Person>> {
        Ok(Ok(Person {
            id: parse_id(&row.get::<_, String>(0)?)?,
            workspace_id: parse_id(&row.get::<_, String>(1)?)?,
            display_name: row.get(2)?,
        }))
    }
);
query_entities!(
    query_agents,
    AgentDefinition,
    "SELECT a.id, a.workspace_id, a.name, a.role, a.instructions, a.provider, a.enabled FROM agents a WHERE a.workspace_id = ?1 ORDER BY a.name, a.id",
    |row| -> rusqlite::Result<StorageResult<AgentDefinition>> {
        Ok(Ok(AgentDefinition {
            id: parse_id(&row.get::<_, String>(0)?)?,
            workspace_id: parse_id(&row.get::<_, String>(1)?)?,
            name: row.get(2)?,
            role: row.get(3)?,
            instructions: row.get(4)?,
            provider: from_json!(&row.get::<_, String>(5)?)?,
            enabled: row.get(6)?,
        }))
    }
);
query_entities!(
    query_messages,
    Message,
    "SELECT m.id, m.channel_id, m.thread_id, m.author, m.body, m.created_at, m.run_id FROM messages m JOIN channels c ON c.id = m.channel_id WHERE c.workspace_id = ?1 ORDER BY m.created_at, m.id",
    |row| -> rusqlite::Result<StorageResult<Message>> {
        Ok(Ok(Message {
            id: parse_id(&row.get::<_, String>(0)?)?,
            channel_id: parse_id(&row.get::<_, String>(1)?)?,
            thread_id: row
                .get::<_, Option<String>>(2)?
                .map(|id| parse_id(&id))
                .transpose()?,
            author: from_json!(&row.get::<_, String>(3)?)?,
            body: row.get(4)?,
            created_at: row.get(5)?,
            run_id: row
                .get::<_, Option<String>>(6)?
                .map(|id| parse_id(&id))
                .transpose()?,
        }))
    }
);
query_entities!(
    query_attachments,
    Attachment,
    "SELECT a.id, a.message_id, a.target FROM attachments a JOIN messages m ON m.id = a.message_id JOIN channels c ON c.id = m.channel_id WHERE c.workspace_id = ?1 ORDER BY a.id",
    |row| -> rusqlite::Result<StorageResult<Attachment>> {
        Ok(Ok(Attachment {
            id: parse_id(&row.get::<_, String>(0)?)?,
            message_id: parse_id(&row.get::<_, String>(1)?)?,
            target: from_json!(&row.get::<_, String>(2)?)?,
        }))
    }
);
query_entities!(
    query_threads,
    TaskThread,
    "SELECT t.id, t.channel_id, t.root_message_id, t.title, t.status, t.created_at, t.updated_at, t.permission_mode FROM threads t JOIN channels c ON c.id = t.channel_id WHERE c.workspace_id = ?1 ORDER BY t.created_at, t.id",
    |row| -> rusqlite::Result<StorageResult<TaskThread>> {
        Ok(Ok(TaskThread {
            id: parse_id(&row.get::<_, String>(0)?)?,
            channel_id: parse_id(&row.get::<_, String>(1)?)?,
            root_message_id: parse_id(&row.get::<_, String>(2)?)?,
            title: row.get(3)?,
            status: from_json!(&row.get::<_, String>(4)?)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
            permission_mode: from_json!(&row.get::<_, String>(7)?)?,
        }))
    }
);
query_entities!(
    query_sessions,
    AgentSession,
    "SELECT s.id, s.thread_id, s.agent_id, s.provider_session_id, s.status, s.created_at FROM sessions s JOIN threads t ON t.id = s.thread_id JOIN channels c ON c.id = t.channel_id WHERE c.workspace_id = ?1 ORDER BY s.created_at, s.id",
    |row| -> rusqlite::Result<StorageResult<AgentSession>> {
        Ok(Ok(AgentSession {
            id: parse_id(&row.get::<_, String>(0)?)?,
            thread_id: parse_id(&row.get::<_, String>(1)?)?,
            agent_id: parse_id(&row.get::<_, String>(2)?)?,
            provider_session_id: row.get(3)?,
            status: from_json!(&row.get::<_, String>(4)?)?,
            created_at: row.get(5)?,
        }))
    }
);
query_entities!(
    query_runs,
    Run,
    "SELECT r.id, r.session_id, r.sequence, r.status, r.started_at, r.finished_at FROM runs r JOIN sessions s ON s.id = r.session_id JOIN threads t ON t.id = s.thread_id JOIN channels c ON c.id = t.channel_id WHERE c.workspace_id = ?1 ORDER BY r.sequence, r.id",
    |row| -> rusqlite::Result<StorageResult<Run>> {
        Ok(Ok(Run {
            id: parse_id(&row.get::<_, String>(0)?)?,
            session_id: parse_id(&row.get::<_, String>(1)?)?,
            sequence: row.get(2)?,
            status: from_json!(&row.get::<_, String>(3)?)?,
            started_at: row.get(4)?,
            finished_at: row.get(5)?,
        }))
    }
);
query_entities!(
    query_drafts,
    Draft,
    "SELECT d.channel_id, d.thread_id, d.body, d.updated_at FROM drafts d JOIN channels c ON c.id = d.channel_id WHERE c.workspace_id = ?1 ORDER BY d.updated_at, d.channel_id, d.thread_id",
    |row| -> rusqlite::Result<StorageResult<Draft>> {
        Ok(Ok(Draft {
            channel_id: parse_id(&row.get::<_, String>(0)?)?,
            thread_id: row
                .get::<_, Option<String>>(1)?
                .map(|id| parse_id(&id))
                .transpose()?,
            body: row.get(2)?,
            updated_at: row.get(3)?,
        }))
    }
);
query_entities!(
    query_memories,
    ChannelMemory,
    "SELECT m.id, m.channel_id, m.fact, m.source_message_id, m.supersedes_id, m.active, m.created_at FROM memories m JOIN channels c ON c.id = m.channel_id WHERE c.workspace_id = ?1 ORDER BY m.created_at, m.id",
    |row| -> rusqlite::Result<StorageResult<ChannelMemory>> {
        Ok(Ok(ChannelMemory {
            id: parse_id(&row.get::<_, String>(0)?)?,
            channel_id: parse_id(&row.get::<_, String>(1)?)?,
            fact: row.get(2)?,
            source_message_id: parse_id(&row.get::<_, String>(3)?)?,
            supersedes_id: row
                .get::<_, Option<String>>(4)?
                .map(|id| parse_id(&id))
                .transpose()?,
            active: row.get(5)?,
            created_at: row.get(6)?,
        }))
    }
);
query_entities!(
    query_worktrees,
    WorktreeRecord,
    "SELECT w.thread_id, w.path, w.branch, w.created_at FROM worktrees w JOIN threads t ON t.id = w.thread_id JOIN channels c ON c.id = t.channel_id WHERE c.workspace_id = ?1 ORDER BY w.created_at, w.thread_id",
    |row| -> rusqlite::Result<StorageResult<WorktreeRecord>> {
        Ok(Ok(WorktreeRecord {
            thread_id: parse_id(&row.get::<_, String>(0)?)?,
            path: from_json!(&row.get::<_, String>(1)?)?,
            branch: row.get(2)?,
            created_at: row.get(3)?,
        }))
    }
);
query_entities!(
    query_usages,
    RunUsage,
    "SELECT u.run_id, u.input_tokens, u.output_tokens, u.cached_tokens, u.cost_micros, u.duration_ms FROM run_usage u JOIN runs r ON r.id = u.run_id JOIN sessions s ON s.id = r.session_id JOIN threads t ON t.id = s.thread_id JOIN channels c ON c.id = t.channel_id WHERE c.workspace_id = ?1 ORDER BY u.run_id",
    |row| -> rusqlite::Result<StorageResult<RunUsage>> {
        Ok(Ok(RunUsage {
            run_id: parse_id(&row.get::<_, String>(0)?)?,
            input_tokens: row.get(1)?,
            output_tokens: row.get(2)?,
            cached_tokens: row.get(3)?,
            cost_micros: row.get(4)?,
            duration_ms: row.get(5)?,
        }))
    }
);

#[cfg(test)]
mod tests;
