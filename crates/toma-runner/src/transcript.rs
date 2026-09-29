use crate::{claude, codex, cursor};
use toma_domain::{RunnerProvider, TimestampMs, TranscriptEntry, TranscriptStep};

/// Rebuilds a run's steps (reasoning summaries, tool calls and results, replies) from the
/// raw output lines it recorded. Built on demand, so nothing beyond the raw lines is stored.
pub struct Transcript {
    parse: fn(&mut Transcript, &serde_json::Value),
    entries: Vec<TranscriptEntry>,
    at: TimestampMs,
    subagent: bool,
}

impl Transcript {
    pub fn new(provider: RunnerProvider) -> Self {
        Self {
            parse: match provider {
                RunnerProvider::ClaudeCodeCli => claude::transcript_line,
                RunnerProvider::CodexCli => codex::transcript_line,
                RunnerProvider::CursorCli => cursor::transcript_line,
            },
            entries: Vec::new(),
            at: 0,
            subagent: false,
        }
    }

    /// Adds a provider output line recorded at `at`. Returns false when the line is not
    /// provider JSON, so the caller can interpret its own records.
    pub fn line(&mut self, line: &str, at: TimestampMs) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        if !value.is_object() {
            return false;
        }
        self.at = at;
        self.subagent = false;
        (self.parse)(self, &value);
        true
    }

    pub fn push(&mut self, step: TranscriptStep) {
        self.entries.push(TranscriptEntry {
            at: self.at,
            subagent: self.subagent,
            step,
        });
    }

    pub fn push_at(&mut self, step: TranscriptStep, at: TimestampMs) {
        self.at = at;
        self.subagent = false;
        self.push(step);
    }

    pub(crate) fn set_subagent(&mut self, subagent: bool) {
        self.subagent = subagent;
    }

    /// Extends the previous text step, for providers that stream text in fragments.
    pub(crate) fn append_text(&mut self, fragment: &str) {
        match self.entries.last_mut() {
            Some(TranscriptEntry {
                step: TranscriptStep::Text { text },
                ..
            }) => text.push_str(fragment),
            _ => self.push(TranscriptStep::Text {
                text: fragment.to_owned(),
            }),
        }
    }

    /// Extends the previous thinking step, for providers that stream it in fragments.
    pub(crate) fn append_thinking(&mut self, fragment: &str) {
        match self.entries.last_mut() {
            Some(TranscriptEntry {
                step: TranscriptStep::Thinking { text },
                ..
            }) => text.push_str(fragment),
            _ => self.push(TranscriptStep::Thinking {
                text: fragment.to_owned(),
            }),
        }
    }

    /// The steps, with blank ones dropped and credentials that look well known redacted.
    pub fn finish(self) -> Vec<TranscriptEntry> {
        self.entries
            .into_iter()
            .filter_map(|mut entry| {
                let texts: Vec<&mut String> = match &mut entry.step {
                    TranscriptStep::Thinking { text }
                    | TranscriptStep::Text { text }
                    | TranscriptStep::Notice { text }
                    | TranscriptStep::Error { text } => vec![text],
                    TranscriptStep::ToolCall { summary, input, .. } => vec![summary, input],
                    TranscriptStep::ToolResult { output, .. } => vec![output],
                };
                for text in texts {
                    *text = redact(text.trim());
                }
                let blank = match &entry.step {
                    TranscriptStep::Thinking { text } | TranscriptStep::Text { text } => {
                        text.is_empty()
                    }
                    _ => false,
                };
                (!blank).then_some(entry)
            })
            .collect()
    }
}

/// One line describing a tool call: the first of the arguments people recognize.
pub(crate) fn tool_summary(input: &serde_json::Value) -> String {
    const KEYS: &[&str] = &[
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
        "prompt",
    ];
    let summary = KEYS
        .iter()
        .find_map(|key| input[key].as_str())
        .map(str::to_owned)
        .or_else(|| input.as_str().map(str::to_owned))
        .unwrap_or_default();
    let line = summary.lines().next().unwrap_or_default();
    match line.char_indices().nth(160) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None if line.len() < summary.trim_end().len() => format!("{line}…"),
        None => line.to_owned(),
    }
}

pub(crate) fn pretty(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(text) => text.clone(),
        _ => serde_json::to_string_pretty(value).unwrap_or_default(),
    }
}

/// Well-known credential prefixes; the secret after them is replaced.
const SECRET_PREFIXES: &[&str] = &[
    "sk-ant-",
    "sk-proj-",
    "sk-",
    "sk_live_",
    "rk_live_",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxs-",
    "AKIA",
    "ASIA",
    "AIza",
    "Bearer ",
];

/// Hides tool output that looks like a credential: prefixed tokens, bearer tokens and
/// PEM private keys. Best effort; it can't recognize every secret.
pub fn redact(text: &str) -> String {
    let text = redact_private_keys(text);
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let at_boundary = out
            .chars()
            .next_back()
            .is_none_or(|previous| !previous.is_ascii_alphanumeric() && previous != '_');
        let secret = at_boundary
            .then(|| {
                SECRET_PREFIXES.iter().find_map(|prefix| {
                    let tail = rest.strip_prefix(prefix)?;
                    let length = tail
                        .find(|c: char| !(c.is_ascii_alphanumeric() || "-_.=+/".contains(c)))
                        .unwrap_or(tail.len());
                    (length >= 16).then_some((prefix.len(), prefix.len() + length))
                })
            })
            .flatten();
        match secret {
            Some((prefix, end)) => {
                out.push_str(&rest[..prefix]);
                out.push_str("[redacted]");
                rest = &rest[end..];
            }
            None => {
                let next = rest.chars().next().expect("rest is not empty");
                out.push(next);
                rest = &rest[next.len_utf8()..];
            }
        }
    }
    out
}

fn redact_private_keys(text: &str) -> String {
    const BEGIN: &str = "-----BEGIN ";
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        let header_end = rest[start..].find("-----\n").or_else(|| {
            rest[start + BEGIN.len()..]
                .find("-----")
                .map(|end| end + BEGIN.len())
        });
        let Some(header_end) = header_end.map(|end| start + end + 5) else {
            break;
        };
        let header = &rest[start..header_end];
        if !header.contains("PRIVATE KEY") {
            out.push_str(&rest[..header_end]);
            rest = &rest[header_end..];
            continue;
        }
        let end = rest[header_end..]
            .find("-----END ")
            .and_then(|end| {
                let footer = header_end + end;
                rest[footer + 9..]
                    .find("-----")
                    .map(|close| footer + 9 + close + 5)
            })
            .unwrap_or(rest.len());
        out.push_str(&rest[..start]);
        out.push_str("[redacted private key]");
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_prefixed_tokens_and_private_keys() {
        assert_eq!(
            redact("export GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789 done"),
            "export GITHUB_TOKEN=ghp_[redacted] done"
        );
        assert_eq!(
            redact("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig"),
            "Authorization: Bearer [redacted]"
        );
        assert_eq!(
            redact("key sk-ant-api03-AAAAAAAAAAAAAAAAAAAA"),
            "key sk-ant-[redacted]"
        );
        // Too short to be a credential, or inside a word.
        assert_eq!(redact("task-list and ask-me"), "task-list and ask-me");
        assert_eq!(
            redact("desk-0123456789abcdefghij"),
            "desk-0123456789abcdefghij"
        );
        assert_eq!(
            redact("a\n-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----\nb"),
            "a\n[redacted private key]\nb"
        );
        assert_eq!(
            redact("-----BEGIN CERTIFICATE-----\nMIIC"),
            "-----BEGIN CERTIFICATE-----\nMIIC"
        );
    }

    #[test]
    fn summarizes_tool_input_by_its_most_telling_argument() {
        let input = serde_json::json!({"description": "List files", "command": "ls -la\npwd"});
        assert_eq!(tool_summary(&input), "ls -la…");
        assert_eq!(tool_summary(&serde_json::json!({"x": 1})), "");
    }

    #[test]
    fn non_json_lines_are_left_to_the_caller() {
        let mut transcript = Transcript::new(RunnerProvider::ClaudeCodeCli);
        assert!(!transcript.line("failed:boom", 1));
        assert!(!transcript.line("42", 1));
        transcript.push_at(
            TranscriptStep::Error {
                text: "boom".into(),
            },
            5,
        );
        assert_eq!(
            transcript.finish(),
            vec![TranscriptEntry {
                at: 5,
                subagent: false,
                step: TranscriptStep::Error {
                    text: "boom".into()
                }
            }]
        );
    }
}
