//! Claude asks the person through Toma's own approval bridge, end to end.
//! Run with `cargo test -p toma --test live_approval -- --ignored --nocapture`.

use std::{sync::Arc, thread, time::Duration};

use toma_core::{SystemClock, TomaCore};
use toma_domain::*;
use toma_runner::ClaudeCodeRunner;
use toma_storage::SqliteStore;
use toma_worktree::GitWorktreeManager;

fn prompt() -> String {
    std::env::var("TOMA_APPROVAL_PROMPT").unwrap_or_else(|_| {
        "Use the Bash tool to run exactly this command and nothing else: \
         touch /private/tmp/toma-approval-probe"
            .into()
    })
}

#[test]
#[ignore = "calls the real Claude CLI"]
fn claude_waits_for_the_person_through_the_bridge() {
    let state = tempfile::TempDir::new().unwrap();
    let folder = tempfile::TempDir::new().unwrap();
    let store = Arc::new(SqliteStore::open(state.path().join("toma.db")).unwrap());
    let workspace_id = store.seed_demo_workspace(state.path()).unwrap();
    let core = Arc::new(TomaCore::new(
        store,
        Arc::new(GitWorktreeManager),
        vec![Arc::new(
            ClaudeCodeRunner::new().with_approvals(env!("CARGO_BIN_EXE_toma")),
        )],
        Arc::new(SystemClock),
    ));
    let snapshot = core.recover(workspace_id).unwrap();
    let claude = snapshot
        .agents
        .iter()
        .find(|a| a.name == "Claude")
        .unwrap()
        .id;
    let Some(AppEvent::ChannelCreated { channel_id }) = core
        .dispatch(AppCommand::CreateChannel {
            workspace_id,
            repository_path: folder.path().to_owned(),
        })
        .unwrap()
        .first()
        .cloned()
    else {
        panic!("no channel");
    };

    // Plays the person: denies whatever Claude asks for.
    let person = thread::spawn({
        let core = Arc::clone(&core);
        move || {
            for _ in 0..1200 {
                let live = core.cached_snapshot(workspace_id).unwrap();
                if let Some(request) = live.permission_requests.first().cloned() {
                    assert_eq!(live.runs[0].status, WorkStatus::WaitingForInput);
                    core.dispatch(AppCommand::AnswerPermission {
                        run_id: request.run_id,
                        request_id: request.id.clone(),
                        allow: false,
                    })
                    .unwrap();
                    return Some(request);
                }
                thread::sleep(Duration::from_millis(100));
            }
            None
        }
    });

    core.dispatch(AppCommand::PostMessage {
        channel_id,
        thread_id: None,
        body: prompt(),
        attachments: vec![AttachmentTarget::Agent { agent_id: claude }],
    })
    .unwrap();
    let live = core.cached_snapshot(workspace_id).unwrap();
    for message in &live.messages {
        println!("{:?}: {}", message.author, message.body);
    }
    let request = person
        .join()
        .unwrap()
        .expect("Claude never asked for permission");
    println!("asked: {} {}", request.tool, request.detail);
    assert!(
        live.messages
            .iter()
            .any(|m| m.body.starts_with("Denied **"))
    );
    assert_eq!(live.runs[0].status, WorkStatus::Completed);
    assert!(
        !std::path::Path::new("/private/tmp/toma-approval-probe").exists(),
        "denied command must not run"
    );
}
