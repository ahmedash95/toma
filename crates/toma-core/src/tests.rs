use super::*;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
};
use toma_runner::RunnerCapabilities;
use toma_storage::StorageResult;
use toma_worktree::{CleanupDecision, GitWorktreeManager, WriteLease};

#[derive(Default)]
struct FakeStore {
    snapshot: Mutex<WorkspaceSnapshot>,
    history: Mutex<Vec<(RunId, i64, String)>>,
    statuses: Mutex<Vec<(RunId, WorkStatus)>>,
    drafts: Mutex<Vec<Draft>>,
}

impl TomaStore for FakeStore {
    fn snapshot(&self, _workspace_id: WorkspaceId) -> StorageResult<WorkspaceSnapshot> {
        Ok(self.snapshot.lock().unwrap().clone())
    }

    fn save_draft(&self, draft: &Draft) -> StorageResult<()> {
        self.drafts.lock().unwrap().push(draft.clone());
        Ok(())
    }

    fn insert_message(&self, message: &Message, attachments: &[Attachment]) -> StorageResult<()> {
        let mut snapshot = self.snapshot.lock().unwrap();
        snapshot.messages.push(message.clone());
        snapshot.attachments.extend_from_slice(attachments);
        Ok(())
    }

    fn insert_thread_bundle(
        &self,
        thread: &TaskThread,
        session: &AgentSession,
        run: &Run,
    ) -> StorageResult<()> {
        let mut snapshot = self.snapshot.lock().unwrap();
        snapshot.threads.push(thread.clone());
        snapshot.sessions.push(session.clone());
        snapshot.runs.push(run.clone());
        Ok(())
    }

    fn insert_session_bundle(&self, session: &AgentSession, run: &Run) -> StorageResult<()> {
        let mut snapshot = self.snapshot.lock().unwrap();
        snapshot.sessions.push(session.clone());
        snapshot.runs.push(run.clone());
        Ok(())
    }

    fn insert_channel(&self, channel: &Channel) -> StorageResult<()> {
        self.snapshot.lock().unwrap().channels.push(channel.clone());
        Ok(())
    }

    fn insert_run(&self, run: &Run) -> StorageResult<()> {
        self.snapshot.lock().unwrap().runs.push(run.clone());
        Ok(())
    }

    fn update_run_status(
        &self,
        run_id: RunId,
        status: WorkStatus,
        at: TimestampMs,
    ) -> StorageResult<()> {
        self.statuses.lock().unwrap().push((run_id, status));
        if let Some(run) = self
            .snapshot
            .lock()
            .unwrap()
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
        {
            run.status = status;
            if status == WorkStatus::Working && run.started_at.is_none() {
                run.started_at = Some(at);
            }
            if status.is_terminal() {
                run.finished_at = Some(at);
            }
        }
        Ok(())
    }

    fn append_raw_history(
        &self,
        run_id: RunId,
        sequence: i64,
        payload: &str,
        _at: TimestampMs,
    ) -> StorageResult<()> {
        self.history
            .lock()
            .unwrap()
            .push((run_id, sequence, payload.into()));
        Ok(())
    }

    fn set_provider_session_id(
        &self,
        session_id: SessionId,
        provider_session_id: &str,
    ) -> StorageResult<()> {
        if let Some(session) = self
            .snapshot
            .lock()
            .unwrap()
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.provider_session_id = Some(provider_session_id.to_owned());
        }
        Ok(())
    }

    fn save_run_usage(&self, usage: &RunUsage) -> StorageResult<()> {
        self.snapshot.lock().unwrap().usages.push(usage.clone());
        Ok(())
    }

    fn save_worktree(&self, worktree: &WorktreeRecord) -> StorageResult<()> {
        self.snapshot
            .lock()
            .unwrap()
            .worktrees
            .push(worktree.clone());
        Ok(())
    }
}

struct FakeRunner {
    emitted: Vec<RunnerEvent>,
    requests: Mutex<Vec<(RunId, SessionId, PathBuf, String)>>,
    cancelled: Mutex<Vec<RunId>>,
    answers: Mutex<Vec<(String, bool)>>,
}

impl AgentRunner for FakeRunner {
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
        Ok(())
    }

    fn run(
        &self,
        request: RunRequest<'_>,
        emit: &mut dyn FnMut(RunnerEvent),
    ) -> Result<(), RunnerError> {
        self.requests.lock().unwrap().push((
            request.run_id,
            request.session_id,
            request.working_directory.into(),
            request.prompt.into(),
        ));
        for event in &self.emitted {
            emit(event.clone());
        }
        Ok(())
    }

    fn cancel(&self, run_id: RunId) -> Result<(), RunnerError> {
        self.cancelled.lock().unwrap().push(run_id);
        Ok(())
    }

    fn answer_permission(
        &self,
        _run_id: RunId,
        request_id: &str,
        allow: bool,
    ) -> Result<(), RunnerError> {
        self.answers
            .lock()
            .unwrap()
            .push((request_id.to_owned(), allow));
        Ok(())
    }
}

#[derive(Default)]
struct FakeWorktrees {
    ensured: Mutex<Vec<ThreadId>>,
}

impl WorktreeManager for FakeWorktrees {
    fn ensure(&self, request: WorktreeRequest<'_>) -> Result<WorktreeRecord, WorktreeError> {
        self.ensured.lock().unwrap().push(request.thread_id);
        Ok(WorktreeRecord {
            thread_id: request.thread_id,
            path: request.worktree_root.join(request.thread_id.to_string()),
            branch: format!("toma/{}", request.thread_id),
            created_at: request.created_at,
        })
    }

    fn acquire_write_lock(
        &self,
        thread_id: ThreadId,
        owner: &str,
    ) -> Result<WriteLease, WorktreeError> {
        GitWorktreeManager.acquire_write_lock(thread_id, owner)
    }

    fn cleanup_decision(
        &self,
        _repository_path: &Path,
        _record: &WorktreeRecord,
    ) -> Result<CleanupDecision, WorktreeError> {
        Ok(CleanupDecision::Safe)
    }
}

struct FakeClock(AtomicI64);

impl Clock for FakeClock {
    fn now_ms(&self) -> TimestampMs {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

struct Fixture {
    core: TomaCore,
    store: Arc<FakeStore>,
    runner: Arc<FakeRunner>,
    worktrees: Arc<FakeWorktrees>,
    workspace_id: WorkspaceId,
    channel_id: ChannelId,
    agent_id: AgentId,
    collaborator_id: AgentId,
}

fn fixture() -> Fixture {
    fixture_with_events(vec![
        RunnerEvent::Started {
            provider_session_id: Some("provider-1".into()),
        },
        RunnerEvent::Output("hello".into()),
        RunnerEvent::Completed,
    ])
}

fn fixture_with_events(emitted: Vec<RunnerEvent>) -> Fixture {
    let workspace_id = WorkspaceId::new();
    let channel_id = ChannelId::new();
    let agent_id = AgentId::new();
    let collaborator_id = AgentId::new();
    let snapshot = WorkspaceSnapshot {
        workspace: Some(Workspace {
            id: workspace_id,
            name: "Toma".into(),
            repository_path: PathBuf::from("/repo"),
            created_at: 1,
        }),
        channels: vec![Channel {
            id: channel_id,
            workspace_id,
            name: "general".into(),
            position: 0,
            repository_path: None,
        }],
        people: vec![Person {
            id: PersonId::new(),
            workspace_id,
            display_name: "User".into(),
        }],
        agents: vec![
            AgentDefinition {
                id: agent_id,
                workspace_id,
                name: "Builder".into(),
                role: "developer".into(),
                instructions: String::new(),
                provider: RunnerProvider::CodexCli,
                enabled: true,
            },
            AgentDefinition {
                id: collaborator_id,
                workspace_id,
                name: "Reviewer".into(),
                role: "reviewer".into(),
                instructions: String::new(),
                provider: RunnerProvider::CodexCli,
                enabled: true,
            },
        ],
        ..WorkspaceSnapshot::default()
    };
    let store = Arc::new(FakeStore {
        snapshot: Mutex::new(snapshot),
        ..FakeStore::default()
    });
    let runner = Arc::new(FakeRunner {
        emitted,
        requests: Mutex::new(Vec::new()),
        cancelled: Mutex::new(Vec::new()),
        answers: Mutex::new(Vec::new()),
    });
    let worktrees = Arc::new(FakeWorktrees::default());
    let core = TomaCore::new(
        store.clone(),
        worktrees.clone(),
        vec![runner.clone()],
        Arc::new(FakeClock(AtomicI64::new(100))),
    );
    core.snapshot(workspace_id).unwrap();
    Fixture {
        core,
        store,
        runner,
        worktrees,
        workspace_id,
        channel_id,
        agent_id,
        collaborator_id,
    }
}

#[test]
fn channel_mention_creates_and_runs_a_durable_thread() {
    let fixture = fixture();
    let events = fixture
        .core
        .dispatch(AppCommand::PostMessage {
            channel_id: fixture.channel_id,
            thread_id: None,
            body: "Build the feature".into(),
            attachments: vec![AttachmentTarget::Agent {
                agent_id: fixture.agent_id,
            }],
        })
        .unwrap();

    let snapshot = fixture.store.snapshot.lock().unwrap();
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.attachments.len(), 1);
    assert_eq!(snapshot.threads.len(), 1);
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.runs.len(), 1);
    assert_eq!(snapshot.worktrees.len(), 1);
    assert_eq!(snapshot.runs[0].status, WorkStatus::Completed);
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::RunnerOutput { text, .. } if text == "hello"
    )));
    assert_eq!(fixture.store.history.lock().unwrap().len(), 3);
    let prompt = fixture.runner.requests.lock().unwrap()[0].3.clone();
    assert!(
        prompt.starts_with("Build the feature\n\n---\nYou are Builder"),
        "{prompt}"
    );
    assert!(prompt.contains("@Reviewer"));
}

#[test]
fn collaborator_gets_a_separate_session_and_run_on_the_same_worktree() {
    let fixture = fixture();
    let first_events = fixture
        .core
        .dispatch(AppCommand::PostMessage {
            channel_id: fixture.channel_id,
            thread_id: None,
            body: "Investigate".into(),
            attachments: vec![AttachmentTarget::Agent {
                agent_id: fixture.agent_id,
            }],
        })
        .unwrap();
    let thread_id = first_events
        .iter()
        .find_map(|event| match event {
            AppEvent::ThreadCreated { thread_id } => Some(*thread_id),
            _ => None,
        })
        .unwrap();

    let events = fixture
        .core
        .dispatch(AppCommand::AttachAgent {
            thread_id,
            agent_id: fixture.collaborator_id,
        })
        .unwrap();

    let snapshot = fixture.store.snapshot.lock().unwrap();
    assert_eq!(snapshot.threads.len(), 1);
    assert_eq!(snapshot.sessions.len(), 2);
    assert_eq!(snapshot.runs.len(), 2);
    assert_eq!(fixture.worktrees.ensured.lock().unwrap().len(), 1);
    assert!(matches!(events[0], AppEvent::SessionAttached { .. }));
    assert!(matches!(events[1], AppEvent::RunCreated { .. }));
}

#[test]
fn plain_thread_reply_continues_the_existing_agent_session() {
    let fixture = fixture();
    let first_events = fixture
        .core
        .dispatch(AppCommand::PostMessage {
            channel_id: fixture.channel_id,
            thread_id: None,
            body: "Investigate".into(),
            attachments: vec![AttachmentTarget::Agent {
                agent_id: fixture.agent_id,
            }],
        })
        .unwrap();
    let thread_id = first_events
        .iter()
        .find_map(|event| match event {
            AppEvent::ThreadCreated { thread_id } => Some(*thread_id),
            _ => None,
        })
        .unwrap();

    let events = fixture
        .core
        .dispatch(AppCommand::PostMessage {
            channel_id: fixture.channel_id,
            thread_id: Some(thread_id),
            body: "Additional context".into(),
            attachments: Vec::new(),
        })
        .unwrap();

    assert!(matches!(events[0], AppEvent::MessagePosted { .. }));
    let requests = fixture.runner.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].1, requests[0].1,
        "same session, so the provider resumes"
    );
    assert!(requests[1].3.starts_with("Additional context"));
    let snapshot = fixture.store.snapshot.lock().unwrap();
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.runs.len(), 2);
    assert_eq!(snapshot.runs[1].sequence, 2);
}

#[test]
fn waiting_output_is_persisted_and_surfaces_status_and_text() {
    let fixture = fixture_with_events(vec![
        RunnerEvent::Started {
            provider_session_id: None,
        },
        RunnerEvent::WaitingForInput("Need approval".into()),
    ]);
    let events = fixture
        .core
        .dispatch(AppCommand::PostMessage {
            channel_id: fixture.channel_id,
            thread_id: None,
            body: "Do work".into(),
            attachments: vec![AttachmentTarget::Agent {
                agent_id: fixture.agent_id,
            }],
        })
        .unwrap();

    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::RunnerOutput { text, .. } if text == "Need approval"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::StatusChanged {
            status: WorkStatus::WaitingForInput,
            ..
        }
    )));
    assert_eq!(
        fixture.store.history.lock().unwrap()[1].2,
        "waiting_for_input:Need approval"
    );
}

#[test]
fn selection_and_drafts_validate_against_hydrated_state() {
    let fixture = fixture();
    assert_eq!(
        fixture
            .core
            .dispatch(AppCommand::SelectChannel {
                channel_id: fixture.channel_id,
            })
            .unwrap(),
        vec![AppEvent::ChannelSelected {
            channel_id: fixture.channel_id,
        }]
    );
    let draft = Draft {
        channel_id: fixture.channel_id,
        thread_id: None,
        body: "unfinished".into(),
        updated_at: 22,
    };
    assert_eq!(
        fixture
            .core
            .dispatch(AppCommand::SaveDraft {
                draft: draft.clone(),
            })
            .unwrap(),
        vec![AppEvent::DraftSaved {
            channel_id: fixture.channel_id,
            thread_id: None,
        }]
    );
    assert_eq!(*fixture.store.drafts.lock().unwrap(), vec![draft]);
}

#[test]
fn cancellation_uses_persisted_run_relationships_after_restart() {
    let fixture = fixture();
    let session = AgentSession {
        id: SessionId::new(),
        thread_id: ThreadId::new(),
        agent_id: fixture.agent_id,
        provider_session_id: Some("resume-me".into()),
        status: WorkStatus::Working,
        created_at: 1,
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        sequence: 1,
        status: WorkStatus::Working,
        started_at: Some(1),
        finished_at: None,
    };
    {
        let mut snapshot = fixture.store.snapshot.lock().unwrap();
        snapshot.threads.push(TaskThread {
            id: session.thread_id,
            channel_id: fixture.channel_id,
            root_message_id: MessageId::new(),
            title: "Recovered".into(),
            status: WorkStatus::Working,
            created_at: 1,
            updated_at: 1,
        });
        snapshot.sessions.push(session);
        snapshot.runs.push(run.clone());
    }

    let restarted = TomaCore::new(
        fixture.store.clone(),
        fixture.worktrees.clone(),
        vec![fixture.runner.clone()],
        Arc::new(FakeClock(AtomicI64::new(500))),
    );
    restarted.snapshot(fixture.workspace_id).unwrap();
    assert_eq!(
        restarted
            .dispatch(AppCommand::CancelRun { run_id: run.id })
            .unwrap(),
        vec![AppEvent::StatusChanged {
            run_id: run.id,
            status: WorkStatus::Cancelled,
        }]
    );
    assert_eq!(*fixture.runner.cancelled.lock().unwrap(), vec![run.id]);
}

#[test]
fn open_workspace_uses_loaded_persistent_identity() {
    let fixture = fixture();
    assert_eq!(
        fixture
            .core
            .dispatch(AppCommand::OpenWorkspace {
                repository_path: PathBuf::from("/repo"),
            })
            .unwrap(),
        vec![AppEvent::WorkspaceOpened {
            workspace_id: fixture.workspace_id,
        }]
    );
}

#[test]
fn recover_fails_runs_orphaned_by_a_previous_process() {
    let fixture = fixture();
    let mut runs = Vec::new();
    for status in [WorkStatus::Working, WorkStatus::WaitingForInput] {
        runs.push(Run {
            id: RunId::new(),
            session_id: SessionId::new(),
            sequence: 1,
            status,
            started_at: Some(1),
            finished_at: None,
        });
    }
    fixture
        .store
        .snapshot
        .lock()
        .unwrap()
        .runs
        .extend(runs.clone());

    let snapshot = fixture.core.recover(fixture.workspace_id).unwrap();
    let status = |id| {
        snapshot
            .runs
            .iter()
            .find(|run| run.id == id)
            .unwrap()
            .status
    };
    assert_eq!(status(runs[0].id), WorkStatus::Failed);
    assert_eq!(status(runs[1].id), WorkStatus::WaitingForInput);
}

#[test]
fn agent_reply_becomes_a_thread_message_and_session_id_persists() {
    let fixture = fixture_with_events(vec![
        RunnerEvent::Started {
            provider_session_id: Some("provider-1".into()),
        },
        RunnerEvent::Reply("done".into()),
        RunnerEvent::Completed,
    ]);
    fixture
        .core
        .dispatch(AppCommand::PostMessage {
            channel_id: fixture.channel_id,
            thread_id: None,
            body: "Build it".into(),
            attachments: vec![AttachmentTarget::Agent {
                agent_id: fixture.agent_id,
            }],
        })
        .unwrap();

    let snapshot = fixture.store.snapshot.lock().unwrap();
    let reply = snapshot.messages.last().unwrap();
    assert_eq!(reply.body, "done");
    assert_eq!(reply.author, MessageAuthor::Agent(fixture.agent_id));
    assert_eq!(reply.thread_id, Some(snapshot.threads[0].id));
    assert_eq!(
        snapshot.sessions[0].provider_session_id.as_deref(),
        Some("provider-1")
    );
}

#[test]
fn streamed_reply_grows_one_live_message_and_persists_on_completion() {
    let fixture = fixture_with_events(vec![
        RunnerEvent::ReplyDelta("\n\n".into()),
        RunnerEvent::ReplyDelta("Hel".into()),
        RunnerEvent::ReplyDelta("lo".into()),
        RunnerEvent::Reply("Hello".into()),
        RunnerEvent::Completed,
    ]);
    fixture
        .core
        .dispatch(AppCommand::PostMessage {
            channel_id: fixture.channel_id,
            thread_id: None,
            body: "Greet".into(),
            attachments: vec![AttachmentTarget::Agent {
                agent_id: fixture.agent_id,
            }],
        })
        .unwrap();

    let live = fixture.core.cached_snapshot(fixture.workspace_id).unwrap();
    let stored = fixture.store.snapshot.lock().unwrap();
    for snapshot in [&live, &*stored] {
        let replies: Vec<_> = snapshot
            .messages
            .iter()
            .filter(|m| m.author == MessageAuthor::Agent(fixture.agent_id))
            .collect();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].body, "Hello");
    }
    assert!(fixture.core.revision() > 0);
    assert_eq!(live.threads[0].status, WorkStatus::Completed);
    assert_eq!(live.sessions[0].status, WorkStatus::Completed);
}

#[test]
fn thread_titles_are_short_and_drop_mentions() {
    assert_eq!(
        thread_title("@Claude what model are you running?\nmore", "Claude"),
        "What model are you running?"
    );
    assert_eq!(
        thread_title(
            "please refactor the storage layer so that migrations run inside one transaction",
            "Claude"
        ),
        "Please refactor the storage layer so that…"
    );
    assert_eq!(thread_title("@Claude", "Claude"), "Task for Claude");
}

#[test]
fn channels_take_their_folder_name_and_plain_folders_skip_worktrees() {
    let fixture = fixture();
    let folder = tempfile::TempDir::new().unwrap();
    let path = folder.path().join("My App");
    std::fs::create_dir(&path).unwrap();
    for _ in 0..2 {
        fixture
            .core
            .dispatch(AppCommand::CreateChannel {
                workspace_id: fixture.workspace_id,
                repository_path: path.clone(),
            })
            .unwrap();
    }
    let snapshot = fixture.core.cached_snapshot(fixture.workspace_id).unwrap();
    let names: Vec<_> = snapshot.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["general", "my-app", "my-app-2"]);

    fixture
        .core
        .dispatch(AppCommand::PostMessage {
            channel_id: snapshot.channels[1].id,
            thread_id: None,
            body: "Look around".into(),
            attachments: vec![AttachmentTarget::Agent {
                agent_id: fixture.agent_id,
            }],
        })
        .unwrap();
    assert_eq!(fixture.runner.requests.lock().unwrap()[0].2, path);
    assert!(fixture.worktrees.ensured.lock().unwrap().is_empty());
}

#[test]
fn permission_requests_wait_for_the_person_and_are_logged() {
    // The fake returns without completing, like a real run blocked on the prompt.
    let fixture = fixture_with_events(vec![RunnerEvent::PermissionRequest {
        request_id: "r1".into(),
        tool: "Bash".into(),
        detail: "git push".into(),
    }]);
    fixture
        .core
        .dispatch(AppCommand::PostMessage {
            channel_id: fixture.channel_id,
            thread_id: None,
            body: "Ship it".into(),
            attachments: vec![AttachmentTarget::Agent {
                agent_id: fixture.agent_id,
            }],
        })
        .unwrap();
    let live = fixture.core.cached_snapshot(fixture.workspace_id).unwrap();
    let run_id = live.runs[0].id;
    assert_eq!(live.runs[0].status, WorkStatus::WaitingForInput);
    assert_eq!(live.permission_requests.len(), 1);
    assert_eq!(live.permission_requests[0].thread_id, live.threads[0].id);

    fixture
        .core
        .dispatch(AppCommand::AnswerPermission {
            run_id,
            request_id: "r1".into(),
            allow: true,
        })
        .unwrap();
    let live = fixture.core.cached_snapshot(fixture.workspace_id).unwrap();
    assert_eq!(
        *fixture.runner.answers.lock().unwrap(),
        [("r1".to_owned(), true)]
    );
    assert!(live.permission_requests.is_empty());
    assert_eq!(live.runs[0].status, WorkStatus::Working);
    assert_eq!(
        live.messages.last().unwrap().body,
        "Allowed **Bash**: `git push`"
    );
    assert!(
        fixture
            .core
            .dispatch(AppCommand::AnswerPermission {
                run_id,
                request_id: "r1".into(),
                allow: true,
            })
            .is_err(),
        "a request is answered once"
    );
}

fn post_to_builder(fixture: &Fixture, body: &str) {
    fixture
        .core
        .dispatch(AppCommand::PostMessage {
            channel_id: fixture.channel_id,
            thread_id: None,
            body: body.into(),
            attachments: vec![AttachmentTarget::Agent {
                agent_id: fixture.agent_id,
            }],
        })
        .unwrap();
}

#[test]
fn a_mention_in_a_reply_hands_the_thread_to_that_agent_with_context() {
    let fixture = fixture_with_events(vec![
        RunnerEvent::Reply("Done. @Reviewer please check it.".into()),
        RunnerEvent::Completed,
    ]);
    post_to_builder(&fixture, "Build it");

    let requests = fixture.runner.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "Builder, then Reviewer");
    let live = fixture.core.cached_snapshot(fixture.workspace_id).unwrap();
    let reviewer = live
        .sessions
        .iter()
        .find(|s| s.agent_id == fixture.collaborator_id)
        .expect("Reviewer joined the thread");
    assert_eq!(requests[1].1, reviewer.id);
    let prompt = &requests[1].3;
    assert!(prompt.contains("**User**: Build it"), "{prompt}");
    assert!(
        prompt.contains("**Builder**: Done. @Reviewer please check it."),
        "{prompt}"
    );
    assert!(
        prompt.contains("Builder mentioned you in this thread."),
        "{prompt}"
    );
}

#[test]
fn agents_mentioning_each_other_stop_after_the_hop_limit() {
    let fixture = fixture_with_events(vec![
        RunnerEvent::Reply("@Builder @Reviewer over to you".into()),
        RunnerEvent::Completed,
    ]);
    post_to_builder(&fixture, "Start");
    assert_eq!(fixture.runner.requests.lock().unwrap().len(), 1 + 4);
}

#[test]
fn usage_is_stored_and_cached_per_run() {
    let fixture = fixture_with_events(vec![
        RunnerEvent::Usage {
            input_tokens: 100,
            output_tokens: 20,
            cached_tokens: 50,
            cost_micros: Some(1234),
            duration_ms: Some(900),
        },
        RunnerEvent::Completed,
    ]);
    post_to_builder(&fixture, "Count");
    let live = fixture.core.cached_snapshot(fixture.workspace_id).unwrap();
    assert_eq!(live.usages.len(), 1);
    assert_eq!(live.usages[0].run_id, live.runs[0].id);
    assert_eq!(fixture.store.snapshot.lock().unwrap().usages, live.usages);
}
