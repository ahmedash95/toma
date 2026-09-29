use crate::process::{CommandSpec, ProcessRunner};
use crate::transcript::{Transcript, pretty, tool_summary};
use crate::{AgentRunner, RunRequest, RunnerCapabilities, RunnerError, RunnerEvent, translate};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use toma_domain::{PermissionMode, RunId, RunnerProvider, TranscriptStep};

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

/// Adds the steps in a stream-json line. Text and thinking arrive as fragments; the
/// flush and final `assistant` lines repeat them, as in `cursor_events`.
pub(crate) fn transcript_line(transcript: &mut Transcript, line: &serde_json::Value) {
    match (line["type"].as_str(), line["subtype"].as_str()) {
        (Some("thinking"), Some("delta")) => {
            transcript.append_thinking(line["text"].as_str().unwrap_or_default())
        }
        (Some("assistant"), _)
            if line["model_call_id"].is_null() && !line["timestamp_ms"].is_null() =>
        {
            transcript.append_text(
                line["message"]["content"][0]["text"]
                    .as_str()
                    .unwrap_or_default(),
            )
        }
        (Some("tool_call"), Some(subtype)) => {
            // `{"shellToolCall": {"args": …, "result": …}}`, or a generic `function` call.
            let Some((kind, call)) = line["tool_call"]
                .as_object()
                .and_then(|call| call.iter().next())
            else {
                return;
            };
            match subtype {
                "started" => {
                    let (name, args) = match kind.as_str() {
                        "function" => {
                            let arguments = &call["arguments"];
                            let parsed = arguments
                                .as_str()
                                .and_then(|text| serde_json::from_str(text).ok())
                                .unwrap_or_else(|| arguments.clone());
                            (call["name"].as_str().unwrap_or("tool").to_owned(), parsed)
                        }
                        kind => (tool_name(kind), call["args"].clone()),
                    };
                    transcript.push(TranscriptStep::ToolCall {
                        name,
                        summary: tool_summary(&args),
                        input: pretty(&args),
                    });
                }
                "completed" => {
                    let result = &call["result"];
                    let (output, is_error) = match result.as_object().and_then(|r| r.iter().next())
                    {
                        Some((outcome, detail)) => (tool_output(detail), outcome != "success"),
                        None => (pretty(result), false),
                    };
                    transcript.push(TranscriptStep::ToolResult { output, is_error });
                }
                _ => {}
            }
        }
        (Some("result"), _) if line["is_error"] == true => transcript.push(TranscriptStep::Error {
            text: line["result"]
                .as_str()
                .unwrap_or("The run failed")
                .to_owned(),
        }),
        _ => {}
    }
}

/// `readToolCall` → `Read`.
fn tool_name(kind: &str) -> String {
    let name = kind.strip_suffix("ToolCall").unwrap_or(kind);
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Shell output, file content, or the outcome's fields when it has neither.
fn tool_output(detail: &serde_json::Value) -> String {
    let streams: Vec<&str> = ["stdout", "stderr"]
        .iter()
        .filter_map(|key| detail[key].as_str())
        .filter(|text| !text.trim().is_empty())
        .collect();
    if !streams.is_empty() {
        return streams.join("\n");
    }
    detail["content"]
        .as_str()
        .or(detail["message"].as_str())
        .or(detail["error"].as_str())
        .map_or_else(|| pretty(detail), str::to_owned)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_joins_fragments_around_tool_calls() {
        let mut transcript = Transcript::new(RunnerProvider::CursorCli);
        for line in [
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            r#"{"type":"thinking","subtype":"delta","text":"Look at "}"#,
            r#"{"type":"thinking","subtype":"delta","text":"the file."}"#,
            r#"{"type":"thinking","subtype":"completed"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reading "}]},"timestamp_ms":1}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"it."}]},"timestamp_ms":2}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reading it."}]},"model_call_id":"m"}"#,
            r#"{"type":"tool_call","subtype":"started","call_id":"c","tool_call":{"readToolCall":{"args":{"path":"a.rs"}}}}"#,
            r#"{"type":"tool_call","subtype":"completed","call_id":"c","tool_call":{"readToolCall":{"args":{"path":"a.rs"},"result":{"success":{"content":"fn main() {}"}}}}}"#,
            r#"{"type":"tool_call","subtype":"completed","call_id":"d","tool_call":{"shellToolCall":{"args":{"command":"x"},"result":{"failure":{"stdout":"","stderr":"not found","exitCode":127}}}}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]},"timestamp_ms":3}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reading it.Done."}]}}"#,
        ] {
            assert!(transcript.line(line, 0));
        }
        let steps: Vec<_> = transcript.finish().into_iter().map(|e| e.step).collect();
        assert_eq!(
            steps,
            vec![
                TranscriptStep::Thinking {
                    text: "Look at the file.".into()
                },
                TranscriptStep::Text {
                    text: "Reading it.".into()
                },
                TranscriptStep::ToolCall {
                    name: "Read".into(),
                    summary: "a.rs".into(),
                    input: "{\n  \"path\": \"a.rs\"\n}".into(),
                },
                TranscriptStep::ToolResult {
                    output: "fn main() {}".into(),
                    is_error: false
                },
                TranscriptStep::ToolResult {
                    output: "not found".into(),
                    is_error: true
                },
                TranscriptStep::Text {
                    text: "Done.".into()
                },
            ]
        );
    }
}
