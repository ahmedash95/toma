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
