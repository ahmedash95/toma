//! Routes Claude's permission prompts to the person using Toma.
//!
//! Claude asks an MCP tool (`--permission-prompt-tool`) whether an action may run. Toma serves
//! that tool from its own executable (`toma mcp-approval <socket>`), which forwards each
//! question over a Unix socket to the run that launched Claude and waits for the answer.

use crate::RunnerEvent;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use toma_domain::{PermissionDecision, RunId};

pub(crate) const TOOL: &str = "approve";

/// Answers awaited by pending permission prompts, keyed by request id.
pub(crate) type Pending = Arc<Mutex<HashMap<String, mpsc::Sender<PermissionDecision>>>>;

/// Serves one run's permission prompts until dropped.
pub(crate) struct ApprovalListener {
    pub(crate) socket: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ApprovalListener {
    pub(crate) fn start(
        run_id: RunId,
        pending: Pending,
        events: mpsc::Sender<RunnerEvent>,
    ) -> io::Result<Self> {
        let socket = std::env::temp_dir().join(format!("toma-{run_id}.sock"));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                let mut count = 0;
                while !stop.load(Ordering::Acquire) {
                    let Ok((stream, _)) = listener.accept() else {
                        thread::sleep(Duration::from_millis(50));
                        continue;
                    };
                    count += 1;
                    let request_id = format!("{run_id}-{count}");
                    let _ = serve(stream, &request_id, &pending, &events, &stop);
                    pending.lock().map(|mut map| map.remove(&request_id)).ok();
                }
            }
        });
        Ok(Self {
            socket,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for ApprovalListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn serve(
    stream: UnixStream,
    request_id: &str,
    pending: &Pending,
    events: &mpsc::Sender<RunnerEvent>,
    stop: &AtomicBool,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let prompt: Value = serde_json::from_str(&line).unwrap_or_default();
    let tool = prompt["tool_name"].as_str().unwrap_or("a tool").to_owned();
    let input = prompt["input"].clone();

    let (answer, decision) = mpsc::channel();
    pending
        .lock()
        .map_err(|_| io::Error::other("approval lock poisoned"))?
        .insert(request_id.to_owned(), answer);
    let (label, rule) = rule(&tool, &input);
    let _ = events.send(RunnerEvent::PermissionRequest {
        request_id: request_id.to_owned(),
        detail: summarize(&tool, &input),
        tool,
        rule: label,
    });
    // Waits for the person; a stopped run (cancelled or exited) denies.
    let answer = loop {
        match decision.recv_timeout(Duration::from_millis(100)) {
            Ok(answer) => break answer,
            Err(mpsc::RecvTimeoutError::Timeout) if !stop.load(Ordering::Acquire) => {}
            Err(_) => break PermissionDecision::Deny,
        }
    };
    let reply = reply(answer, input, rule);
    let mut stream = stream;
    writeln!(stream, "{reply}")
}

/// Claude's permission-result JSON. "Always" and "auto" use its own `updatedPermissions`
/// protocol, so Claude remembers them for the rest of the session.
fn reply(decision: PermissionDecision, input: Value, rule: Value) -> Value {
    let allow = |updates: Value| json!({ "behavior": "allow", "updatedInput": input, "updatedPermissions": updates });
    match decision {
        PermissionDecision::Deny => {
            json!({ "behavior": "deny", "message": "The user denied this action in Toma." })
        }
        PermissionDecision::Allow => json!({ "behavior": "allow", "updatedInput": input }),
        PermissionDecision::AlwaysAllow => allow(json!([{
            "type": "addRules",
            "rules": [rule],
            "behavior": "allow",
            "destination": "session",
        }])),
        PermissionDecision::SwitchToAuto => allow(json!([{
            "type": "setMode",
            "mode": "auto",
            "destination": "session",
        }])),
    }
}

/// The permission rule "Always allow" adds: a command prefix for Bash (`git push:*`),
/// otherwise the whole tool. Returns a label for people and the rule for Claude.
pub(crate) fn rule(tool: &str, input: &Value) -> (String, Value) {
    let Some(command) = input["command"].as_str().filter(|_| tool == "Bash") else {
        return (tool.to_owned(), json!({ "toolName": tool }));
    };
    let words: Vec<&str> = command.split_whitespace().collect();
    let prefix = match words.as_slice() {
        [program, sub, ..]
            if sub.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !sub.starts_with('-') =>
        {
            format!("{program} {sub}")
        }
        [program, ..] => (*program).to_owned(),
        [] => String::new(),
    };
    let rule = json!({ "toolName": "Bash", "ruleContent": format!("{prefix}:*") });
    (prefix, rule)
}

/// A one-line description of what the tool is about to do.
pub(crate) fn summarize(tool: &str, input: &Value) -> String {
    let field = ["command", "file_path", "url", "path", "pattern"]
        .iter()
        .find_map(|key| input[key].as_str());
    let text = match field {
        Some(text) => text.to_owned(),
        None if input.is_null() => tool.to_owned(),
        None => input.to_string(),
    };
    let mut chars = text.chars();
    let short: String = chars.by_ref().take(400).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

/// Runs the MCP stdio server that Claude launches, forwarding prompts to `socket`.
pub fn run_approval_bridge(socket: &Path) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(message) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        let reply = handle_mcp(&message, &mut |arguments| ask(socket, arguments));
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn ask(socket: &Path, arguments: &Value) -> Value {
    let answer = (|| -> io::Result<Value> {
        let mut stream = UnixStream::connect(socket)?;
        writeln!(stream, "{arguments}")?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        serde_json::from_str(&line).map_err(io::Error::other)
    })();
    answer.unwrap_or_else(|error| {
        json!({ "behavior": "deny", "message": format!("Toma could not ask the user: {error}") })
    })
}

/// Handles one JSON-RPC message; notifications get no reply.
fn handle_mcp(message: &Value, ask: &mut dyn FnMut(&Value) -> Value) -> Option<Value> {
    let id = message.get("id")?.clone();
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": message["params"]["protocolVersion"],
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "toma", "version": env!("CARGO_PKG_VERSION") },
        }),
        "tools/list" => json!({ "tools": [{
            "name": TOOL,
            "description": "Asks the person using Toma to allow or deny a tool call.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tool_name": { "type": "string" },
                    "input": { "type": "object" },
                    "tool_use_id": { "type": "string" },
                },
                "required": ["tool_name", "input"],
            },
        }]}),
        "tools/call" => {
            let decision = ask(&message["params"]["arguments"]);
            json!({ "content": [{ "type": "text", "text": decision.to_string() }] })
        }
        "ping" => json!({}),
        method => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("unknown method {method}") },
            }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_speaks_mcp_and_forwards_calls() {
        let mut asked = Vec::new();
        let mut ask = |arguments: &Value| {
            asked.push(arguments.clone());
            json!({ "behavior": "deny", "message": "no" })
        };
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}});
        let reply = handle_mcp(&init, &mut ask).unwrap();
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
        assert!(handle_mcp(&json!({"method":"notifications/initialized"}), &mut ask).is_none());

        let list = handle_mcp(&json!({"id":2,"method":"tools/list"}), &mut ask).unwrap();
        assert_eq!(list["result"]["tools"][0]["name"], TOOL);

        let call = json!({"id":3,"method":"tools/call","params":{"name":TOOL,"arguments":{"tool_name":"Bash","input":{"command":"ls"}}}});
        let reply = handle_mcp(&call, &mut ask).unwrap();
        let text: Value =
            serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["behavior"], "deny");
        assert_eq!(asked[0]["input"]["command"], "ls");
    }

    #[test]
    fn listener_turns_socket_prompts_into_events_and_answers() {
        let pending: Pending = Arc::default();
        let (events, received) = mpsc::channel();
        let listener = ApprovalListener::start(RunId::new(), Arc::clone(&pending), events).unwrap();
        let socket = listener.socket.clone();
        let asker = thread::spawn(move || {
            ask(
                &socket,
                &json!({"tool_name":"Bash","input":{"command":"git push"}}),
            )
        });
        let RunnerEvent::PermissionRequest {
            request_id,
            tool,
            detail,
            rule,
        } = received.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("expected a permission request");
        };
        assert_eq!((tool.as_str(), detail.as_str()), ("Bash", "git push"));
        assert_eq!(rule, "git push");
        pending.lock().unwrap()[&request_id]
            .send(PermissionDecision::AlwaysAllow)
            .unwrap();
        let answer = asker.join().unwrap();
        assert_eq!(answer["behavior"], "allow");
        assert_eq!(answer["updatedInput"]["command"], "git push");
        assert_eq!(answer["updatedPermissions"][0]["type"], "addRules");
        assert_eq!(
            answer["updatedPermissions"][0]["rules"][0]["ruleContent"],
            "git push:*"
        );
    }

    #[test]
    fn rules_cover_a_command_prefix_or_a_whole_tool() {
        let bash = |command: &str| rule("Bash", &json!({ "command": command })).0;
        assert_eq!(bash("mkdir -p build/out"), "mkdir");
        assert_eq!(bash("git push origin main"), "git push");
        assert_eq!(bash("npm run test"), "npm run");
        assert_eq!(rule("Write", &json!({ "file_path": "/a" })).0, "Write");
        let auto = reply(PermissionDecision::SwitchToAuto, json!({}), json!({}));
        assert_eq!(auto["updatedPermissions"][0]["mode"], "auto");
    }
}
