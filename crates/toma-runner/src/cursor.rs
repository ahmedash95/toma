use crate::process::{CommandSpec, ProcessRunner};
use crate::{AgentRunner, RunRequest, RunnerCapabilities, RunnerError, RunnerEvent, translate};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use toma_domain::{PermissionMode, RunId, RunnerProvider};

#[derive(Clone)]
pub struct CursorCliRunner {
    process: ProcessRunner,
}

impl CursorCliRunner {
    pub fn new() -> Self {
        Self {
            process: ProcessRunner::discover("cursor-agent"),
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

    fn command(&self, request: &RunRequest<'_>) -> CommandSpec {
        // Cursor has no permission-prompt hook. Verified headless: by default shell commands
        // are rejected (file edits still work through its edit tools); with
        // `--sandbox enabled` shell commands run inside Cursor's sandbox. So we use the
        // sandbox and never `--force`/`--yolo`.
        let mut arguments: Vec<OsString> = [
            "-p",
            "--trust",
            "--sandbox",
            "enabled",
            "--output-format",
            "stream-json",
            "--stream-partial-output",
        ]
        .map(OsString::from)
        .into();
        // Cursor has no automatic reviewer, so Auto behaves like Ask (sandboxed commands).
        if request.permission_mode == PermissionMode::Plan {
            arguments.extend(["--mode".into(), "plan".into()]);
        }
        if let Some(session_id) = request.provider_session_id {
            arguments.push("--resume".into());
            arguments.push(session_id.into());
        }
        arguments.push(request.prompt.into());
        CommandSpec {
            arguments,
            events: None,
        }
    }
}

/// Maps a `--output-format stream-json --stream-partial-output` line to events.
///
/// `assistant` lines come in three kinds: streamed deltas (`timestamp_ms`, no
/// `model_call_id`), a flush repeating the text so far before a tool call (has
/// `model_call_id`; emitted as a paragraph separator only), and the final full message
/// (no `timestamp_ms`; ignored, `result` carries the same text).
fn cursor_events(line: &serde_json::Value) -> Vec<RunnerEvent> {
    let tokens = |key: &str| line["usage"][key].as_i64().unwrap_or(0);
    match (line["type"].as_str(), line["subtype"].as_str()) {
        (Some("system"), Some("init")) => vec![RunnerEvent::Started {
            provider_session_id: line["session_id"].as_str().map(str::to_owned),
        }],
        (Some("assistant"), _) if !line["model_call_id"].is_null() => {
            vec![RunnerEvent::ReplyDelta("\n\n".into())]
        }
        (Some("assistant"), _) if !line["timestamp_ms"].is_null() => line["message"]["content"][0]
            ["text"]
            .as_str()
            .map(|text| RunnerEvent::ReplyDelta(text.to_owned()))
            .into_iter()
            .collect(),
        (Some("result"), _) => {
            let mut events: Vec<_> = line["result"]
                .as_str()
                .map(|text| RunnerEvent::Reply(text.to_owned()))
                .into_iter()
                .collect();
            // `inputTokens` excludes cache reads/writes (input 8725 + cacheRead 3904 for a
            // ~12.6k-token prompt), so the total input is the sum, as in the Claude runner.
            events.push(RunnerEvent::Usage {
                input_tokens: tokens("inputTokens")
                    + tokens("cacheReadTokens")
                    + tokens("cacheWriteTokens"),
                output_tokens: tokens("outputTokens"),
                cached_tokens: tokens("cacheReadTokens"),
                cost_micros: None,
                duration_ms: line["duration_ms"].as_i64(),
            });
            events
        }
        _ => Vec::new(),
    }
}

impl Default for CursorCliRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRunner for CursorCliRunner {
    fn provider(&self) -> RunnerProvider {
        RunnerProvider::CursorCli
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
        let command = self.command(&request);
        self.process.run(request, command, &mut |event| {
            translate(event, emit, cursor_events)
        })
    }

    fn cancel(&self, run_id: RunId) -> Result<(), RunnerError> {
        self.process.cancel(run_id)
    }
}
