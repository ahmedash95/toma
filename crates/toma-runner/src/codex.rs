use crate::process::{CommandSpec, ProcessRunner};
use crate::{AgentRunner, RunRequest, RunnerCapabilities, RunnerError, RunnerEvent, translate};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use toma_domain::{PermissionMode, RunId, RunnerProvider};

#[derive(Clone)]
pub struct CodexCliRunner {
    process: ProcessRunner,
}

impl CodexCliRunner {
    pub fn new() -> Self {
        Self {
            process: ProcessRunner::discover("codex"),
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
        let mut arguments = vec![OsString::from("exec")];
        // Codex's own modes: sandboxed writes, its automatic reviewer, or read-only.
        arguments.extend(
            match request.permission_mode {
                PermissionMode::Ask => &["--sandbox", "workspace-write"][..],
                PermissionMode::Auto => &["--approve-for-me"],
                PermissionMode::Plan => &["--sandbox", "read-only"],
            }
            .iter()
            .map(OsString::from),
        );
        if let Some(session_id) = request.provider_session_id {
            arguments.push(OsString::from("resume"));
            arguments.push(OsString::from("--json"));
            arguments.push(OsString::from(session_id));
        } else {
            arguments.push(OsString::from("--json"));
        }
        arguments.push(OsString::from(request.prompt));
        CommandSpec {
            arguments,
            events: None,
        }
    }
}

/// Maps an `exec --json` line to the events it implies.
fn codex_event(line: &serde_json::Value) -> Option<RunnerEvent> {
    match line["type"].as_str()? {
        "thread.started" => Some(RunnerEvent::Started {
            provider_session_id: line["thread_id"].as_str().map(str::to_owned),
        }),
        "item.completed" if line["item"]["type"] == "agent_message" => Some(RunnerEvent::Reply(
            line["item"]["text"].as_str()?.to_owned(),
        )),
        _ => None,
    }
}

impl Default for CodexCliRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRunner for CodexCliRunner {
    fn provider(&self) -> RunnerProvider {
        RunnerProvider::CodexCli
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
        self.process.run(request, command, &mut |event| {
            translate(event, emit, |line| codex_event(line).into_iter().collect())
        })
    }

    fn cancel(&self, run_id: RunId) -> Result<(), RunnerError> {
        self.process.cancel(run_id)
    }
}
