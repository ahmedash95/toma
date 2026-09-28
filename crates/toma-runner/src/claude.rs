use crate::approval::{self, ApprovalListener, Pending};
use crate::process::{CommandSpec, ProcessRunner};
use crate::{AgentRunner, RunRequest, RunnerCapabilities, RunnerError, RunnerEvent, translate};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use toma_domain::{PermissionDecision, PermissionMode, RunId, RunnerProvider};

#[derive(Clone)]
pub struct ClaudeCodeRunner {
    process: ProcessRunner,
    /// Executable serving `mcp-approval`; without it, prompts are denied automatically.
    approval_bridge: Option<PathBuf>,
    pending: Pending,
    permission_mode: String,
}

impl ClaudeCodeRunner {
    pub fn new() -> Self {
        Self::from_process(ProcessRunner::discover("claude"))
    }

    pub fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self::from_process(ProcessRunner::with_executable(executable))
    }

    fn from_process(process: ProcessRunner) -> Self {
        Self {
            process,
            approval_bridge: None,
            pending: Pending::default(),
            permission_mode: "auto".into(),
        }
    }

    /// Routes permission prompts to the person through `executable mcp-approval <socket>`.
    /// Uses `acceptEdits`: edits in the working folder go ahead, commands and anything
    /// outside the folder wait for the person. (`auto` never asks, so it would bypass them.)
    pub fn with_approvals(mut self, executable: impl Into<PathBuf>) -> Self {
        self.approval_bridge = Some(executable.into());
        self.permission_mode = "acceptEdits".into();
        self
    }

    /// Any `claude --permission-mode` value.
    pub fn with_permission_mode(mut self, mode: impl Into<String>) -> Self {
        self.permission_mode = mode.into();
        self
    }

    pub fn executable(&self) -> &Path {
        self.process.executable()
    }

    fn command(&self, request: &RunRequest<'_>, socket: Option<&Path>) -> CommandSpec {
        let mut arguments = vec![
            OsString::from("--print"),
            OsString::from("--output-format"),
            OsString::from("stream-json"),
            OsString::from("--verbose"),
            OsString::from("--include-partial-messages"),
            OsString::from("--permission-mode"),
            OsString::from(match request.permission_mode {
                PermissionMode::Ask => self.permission_mode.as_str(),
                PermissionMode::Auto => "auto",
                PermissionMode::Plan => "plan",
            }),
        ];
        match (&self.approval_bridge, socket) {
            (Some(bridge), Some(socket)) => {
                let config = serde_json::json!({ "mcpServers": { "toma": {
                    "command": bridge,
                    "args": ["mcp-approval", socket],
                }}});
                arguments.extend([
                    OsString::from("--mcp-config"),
                    OsString::from(config.to_string()),
                    OsString::from("--permission-prompt-tool"),
                    OsString::from(format!("mcp__toma__{}", approval::TOOL)),
                    OsString::from("--permission-prompts"),
                    OsString::from("host"),
                ]);
            }
            _ => arguments.extend([
                OsString::from("--permission-prompts"),
                OsString::from("none"),
            ]),
        }
        if let Some(session_id) = request.provider_session_id {
            arguments.push(OsString::from("--resume"));
            arguments.push(OsString::from(session_id));
        }
        arguments.push(OsString::from(request.prompt));
        CommandSpec {
            arguments,
            events: None,
        }
    }
}

/// Maps a `--output-format stream-json` line to the events it implies.
fn claude_events(line: &serde_json::Value) -> Vec<RunnerEvent> {
    if line["type"] != "result" {
        return claude_event(line).into_iter().collect();
    }
    let usage = &line["usage"];
    let tokens = |key: &str| usage[key].as_i64().unwrap_or(0);
    let mut events: Vec<_> = line["result"]
        .as_str()
        .map(|text| RunnerEvent::Reply(text.to_owned()))
        .into_iter()
        .collect();
    events.push(RunnerEvent::Usage {
        input_tokens: tokens("input_tokens")
            + tokens("cache_creation_input_tokens")
            + tokens("cache_read_input_tokens"),
        output_tokens: tokens("output_tokens"),
        cached_tokens: tokens("cache_read_input_tokens"),
        cost_micros: line["total_cost_usd"]
            .as_f64()
            .map(|usd| (usd * 1_000_000.0).round() as i64),
        duration_ms: line["duration_ms"].as_i64(),
    });
    events
}

fn claude_event(line: &serde_json::Value) -> Option<RunnerEvent> {
    match (line["type"].as_str()?, line["subtype"].as_str()) {
        ("system", Some("init")) => Some(RunnerEvent::Started {
            provider_session_id: line["session_id"].as_str().map(str::to_owned),
        }),
        ("stream_event", _) => {
            let event = &line["event"];
            match event["type"].as_str()? {
                "content_block_delta" if event["delta"]["type"] == "text_delta" => Some(
                    RunnerEvent::ReplyDelta(event["delta"]["text"].as_str()?.to_owned()),
                ),
                // Separates text from successive turns (e.g. around tool calls).
                "content_block_start" if event["content_block"]["type"] == "text" => {
                    Some(RunnerEvent::ReplyDelta("\n\n".into()))
                }
                _ => None,
            }
        }
        _ => None,
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
            permission_requests: self.approval_bridge.is_some(),
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
        // The listener lives exactly as long as the Claude process it serves.
        let (events, listener) = match self.approval_bridge {
            Some(_) => {
                let (sender, receiver) = mpsc::channel();
                let listener =
                    ApprovalListener::start(request.run_id, self.pending.clone(), sender)?;
                (Some(receiver), Some(listener))
            }
            None => (None, None),
        };
        let mut command = self.command(&request, listener.as_ref().map(|l| l.socket.as_path()));
        command.events = events;
        self.process.run(request, command, &mut |event| {
            translate(event, emit, claude_events)
        })
    }

    fn cancel(&self, run_id: RunId) -> Result<(), RunnerError> {
        self.process.cancel(run_id)
    }

    fn answer_permission(
        &self,
        _run_id: RunId,
        request_id: &str,
        decision: PermissionDecision,
    ) -> Result<(), RunnerError> {
        let answer = self
            .pending
            .lock()
            .map_err(|_| RunnerError::Protocol("approval lock poisoned".into()))?
            .remove(request_id)
            .ok_or_else(|| RunnerError::Protocol(format!("no pending request {request_id}")))?;
        answer
            .send(decision)
            .map_err(|_| RunnerError::Protocol("the run stopped waiting".into()))
    }
}
