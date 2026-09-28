mod composer;
mod markdown;
mod shell;
mod theme;
mod view_model;

pub use composer::{ComposerModel, MentionCandidate};
pub use markdown::render_markdown;
pub use theme::Theme;
pub use view_model::{ContextKey, ShellViewModel};

use std::sync::Arc;

use gpui::{App, Application, Bounds, WindowBounds, WindowOptions, px, size};
use toma_core::TomaCore;
use toma_domain::*;
use toma_storage::WorkspaceSnapshot;

/// Opens Toma with an already-loaded snapshot. This boundary deliberately has no
/// storage, runner, or worktree responsibilities.
pub fn run_with_core(core: Arc<TomaCore>, workspace_id: WorkspaceId, snapshot: WorkspaceSnapshot) {
    open(snapshot, Some((core, workspace_id)));
}

pub fn run_with_snapshot(snapshot: WorkspaceSnapshot) {
    open(snapshot, None);
}

fn open(snapshot: WorkspaceSnapshot, backend: Option<(Arc<TomaCore>, WorkspaceId)>) {
    Application::new().run(move |cx: &mut App| {
        cx.set_global(Theme::for_appearance(cx.window_appearance()));
        let bounds = Bounds::centered(None, size(px(1180.), px(760.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(860.), px(560.))),
                ..Default::default()
            },
            |window, cx| shell::TomaShell::new(snapshot, backend, window, cx),
        )
        .expect("failed to open the Toma window");
        cx.activate(true);
    });
}

/// Runs a self-contained sample shell for visual development.
pub fn run() {
    run_with_snapshot(sample_snapshot());
}

fn sample_snapshot() -> WorkspaceSnapshot {
    let workspace_id = WorkspaceId::new();
    let person_id = PersonId::new();
    let general_id = ChannelId::new();
    let shipping_id = ChannelId::new();
    let thread_id = ThreadId::new();
    let root_id = MessageId::new();
    let builder_id = AgentId::new();
    let reviewer_id = AgentId::new();
    let researcher_id = AgentId::new();
    let builder_session = SessionId::new();
    let reviewer_session = SessionId::new();

    WorkspaceSnapshot {
        workspace: Some(Workspace {
            id: workspace_id,
            name: "Toma".into(),
            repository_path: "/Users/ahmed/Code/side-projects/toma".into(),
            created_at: 0,
        }),
        channels: vec![
            Channel { id: general_id, workspace_id, name: "general".into(), position: 0 },
            Channel { id: shipping_id, workspace_id, name: "shipping".into(), position: 1 },
            Channel { id: ChannelId::new(), workspace_id, name: "research".into(), position: 2 },
            Channel { id: ChannelId::new(), workspace_id, name: "design-review".into(), position: 3 },
        ],
        people: vec![Person { id: person_id, workspace_id, display_name: "Ahmed".into() }],
        agents: vec![
            AgentDefinition { id: builder_id, workspace_id, name: "Builder".into(), role: "Implementation".into(), instructions: String::new(), provider: RunnerProvider::CodexCli, enabled: true },
            AgentDefinition { id: reviewer_id, workspace_id, name: "Reviewer".into(), role: "Code review".into(), instructions: String::new(), provider: RunnerProvider::CodexCli, enabled: true },
            AgentDefinition { id: researcher_id, workspace_id, name: "Researcher".into(), role: "Investigation".into(), instructions: String::new(), provider: RunnerProvider::ClaudeCodeCli, enabled: true },
        ],
        messages: vec![
            Message { id: root_id, channel_id: general_id, thread_id: None, author: MessageAuthor::Person(person_id), body: "Let's make the first native shell feel focused and useful. Keep the conversation dense, and make agent state readable at a glance.".into(), created_at: 1 },
            Message { id: MessageId::new(), channel_id: general_id, thread_id: None, author: MessageAuthor::Agent(builder_id), body: "I've mapped the domain snapshot into the shell. The composer keeps a separate draft for every channel and thread.".into(), created_at: 2 },
            Message { id: MessageId::new(), channel_id: general_id, thread_id: None, author: MessageAuthor::Agent(reviewer_id), body: "Reviewing keyboard behavior and empty-state edges now. One thread needs your input before it can continue.".into(), created_at: 3 },
            Message { id: MessageId::new(), channel_id: general_id, thread_id: Some(thread_id), author: MessageAuthor::Agent(builder_id), body: "The main pane and thread context stay independent, including their drafts.".into(), created_at: 4 },
            Message { id: MessageId::new(), channel_id: shipping_id, thread_id: None, author: MessageAuthor::Person(person_id), body: "Prepare the release notes once the shell is ready.".into(), created_at: 5 },
        ],
        threads: vec![TaskThread { id: thread_id, channel_id: general_id, root_message_id: root_id, title: "Native shell polish".into(), status: WorkStatus::Working, created_at: 1, updated_at: 4 }],
        sessions: vec![
            AgentSession { id: builder_session, thread_id, agent_id: builder_id, provider_session_id: None, status: WorkStatus::Working, created_at: 1 },
            AgentSession { id: reviewer_session, thread_id, agent_id: reviewer_id, provider_session_id: None, status: WorkStatus::WaitingForInput, created_at: 2 },
        ],
        runs: vec![
            Run { id: RunId::new(), session_id: builder_session, sequence: 1, status: WorkStatus::Working, started_at: Some(1), finished_at: None },
            Run { id: RunId::new(), session_id: reviewer_session, sequence: 1, status: WorkStatus::WaitingForInput, started_at: Some(2), finished_at: None },
        ],
        drafts: vec![Draft { channel_id: shipping_id, thread_id: None, body: "Release notes: ".into(), updated_at: 6 }],
        ..Default::default()
    }
}
