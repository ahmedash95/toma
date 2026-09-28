use crate::process::{CommandSpec, ProcessRunner};
use crate::{AgentRunner, RunRequest, RunnerCapabilities, RunnerError, RunnerEvent};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use toma_domain::{RunId, RunnerProvider};

#[derive(Clone)]
pub struct ClaudeCodeRunner {
    process: ProcessRunner,
}

impl ClaudeCodeRunner {
    pub fn new() -> Self {
        Self {
            process: ProcessRunner::discover("claude"),
        }
    }

    pub fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self {
            process: ProcessRunner::with_executable(executable),
        }
    }

    pub fn executable(&self) -> &Path {
        self.process.executable()
    }

    fn command(request: &RunRequest<'_>) -> CommandSpec {
        let mut arguments = vec![
            OsString::from("--print"),
            OsString::from("--output-format"),
            OsString::from("stream-json"),
            OsString::from("--verbose"),
            OsString::from("--permission-mode"),
            OsString::from("dontAsk"),
            OsString::from("--permission-prompts"),
            OsString::from("none"),
        ];
        if let Some(session_id) = request.provider_session_id {
            arguments.push(OsString::from("--resume"));
            arguments.push(OsString::from(session_id));
        }
        arguments.push(OsString::from(request.prompt));
        CommandSpec { arguments }
    }
}

impl Default for ClaudeCodeRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRunner for ClaudeCodeRunner {
    fn provider(&self) -> RunnerProvider {
        RunnerProvider::ClaudeCodeCli
    }

    fn capabilities(&self) -> RunnerCapabilities {
        RunnerCapabilities {
            resume: true,
            streaming: true,
            cancellation: true,
            permission_requests: false,
        }
    }

    fn availability(&self) -> Result<(), RunnerError> {
        self.process.availability()
    }

    fn run(
        &self,
        request: RunRequest<'_>,
        emit: &mut dyn FnMut(RunnerEvent),
    ) -> Result<(), RunnerError> {
        let command = Self::command(&request);
        self.process.run(request, command, emit)
    }

    fn cancel(&self, run_id: RunId) -> Result<(), RunnerError> {
        self.process.cancel(run_id)
    }
}
