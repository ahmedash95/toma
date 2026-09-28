#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use toma_domain::{RunId, RunnerProvider, SessionId};
use toma_runner::{
    AgentRunner, ClaudeCodeRunner, CodexCliRunner, RunRequest, RunnerError, RunnerEvent,
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "toma-runner-test-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fake_executable(directory: &TestDirectory, body: &str) -> PathBuf {
    let path = directory.path().join("fake-provider");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn request(run_id: RunId, directory: &Path) -> RunRequest<'_> {
    RunRequest {
        run_id,
        session_id: SessionId::new(),
        provider_session_id: None,
        working_directory: directory,
        prompt: "do the work",
    }
}

#[test]
fn availability_runs_the_executable_instead_of_trusting_its_presence() {
    let directory = TestDirectory::new();
    let healthy = fake_executable(&directory, "[ \"$1\" = \"--version\" ]");
    assert!(
        ClaudeCodeRunner::with_executable(healthy)
            .availability()
            .is_ok()
    );

    let broken_directory = TestDirectory::new();
    let broken = fake_executable(
        &broken_directory,
        "echo 'native executable is missing' >&2\nexit 127",
    );
    let error = CodexCliRunner::with_executable(broken)
        .availability()
        .unwrap_err();
    assert!(matches!(error, RunnerError::Unavailable(_)));
    assert!(error.to_string().contains("native executable is missing"));
}

#[test]
fn claude_streams_lines_and_uses_noninteractive_resume_arguments() {
    let directory = TestDirectory::new();
    let arguments = directory.path().join("arguments");
    let executable = fake_executable(
        &directory,
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\nprintf 'first\\nsecond\\n'",
            arguments.display()
        ),
    );
    let runner = ClaudeCodeRunner::with_executable(executable);
    let run_id = RunId::new();
    let mut request = request(run_id, directory.path());
    request.provider_session_id = Some("session-123");
    let mut events = Vec::new();

    runner
        .run(request, &mut |event| events.push(event))
        .unwrap();

    assert_eq!(runner.provider(), RunnerProvider::ClaudeCodeCli);
    assert_eq!(
        events,
        vec![
            RunnerEvent::Started {
                provider_session_id: Some("session-123".into())
            },
            RunnerEvent::Output("first".into()),
            RunnerEvent::Output("second".into()),
            RunnerEvent::Completed,
        ]
    );
    let arguments = fs::read_to_string(arguments).unwrap();
    assert!(arguments.contains("--print\n"));
    assert!(arguments.contains("--output-format\nstream-json\n"));
    assert!(arguments.contains("--permission-prompts\nnone\n"));
    assert!(arguments.contains("--resume\nsession-123\n"));
}

#[test]
fn codex_keeps_its_command_shape_provider_specific() {
    let directory = TestDirectory::new();
    let arguments = directory.path().join("arguments");
    let executable = fake_executable(
        &directory,
        &format!("printf '%s\\n' \"$@\" > '{}'", arguments.display()),
    );
    let runner = CodexCliRunner::with_executable(executable);

    runner
        .run(request(RunId::new(), directory.path()), &mut |_| {})
        .unwrap();

    assert_eq!(runner.provider(), RunnerProvider::CodexCli);
    assert_eq!(
        fs::read_to_string(arguments).unwrap(),
        "exec\n--json\ndo the work\n"
    );
}

#[test]
fn nonzero_exit_streams_stderr_and_returns_the_failure() {
    let directory = TestDirectory::new();
    let executable = fake_executable(
        &directory,
        "echo 'provider detail' >&2\necho 'partial output'\nexit 23",
    );
    let runner = ClaudeCodeRunner::with_executable(executable);
    let mut events = Vec::new();

    let error = runner
        .run(request(RunId::new(), directory.path()), &mut |event| {
            events.push(event)
        })
        .unwrap_err();

    assert!(error.to_string().contains("status 23"));
    assert!(error.to_string().contains("provider detail"));
    assert!(events.contains(&RunnerEvent::Output("partial output".into())));
    assert!(events.contains(&RunnerEvent::Output("provider detail".into())));
    assert!(
        matches!(events.last(), Some(RunnerEvent::Failed(message)) if message.contains("provider detail"))
    );
}

#[test]
fn cancellation_targets_the_requested_active_run_and_cleans_it_up() {
    let directory = TestDirectory::new();
    let executable = fake_executable(&directory, "echo ready\nwhile true; do sleep 1; done");
    let runner = Arc::new(ClaudeCodeRunner::with_executable(executable));
    let run_id = RunId::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let run_runner = Arc::clone(&runner);
    let run_events = Arc::clone(&events);
    let working_directory = directory.path().to_path_buf();
    let handle = thread::spawn(move || {
        run_runner.run(request(run_id, &working_directory), &mut |event| {
            run_events.lock().unwrap().push(event)
        })
    });

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if events
            .lock()
            .unwrap()
            .contains(&RunnerEvent::Output("ready".into()))
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    runner.cancel(run_id).unwrap();
    assert!(handle.join().unwrap().is_ok());
    assert!(matches!(
        events.lock().unwrap().last(),
        Some(RunnerEvent::Cancelled)
    ));
    assert!(runner.cancel(run_id).is_err());
}
