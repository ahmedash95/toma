use crate::approval::{self, ApprovalListener, Pending};
use crate::process::{CommandSpec, ProcessRunner};
use crate::transcript::{Transcript, pretty, tool_summary};
use crate::{AgentRunner, RunRequest, RunnerCapabilities, RunnerError, RunnerEvent, translate};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use toma_domain::{PermissionDecision, PermissionMode, RunId, RunnerProvider, TranscriptStep};

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

/// Adds the steps in a stream-json line. Reads the complete `assistant` and `user`
/// messages; the `stream_event` partials repeat them.
pub(crate) fn transcript_line(transcript: &mut Transcript, line: &serde_json::Value) {
    transcript.set_subagent(!line["parent_tool_use_id"].is_null());
    let blocks = line["message"]["content"].as_array().map(Vec::as_slice);
    match line["type"].as_str() {
        Some("assistant") => {
            for block in blocks.unwrap_or_default() {
                let text = |key: &str| block[key].as_str().unwrap_or_default().to_owned();
                match block["type"].as_str() {
                    // Usually empty: Claude often keeps its thinking to itself.
                    Some("thinking") => transcript.push(TranscriptStep::Thinking {
                        text: text("thinking"),
                    }),
                    Some("text") => transcript.push(TranscriptStep::Text { text: text("text") }),
                    Some("tool_use") => transcript.push(TranscriptStep::ToolCall {
                        name: text("name"),
                        summary: tool_summary(&block["input"]),
                        input: pretty(&block["input"]),
                    }),
                    _ => {}
                }
            }
        }
        Some("user") => {
            for block in blocks.unwrap_or_default() {
                if block["type"] == "tool_result" {
                    transcript.push(TranscriptStep::ToolResult {
                        output: tool_result_text(&block["content"]),
                        is_error: block["is_error"].as_bool().unwrap_or(false),
                    });
                }
            }
        }
        Some("result") if line["is_error"] == true => transcript.push(TranscriptStep::Error {
            text: line["result"]
                .as_str()
                .or(line["subtype"].as_str())
                .unwrap_or("The run failed")
                .to_owned(),
        }),
        _ => {}
    }
}

/// A tool result is plain text or a list of content blocks.
fn tool_result_text(content: &serde_json::Value) -> String {
    match content.as_array() {
        Some(blocks) => blocks
            .iter()
            .map(|block| match block["type"].as_str() {
                Some("text") => block["text"].as_str().unwrap_or_default().to_owned(),
                Some(kind) => format!("[{kind}]"),
                None => pretty(block),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => pretty(content),
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

#[cfg(test)]
mod tests {
    use super::*;
    use toma_domain::TranscriptEntry;

    fn steps(lines: &[&str]) -> Vec<TranscriptEntry> {
        let mut transcript = Transcript::new(RunnerProvider::ClaudeCodeCli);
        for (at, line) in lines.iter().enumerate() {
            assert!(transcript.line(line, at as i64));
        }
        transcript.finish()
    }

    #[test]
    fn transcript_has_thinking_tool_calls_results_and_text() {
        let entries = steps(&[
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Let"}}}"#,
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"thinking","thinking":"Check the tree.","signature":"x"}]}}"#,
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"thinking","thinking":"","signature":"x"}]}}"#,
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls","description":"List"}}]}}"#,
            r#"{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"a.rs\nb.rs","is_error":false}]}}"#,
            r#"{"type":"user","parent_tool_use_id":"t2","message":{"content":[{"type":"tool_result","tool_use_id":"t3","content":[{"type":"text","text":"nope"}],"is_error":true}]}}"#,
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Two files."}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Two files."}"#,
        ]);
        let steps: Vec<_> = entries
            .iter()
            .map(|e| (e.at, e.subagent, e.step.clone()))
            .collect();
        assert_eq!(
            steps,
            vec![
                (
                    2,
                    false,
                    TranscriptStep::Thinking {
                        text: "Check the tree.".into()
                    }
                ),
                (
                    4,
                    false,
                    TranscriptStep::ToolCall {
                        name: "Bash".into(),
                        summary: "ls".into(),
                        input: "{\n  \"command\": \"ls\",\n  \"description\": \"List\"\n}".into(),
                    }
                ),
                (
                    5,
                    false,
                    TranscriptStep::ToolResult {
                        output: "a.rs\nb.rs".into(),
                        is_error: false
                    }
                ),
                (
                    6,
                    true,
                    TranscriptStep::ToolResult {
                        output: "nope".into(),
                        is_error: true
                    }
                ),
                (
                    7,
                    false,
                    TranscriptStep::Text {
                        text: "Two files.".into()
                    }
                ),
            ]
        );
    }

    #[test]
    fn failed_result_is_an_error_step() {
        let entries = steps(&[r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#]);
        assert_eq!(
            entries[0].step,
            TranscriptStep::Error {
                text: "error_max_turns".into()
            }
        );
    }
}
