mod approval;
mod claude;
mod codex;
mod process;

pub use approval::run_approval_bridge;
pub use claude::ClaudeCodeRunner;
pub use codex::CodexCliRunner;
pub use process::find_executable;

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;
use toma_domain::*;

pub type ClaudeRunner = ClaudeCodeRunner;
pub type ClaudeCodeCliRunner = ClaudeCodeRunner;
pub type CodexRunner = CodexCliRunner;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunnerCapabilities {
    pub resume: bool,
    pub streaming: bool,
    pub cancellation: bool,
    pub permission_requests: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunRequest<'a> {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub provider_session_id: Option<&'a str>,
    pub working_directory: &'a Path,
    pub prompt: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunnerEvent {
    Started {
        provider_session_id: Option<String>,
    },
    Output(String),
    /// The agent's user-facing answer, extracted from the provider's raw output.
    Reply(String),
    /// A streamed fragment of the reply, ahead of the final `Reply`.
    ReplyDelta(String),
    WaitingForInput(String),
    /// Totals the provider reports when a run ends.
    Usage {
        input_tokens: i64,
        output_tokens: i64,
        cached_tokens: i64,
        cost_micros: Option<i64>,
        duration_ms: Option<i64>,
    },
    /// The agent is blocked until the person allows or denies `tool`.
    PermissionRequest {
        request_id: String,
        tool: String,
        detail: String,
    },
    Completed,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("runner is unavailable: {0}")]
    Unavailable(String),
    #[error("runner I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("runner protocol failed: {0}")]
    Protocol(String),
}

pub trait AgentRunner: Send + Sync {
    fn provider(&self) -> RunnerProvider;
    fn capabilities(&self) -> RunnerCapabilities;
    fn availability(&self) -> Result<(), RunnerError>;
    fn run(
        &self,
        request: RunRequest<'_>,
        emit: &mut dyn FnMut(RunnerEvent),
    ) -> Result<(), RunnerError>;
    fn cancel(&self, run_id: RunId) -> Result<(), RunnerError>;
    fn answer_permission(
        &self,
        _run_id: RunId,
        _request_id: &str,
        _allow: bool,
    ) -> Result<(), RunnerError> {
        Err(RunnerError::Protocol(
            "this runner does not ask for permission".into(),
        ))
    }
}

/// Forwards `event`, followed by whatever `parse` derives from it when it is a JSON output line.
pub(crate) fn translate(
    event: RunnerEvent,
    emit: &mut dyn FnMut(RunnerEvent),
    parse: fn(&serde_json::Value) -> Vec<RunnerEvent>,
) {
    let derived = match &event {
        RunnerEvent::Output(line) => serde_json::from_str(line)
            .map(|value| parse(&value))
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    emit(event);
    derived.into_iter().for_each(emit);
}
