use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;
use toma_domain::*;

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
    Started { provider_session_id: Option<String> },
    Output(String),
    WaitingForInput(String),
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
}
