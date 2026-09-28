#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use toma_domain::{PermissionMode, RunId, RunnerProvider, SessionId};
use toma_runner::{AgentRunner, CursorCliRunner, RunRequest, RunnerEvent};

const SAMPLE: &str = r#"{"type":"system","subtype":"init","apiKeySource":"login","cwd":"/tmp","session_id":"4a7a90fc","model":"Composer 2.5 Fast","permissionMode":"default"}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"go"}]},"session_id":"4a7a90fc"}
{"type":"thinking","subtype":"delta","text":"The user wants","session_id":"4a7a90fc","timestamp_ms":1}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Look"}]},"session_id":"4a7a90fc","timestamp_ms":2}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ing."}]},"session_id":"4a7a90fc","timestamp_ms":3}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking."}]},"session_id":"4a7a90fc","model_call_id":"m1","timestamp_ms":4}
{"type":"tool_call","subtype":"started","call_id":"t1","tool_call":{}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]},"session_id":"4a7a90fc","timestamp_ms":5}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]},"session_id":"4a7a90fc"}
{"type":"result","subtype":"success","duration_ms":2807,"is_error":false,"result":"Looking.\nhi","session_id":"4a7a90fc","usage":{"inputTokens":8955,"outputTokens":59,"cacheReadTokens":3872,"cacheWriteTokens":0}}
"#;

fn fake(body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("toma-cursor-test-{}", RunId::new()));
    fs::create_dir(&dir).unwrap();
    let path = dir.join("cursor-agent");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, path)
}

fn run(
    runner: &CursorCliRunner,
    dir: &std::path::Path,
    resume: Option<&str>,
    prompt: &str,
) -> Vec<RunnerEvent> {
    let mut events = Vec::new();
    runner
        .run(
            RunRequest {
                run_id: RunId::new(),
                session_id: SessionId::new(),
                provider_session_id: resume,
                working_directory: dir,
                prompt,
                permission_mode: PermissionMode::Ask,
            },
            &mut |event| events.push(event),
        )
        .unwrap();
    events
}

#[test]
fn cursor_maps_stream_json_to_session_deltas_reply_and_usage() {
    let (dir, exe) = fake(&format!("cat <<'EOF'\n{SAMPLE}EOF"));
    let runner = CursorCliRunner::with_executable(exe);
    let events = run(&runner, &dir, None, "go");
    let derived: Vec<_> = events
        .into_iter()
        .filter(|event| !matches!(event, RunnerEvent::Output(_)))
        .collect();

    assert_eq!(runner.provider(), RunnerProvider::CursorCli);
    assert_eq!(
        derived,
        vec![
            RunnerEvent::Started {
                provider_session_id: None
            },
            RunnerEvent::Started {
                provider_session_id: Some("4a7a90fc".into())
            },
            RunnerEvent::ReplyDelta("Look".into()),
            RunnerEvent::ReplyDelta("ing.".into()),
            RunnerEvent::ReplyDelta("\n\n".into()),
            RunnerEvent::ReplyDelta("hi".into()),
            RunnerEvent::Reply("Looking.\nhi".into()),
            RunnerEvent::Usage {
                input_tokens: 8955 + 3872,
                output_tokens: 59,
                cached_tokens: 3872,
                cost_micros: None,
                duration_ms: Some(2807),
            },
            RunnerEvent::Completed,
        ]
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn cursor_command_is_headless_sandboxed_and_resumes() {
    let (dir, exe) = fake("printf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"");
    let runner = CursorCliRunner::with_executable(exe);
    run(&runner, &dir, Some("sess-1"), "do it");
    let args = fs::read_to_string(dir.join("args")).unwrap();
    for expected in [
        "-p\n",
        "--trust\n",
        "--sandbox\nenabled\n",
        "--output-format\nstream-json\n",
        "--stream-partial-output\n",
        "--resume\nsess-1\ndo it\n",
    ] {
        assert!(args.contains(expected), "missing {expected:?} in {args:?}");
    }
    assert!(!args.contains("--force") && !args.contains("--yolo"));

    run(&runner, &dir, None, "fresh");
    assert!(
        !fs::read_to_string(dir.join("args"))
            .unwrap()
            .contains("--resume")
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
#[ignore = "runs the real cursor-agent"]
fn live_cursor_agent_replies() {
    let dir = std::env::temp_dir().join(format!("toma-cursor-live-{}", RunId::new()));
    fs::create_dir(&dir).unwrap();
    let events = run(
        &CursorCliRunner::new(),
        &dir,
        None,
        "reply with the single word hi",
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RunnerEvent::Reply(text) if !text.trim().is_empty())),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RunnerEvent::Usage { .. }))
    );
    let _ = fs::remove_dir_all(dir);
}
