//! In-memory workspace with threads full of links, for trying the in-app browser.
//! `cargo run -p toma-ui --example browser_demo`

use toma_domain::*;
use toma_storage::WorkspaceSnapshot;

fn main() {
    let workspace_id = WorkspaceId::new();
    let person_id = PersonId::new();
    let general = ChannelId::new();
    let research = ChannelId::new();
    let claude = AgentId::new();
    let codex = AgentId::new();

    let mut messages = Vec::new();
    let mut threads = Vec::new();
    let mut sessions = Vec::new();
    let mut time = 0;
    let mut tick = || {
        time += 60_000;
        time
    };

    let mut thread = |channel_id, title: &str, ask: &str, agent, replies: &[&str]| {
        let root = MessageId::new();
        let id = ThreadId::new();
        let created = tick();
        messages.push(Message {
            id: root,
            channel_id,
            thread_id: None,
            author: MessageAuthor::Person(person_id),
            body: ask.into(),
            run_id: None,
            created_at: created,
        });
        for reply in replies {
            messages.push(Message {
                id: MessageId::new(),
                channel_id,
                thread_id: Some(id),
                author: MessageAuthor::Agent(agent),
                body: (*reply).into(),
                run_id: None,
                created_at: tick(),
            });
        }
        threads.push(TaskThread {
            id,
            channel_id,
            root_message_id: root,
            title: title.into(),
            status: WorkStatus::Completed,
            created_at: created,
            updated_at: created,
            permission_mode: PermissionMode::Ask,
        });
        sessions.push(AgentSession {
            id: SessionId::new(),
            thread_id: id,
            agent_id: agent,
            provider_session_id: None,
            status: WorkStatus::Completed,
            created_at: created,
        });
    };

    thread(
        general,
        "Embed a webview in GPUI",
        "@Claude how do other GPUI apps embed a webview? See https://github.com/longbridge/gpui-component",
        claude,
        &[
            "The cleanest reference is [gpui-component's webview crate](https://github.com/longbridge/gpui-component/tree/main/crates/webview), built on [wry](https://github.com/tauri-apps/wry).",
            "Zed itself has no webview; see the discussion in [zed-industries/zed](https://github.com/zed-industries/zed/discussions). Cmd-click any link to open it in your system browser instead.",
        ],
    );
    thread(
        general,
        "Review the Codex browser issues",
        "@Codex summarize the open browser-pane issues",
        codex,
        &[
            "Main ones:\n\n- [Panel width is global](https://github.com/openai/codex/issues/20236)\n- [Tabs follow the wrong task](https://github.com/openai/codex/issues/48514)\n- [No Cmd+Shift+Click to open externally](https://github.com/openai/codex/issues/44869)",
            "Also worth a look: [the wry WebViewBuilder docs](https://docs.rs/wry/latest/wry/struct.WebViewBuilder.html). A `mailto:` link like [email](mailto:someone@example.com) goes to the system.",
        ],
    );
    thread(
        research,
        "Rust release notes",
        "@Claude what's new in Rust lately? https://github.com/rust-lang/rust/releases",
        claude,
        &[
            "See the [Rust blog](https://blog.rust-lang.org/) and [RELEASES.md](https://github.com/rust-lang/rust/blob/master/RELEASES.md).",
        ],
    );
    messages.push(Message {
        id: MessageId::new(),
        channel_id: general,
        thread_id: None,
        author: MessageAuthor::Person(person_id),
        body: "A link outside any thread opens in the system browser: https://github.com".into(),
        run_id: None,
        created_at: tick(),
    });

    toma_ui::run_with_snapshot(WorkspaceSnapshot {
        workspace: Some(Workspace {
            id: workspace_id,
            name: "Browser demo".into(),
            repository_path: std::env::current_dir().unwrap(),
            created_at: 0,
        }),
        channels: vec![
            Channel {
                id: general,
                workspace_id,
                name: "general".into(),
                position: 0,
                repository_path: None,
            },
            Channel {
                id: research,
                workspace_id,
                name: "research".into(),
                position: 1,
                repository_path: None,
            },
        ],
        people: vec![Person {
            id: person_id,
            workspace_id,
            display_name: "Ahmed".into(),
        }],
        agents: vec![
            AgentDefinition {
                id: claude,
                workspace_id,
                name: "Claude".into(),
                role: "Implementation".into(),
                instructions: String::new(),
                provider: RunnerProvider::ClaudeCodeCli,
                enabled: true,
            },
            AgentDefinition {
                id: codex,
                workspace_id,
                name: "Codex".into(),
                role: "Review".into(),
                instructions: String::new(),
                provider: RunnerProvider::CodexCli,
                enabled: true,
            },
        ],
        messages,
        threads,
        sessions,
        ..Default::default()
    });
}
