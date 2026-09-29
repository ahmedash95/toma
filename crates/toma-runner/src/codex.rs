use crate::process::{CommandSpec, ProcessRunner};
use crate::transcript::{Transcript, pretty, tool_summary};
use crate::{AgentRunner, RunRequest, RunnerCapabilities, RunnerError, RunnerEvent, translate};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use toma_domain::{PermissionMode, RunId, RunnerProvider, TranscriptStep};

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

/// Adds the steps in an `exec --json` line. Completed items carry everything the started
/// ones did, plus the outcome.
pub(crate) fn transcript_line(transcript: &mut Transcript, line: &serde_json::Value) {
    let item = &line["item"];
    let text = |value: &serde_json::Value| value.as_str().unwrap_or_default().to_owned();
    match line["type"].as_str() {
        Some("item.completed") => {}
        Some("turn.failed") => {
            return transcript.push(TranscriptStep::Error {
                text: text(&line["error"]["message"]),
            });
        }
        Some("error") => {
            return transcript.push(TranscriptStep::Error {
                text: text(&line["message"]),
            });
        }
        _ => return,
    }
    let failed = item["status"] == "failed";
    match item["type"].as_str() {
        Some("agent_message") => transcript.push(TranscriptStep::Text {
            text: text(&item["text"]),
        }),
        Some("reasoning") => transcript.push(TranscriptStep::Thinking {
            text: text(&item["text"]),
        }),
        Some("command_execution") => {
            transcript.push(TranscriptStep::ToolCall {
                name: "Shell".into(),
                summary: tool_summary(&item["command"]),
                input: text(&item["command"]),
            });
            transcript.push(TranscriptStep::ToolResult {
                output: text(&item["aggregated_output"]),
                is_error: failed || item["exit_code"].as_i64().is_some_and(|code| code != 0),
            });
        }
        Some("file_change") => {
            let changes: Vec<String> = item["changes"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|change| format!("{} {}", text(&change["kind"]), text(&change["path"])))
                .collect();
            transcript.push(TranscriptStep::ToolCall {
                name: "Edit".into(),
                summary: match changes.as_slice() {
                    [one] => one.clone(),
                    many => format!("{} files", many.len()),
                },
                input: changes.join("\n"),
            });
            if failed {
                transcript.push(TranscriptStep::ToolResult {
                    output: "The change was not applied.".into(),
                    is_error: true,
                });
            }
        }
        Some("mcp_tool_call") => {
            transcript.push(TranscriptStep::ToolCall {
                name: format!("{}.{}", text(&item["server"]), text(&item["tool"])),
                summary: tool_summary(&item["arguments"]),
                input: pretty(&item["arguments"]),
            });
            let output = match item["error"]["message"].as_str() {
                Some(message) => message.to_owned(),
                None => pretty(&item["result"]),
            };
            transcript.push(TranscriptStep::ToolResult {
                output,
                is_error: failed || !item["error"].is_null(),
            });
        }
        Some("web_search") => transcript.push(TranscriptStep::ToolCall {
            name: "Web search".into(),
            summary: text(&item["query"]),
            input: text(&item["query"]),
        }),
        Some("todo_list") => transcript.push(TranscriptStep::Notice {
            text: item["items"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|todo| {
                    let mark = if todo["completed"] == true { "x" } else { " " };
                    format!("[{mark}] {}", text(&todo["text"]))
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }),
        Some("error") => transcript.push(TranscriptStep::Error {
            text: text(&item["message"]),
        }),
        _ => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_has_reasoning_commands_edits_and_replies() {
        let mut transcript = Transcript::new(RunnerProvider::CodexCli);
        for line in [
            r#"{"type":"thread.started","thread_id":"t"}"#,
            r#"{"type":"item.started","item":{"id":"i0","type":"command_execution","command":"ls","status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"i1","type":"reasoning","text":"**Listing files**"}}"#,
            r#"{"type":"item.completed","item":{"id":"i0","type":"command_execution","command":"ls","aggregated_output":"a.rs\n","exit_code":0,"status":"completed"}}"#,
            r#"{"type":"item.completed","item":{"id":"i2","type":"command_execution","command":"false","aggregated_output":"","exit_code":1,"status":"failed"}}"#,
            r#"{"type":"item.completed","item":{"id":"i3","type":"file_change","changes":[{"path":"a.rs","kind":"update"}],"status":"completed"}}"#,
            r#"{"type":"item.completed","item":{"id":"i4","type":"agent_message","text":"Done."}}"#,
            r#"{"type":"turn.failed","error":{"message":"stream disconnected"}}"#,
        ] {
            assert!(transcript.line(line, 0));
        }
        let steps: Vec<_> = transcript.finish().into_iter().map(|e| e.step).collect();
        assert_eq!(
            steps,
            vec![
                TranscriptStep::Thinking {
                    text: "**Listing files**".into()
                },
                TranscriptStep::ToolCall {
                    name: "Shell".into(),
                    summary: "ls".into(),
                    input: "ls".into()
                },
                TranscriptStep::ToolResult {
                    output: "a.rs".into(),
                    is_error: false
                },
                TranscriptStep::ToolCall {
                    name: "Shell".into(),
                    summary: "false".into(),
                    input: "false".into()
                },
                TranscriptStep::ToolResult {
                    output: "".into(),
                    is_error: true
                },
                TranscriptStep::ToolCall {
                    name: "Edit".into(),
                    summary: "update a.rs".into(),
                    input: "update a.rs".into()
                },
                TranscriptStep::Text {
                    text: "Done.".into()
                },
                TranscriptStep::Error {
                    text: "stream disconnected".into()
                },
            ]
        );
    }
}
