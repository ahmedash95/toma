use crate::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub repository_path: PathBuf,
    pub created_at: TimestampMs,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub position: i64,
    /// Folder agents work in; `None` uses the workspace repository.
    pub repository_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Person {
    pub id: PersonId,
    pub workspace_id: WorkspaceId,
    pub display_name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerProvider {
    CodexCli,
    ClaudeCodeCli,
    CursorCli,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub id: AgentId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub role: String,
    pub instructions: String,
    pub provider: RunnerProvider,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageAuthor {
    Person(PersonId),
    Agent(AgentId),
    System,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub channel_id: ChannelId,
    pub thread_id: Option<ThreadId>,
    pub author: MessageAuthor,
    pub body: String,
    pub created_at: TimestampMs,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachmentTarget {
    Agent {
        agent_id: AgentId,
    },
    RepositoryFile {
        path: PathBuf,
        revision: Option<String>,
    },
    Symbol {
        path: PathBuf,
        symbol: String,
    },
    Skill {
        name: String,
        version: String,
    },
    Thread {
        thread_id: ThreadId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: AttachmentId,
    pub message_id: MessageId,
    pub target: AttachmentTarget,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskThread {
    pub id: ThreadId,
    pub channel_id: ChannelId,
    pub root_message_id: MessageId,
    pub title: String,
    pub status: WorkStatus,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
    pub permission_mode: PermissionMode,
}

/// How much agents in a thread may do without asking. Each provider maps it to its own
/// native mode (Claude `--permission-mode`, Codex sandbox/`--approve-for-me`, Cursor `--mode`).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Edits in the working folder proceed; commands and anything outside ask the person.
    #[default]
    Ask,
    /// The provider's own reviewer answers for the person.
    Auto,
    /// Read-only: the agent analyzes and proposes, without editing.
    Plan,
}

/// The person's answer to a permission request, sent back through the provider's protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Deny,
    Allow,
    /// Allow, and stop asking about the same command (or tool) for this session.
    AlwaysAllow,
    /// Allow, and let the provider's reviewer answer from now on.
    SwitchToAuto,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: SessionId,
    pub thread_id: ThreadId,
    pub agent_id: AgentId,
    pub provider_session_id: Option<String>,
    pub status: WorkStatus,
    pub created_at: TimestampMs,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub session_id: SessionId,
    pub sequence: i64,
    pub status: WorkStatus,
    pub started_at: Option<TimestampMs>,
    pub finished_at: Option<TimestampMs>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    pub channel_id: ChannelId,
    pub thread_id: Option<ThreadId>,
    pub body: String,
    pub updated_at: TimestampMs,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChannelMemory {
    pub id: MemoryId,
    pub channel_id: ChannelId,
    pub fact: String,
    pub source_message_id: MessageId,
    pub supersedes_id: Option<MemoryId>,
    pub active: bool,
    pub created_at: TimestampMs,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorktreeRecord {
    pub thread_id: ThreadId,
    pub path: PathBuf,
    pub branch: String,
    pub created_at: TimestampMs,
}

/// An agent action waiting for the person to allow or deny it. Lives only as long as the run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub run_id: RunId,
    pub thread_id: ThreadId,
    pub agent_id: AgentId,
    pub tool: String,
    pub detail: String,
    /// What "Always allow" would cover, such as `git push` or `Write`.
    pub rule: String,
}

/// What a run consumed, as reported by the provider at the end of the run.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunUsage {
    pub run_id: RunId,
    /// All prompt tokens, including cache reads and writes.
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    /// Millionths of a US dollar, when the provider reports cost.
    pub cost_micros: Option<i64>,
    pub duration_ms: Option<i64>,
}
