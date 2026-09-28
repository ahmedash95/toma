//! Talks to the real `claude` CLI through the same stack the app wires up.
//! Run with `cargo test -p toma-core --test live_claude -- --ignored`.

use std::{process::Command, sync::Arc};

use tempfile::TempDir;
use toma_core::{SystemClock, TomaCore};
use toma_domain::*;
use toma_runner::{ClaudeCodeRunner, CodexCliRunner};
use toma_storage::SqliteStore;
use toma_worktree::GitWorktreeManager;

#[test]
#[ignore = "calls the real Claude CLI"]
fn claude_replies_in_the_thread_and_remembers_follow_ups() {
    let repo = TempDir::new().unwrap();
    for args in [
        &["init", "-q"][..],
        &["commit", "-q", "--allow-empty", "-m", "init"],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success()
        );
    }
    let store = Arc::new(SqliteStore::open(repo.path().join("toma.db")).unwrap());
    let workspace_id = store.seed_demo_workspace(repo.path()).unwrap();
    let core = TomaCore::new(
        store,
        Arc::new(GitWorktreeManager),
        vec![
            Arc::new(ClaudeCodeRunner::new()),
            Arc::new(CodexCliRunner::new()),
        ],
        Arc::new(SystemClock),
    );
    let snapshot = core.recover(workspace_id).unwrap();
    let claude = snapshot
        .agents
        .iter()
        .find(|a| a.name == "Claude")
        .unwrap()
        .id;
    let channel_id = snapshot.channels[0].id;

    core.dispatch(AppCommand::PostMessage {
        channel_id,
        thread_id: None,
        body: "@Claude remember the word PELICAN. Reply with just OK.".into(),
        attachments: vec![AttachmentTarget::Agent { agent_id: claude }],
    })
    .unwrap();
    let snapshot = core.snapshot(workspace_id).unwrap();
    let thread_id = snapshot.threads[0].id;
    let replies = |snapshot: &toma_storage::WorkspaceSnapshot| -> Vec<String> {
        snapshot
            .messages
            .iter()
            .filter(|m| m.author == MessageAuthor::Agent(claude) && m.thread_id == Some(thread_id))
            .map(|m| m.body.clone())
            .collect()
    };
    assert_eq!(replies(&snapshot).len(), 1, "runs: {:?}", snapshot.runs);
    assert!(snapshot.sessions[0].provider_session_id.is_some());

    core.dispatch(AppCommand::PostMessage {
        channel_id,
        thread_id: Some(thread_id),
        body: "Which word did I ask you to remember? Reply with just the word.".into(),
        attachments: Vec::new(),
    })
    .unwrap();
    let replies = replies(&core.snapshot(workspace_id).unwrap());
    println!("replies: {replies:?}");
    assert_eq!(replies.len(), 2);
    assert!(replies[1].to_uppercase().contains("PELICAN"));
}

#[test]
#[ignore = "calls the real Claude CLI"]
fn claude_works_inside_a_plain_folder_channel() {
    let state = TempDir::new().unwrap();
    let folder = TempDir::new().unwrap();
    std::fs::write(folder.path().join("breakfast-pancakes.txt"), "eggs, flour").unwrap();
    let store = Arc::new(SqliteStore::open(state.path().join("toma.db")).unwrap());
    let workspace_id = store.seed_demo_workspace(state.path()).unwrap();
    let core = TomaCore::new(
        store,
        Arc::new(GitWorktreeManager),
        vec![Arc::new(ClaudeCodeRunner::new())],
        Arc::new(SystemClock),
    );
    let snapshot = core.recover(workspace_id).unwrap();
    let claude = snapshot
        .agents
        .iter()
        .find(|a| a.name == "Claude")
        .unwrap()
        .id;
    let events = core
        .dispatch(AppCommand::CreateChannel {
            workspace_id,
            repository_path: folder.path().to_owned(),
        })
        .unwrap();
    let Some(AppEvent::ChannelCreated { channel_id }) = events.first().cloned() else {
        panic!("no channel: {events:?}");
    };

    core.dispatch(AppCommand::PostMessage {
        channel_id,
        thread_id: None,
        body: "@Claude list the file names in the current directory, nothing else.".into(),
        attachments: vec![AttachmentTarget::Agent { agent_id: claude }],
    })
    .unwrap();
    let snapshot = core.cached_snapshot(workspace_id).unwrap();
    let reply = snapshot
        .messages
        .iter()
        .find(|m| m.author == MessageAuthor::Agent(claude))
        .map(|m| m.body.clone())
        .unwrap_or_default();
    println!("reply: {reply:?}");
    assert!(reply.contains("breakfast-pancakes.txt"));
    assert!(snapshot.worktrees.is_empty(), "plain folders run in place");

    // Edits used to be denied because the runner started Claude in dontAsk mode.
    core.dispatch(AppCommand::PostMessage {
        channel_id,
        thread_id: Some(snapshot.threads[0].id),
        body: "Create notes.txt here containing the word ok.".into(),
        attachments: Vec::new(),
    })
    .unwrap();
    let notes = std::fs::read_to_string(folder.path().join("notes.txt")).unwrap_or_default();
    assert!(notes.contains("ok"), "notes.txt: {notes:?}");
}
