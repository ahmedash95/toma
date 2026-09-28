use super::*;
use std::path::Path;
use tempfile::TempDir;

struct Fixture {
    workspace: Workspace,
    channel: Channel,
    person: Person,
    agent: AgentDefinition,
    root_message: Message,
    attachment: Attachment,
    thread: TaskThread,
    session: AgentSession,
    run: Run,
    memory: ChannelMemory,
    draft: Draft,
    worktree: WorktreeRecord,
}

impl Fixture {
    fn new(repository_path: &Path) -> Self {
        let workspace = Workspace {
            id: WorkspaceId::new(),
            name: "Test workspace".into(),
            repository_path: repository_path.into(),
            created_at: 100,
        };
        let channel = Channel {
            id: ChannelId::new(),
            workspace_id: workspace.id,
            name: "general".into(),
            position: 0,
            repository_path: Some(repository_path.join("app")),
        };
        let person = Person {
            id: PersonId::new(),
            workspace_id: workspace.id,
            display_name: "Ada".into(),
        };
        let agent = AgentDefinition {
            id: AgentId::new(),
            workspace_id: workspace.id,
            name: "Codex".into(),
            role: "Engineer".into(),
            instructions: "Be precise.".into(),
            provider: RunnerProvider::CodexCli,
            enabled: true,
        };
        let root_message = Message {
            id: MessageId::new(),
            channel_id: channel.id,
            thread_id: None,
            author: MessageAuthor::Person(person.id),
            body: "Implement persistence".into(),
            created_at: 110,
        };
        let attachment = Attachment {
            id: AttachmentId::new(),
            message_id: root_message.id,
            target: AttachmentTarget::RepositoryFile {
                path: "src/lib.rs".into(),
                revision: Some("abc123".into()),
            },
        };
        let thread = TaskThread {
            id: ThreadId::new(),
            channel_id: channel.id,
            root_message_id: root_message.id,
            title: "Storage".into(),
            status: WorkStatus::Queued,
            created_at: 120,
            updated_at: 120,
        };
        let session = AgentSession {
            id: SessionId::new(),
            thread_id: thread.id,
            agent_id: agent.id,
            provider_session_id: Some("provider-1".into()),
            status: WorkStatus::Queued,
            created_at: 130,
        };
        let run = Run {
            id: RunId::new(),
            session_id: session.id,
            sequence: 1,
            status: WorkStatus::Queued,
            started_at: None,
            finished_at: None,
        };
        let memory = ChannelMemory {
            id: MemoryId::new(),
            channel_id: channel.id,
            fact: "SQLite is required".into(),
            source_message_id: root_message.id,
            supersedes_id: None,
            active: true,
            created_at: 140,
        };
        let draft = Draft {
            channel_id: channel.id,
            thread_id: Some(thread.id),
            body: "first draft".into(),
            updated_at: 150,
        };
        let worktree = WorktreeRecord {
            thread_id: thread.id,
            path: repository_path.join("worktrees/storage"),
            branch: "codex/storage".into(),
            created_at: 160,
        };
        Self {
            workspace,
            channel,
            person,
            agent,
            root_message,
            attachment,
            thread,
            session,
            run,
            memory,
            draft,
            worktree,
        }
    }

    fn persist(&self, store: &SqliteStore) {
        store.insert_workspace(&self.workspace).unwrap();
        store.insert_channel(&self.channel).unwrap();
        store.insert_person(&self.person).unwrap();
        store.insert_agent(&self.agent).unwrap();
        store
            .insert_message(&self.root_message, std::slice::from_ref(&self.attachment))
            .unwrap();
        store
            .insert_thread_bundle(&self.thread, &self.session, &self.run)
            .unwrap();
        store.insert_memory(&self.memory).unwrap();
        store.save_draft(&self.draft).unwrap();
        store.save_worktree(&self.worktree).unwrap();
    }
}

#[test]
fn round_trips_every_snapshot_entity_across_restart() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("toma.sqlite3");
    let fixture = Fixture::new(directory.path());

    {
        let store = SqliteStore::open(&database).unwrap();
        fixture.persist(&store);
        store
            .append_raw_history(fixture.run.id, 0, "{\"event\":\"started\"}", 170)
            .unwrap();
    }

    let store = SqliteStore::open(&database).unwrap();
    let snapshot = store.snapshot(fixture.workspace.id).unwrap();
    assert_eq!(snapshot.workspace, Some(fixture.workspace));
    assert_eq!(snapshot.channels, vec![fixture.channel]);
    assert_eq!(snapshot.people, vec![fixture.person]);
    assert_eq!(snapshot.agents, vec![fixture.agent]);
    assert_eq!(snapshot.messages, vec![fixture.root_message]);
    assert_eq!(snapshot.attachments, vec![fixture.attachment]);
    assert_eq!(snapshot.threads, vec![fixture.thread]);
    assert_eq!(snapshot.sessions, vec![fixture.session]);
    assert_eq!(snapshot.runs, vec![fixture.run.clone()]);
    assert_eq!(snapshot.drafts, vec![fixture.draft]);
    assert_eq!(snapshot.memories, vec![fixture.memory]);
    assert_eq!(snapshot.worktrees, vec![fixture.worktree]);
    assert_eq!(
        store.raw_history(fixture.run.id).unwrap(),
        vec![RawHistoryEntry {
            run_id: fixture.run.id,
            sequence: 0,
            payload: "{\"event\":\"started\"}".into(),
            created_at: 170,
        }]
    );
}

#[test]
fn draft_and_worktree_writes_are_upserts() {
    let store = SqliteStore::open_in_memory().unwrap();
    let fixture = Fixture::new(Path::new("/repo"));
    fixture.persist(&store);

    let mut draft = fixture.draft.clone();
    draft.body = "replacement".into();
    draft.updated_at = 200;
    store.save_draft(&draft).unwrap();

    let mut worktree = fixture.worktree.clone();
    worktree.path = "/repo/new-worktree".into();
    worktree.branch = "codex/new".into();
    store.save_worktree(&worktree).unwrap();

    let snapshot = store.snapshot(fixture.workspace.id).unwrap();
    assert_eq!(snapshot.drafts, vec![draft]);
    assert_eq!(snapshot.worktrees, vec![worktree]);
}

#[test]
fn bundle_failure_rolls_back_every_row() {
    let store = SqliteStore::open_in_memory().unwrap();
    let fixture = Fixture::new(Path::new("/repo"));
    store.insert_workspace(&fixture.workspace).unwrap();
    store.insert_channel(&fixture.channel).unwrap();
    store.insert_person(&fixture.person).unwrap();
    store.insert_agent(&fixture.agent).unwrap();
    store.insert_message(&fixture.root_message, &[]).unwrap();

    let mut invalid_session = fixture.session.clone();
    invalid_session.agent_id = AgentId::new();
    assert!(
        store
            .insert_thread_bundle(&fixture.thread, &invalid_session, &fixture.run)
            .is_err()
    );

    let snapshot = store.snapshot(fixture.workspace.id).unwrap();
    assert!(snapshot.threads.is_empty());
    assert!(snapshot.sessions.is_empty());
    assert!(snapshot.runs.is_empty());
}

#[test]
fn status_update_validates_and_propagates_the_lifecycle() {
    let store = SqliteStore::open_in_memory().unwrap();
    let fixture = Fixture::new(Path::new("/repo"));
    fixture.persist(&store);

    store
        .update_run_status(fixture.run.id, WorkStatus::Working, 200)
        .unwrap();
    store
        .update_run_status(fixture.run.id, WorkStatus::Completed, 250)
        .unwrap();
    assert!(
        store
            .update_run_status(fixture.run.id, WorkStatus::Working, 300)
            .is_err()
    );

    let snapshot = store.snapshot(fixture.workspace.id).unwrap();
    assert_eq!(snapshot.runs[0].status, WorkStatus::Completed);
    assert_eq!(snapshot.runs[0].started_at, Some(200));
    assert_eq!(snapshot.runs[0].finished_at, Some(250));
    assert_eq!(snapshot.sessions[0].status, WorkStatus::Completed);
    assert_eq!(snapshot.threads[0].status, WorkStatus::Completed);
    assert_eq!(snapshot.threads[0].updated_at, 250);
}

#[test]
fn raw_history_is_append_only_and_survives_failed_replacement() {
    let store = SqliteStore::open_in_memory().unwrap();
    let fixture = Fixture::new(Path::new("/repo"));
    fixture.persist(&store);
    store
        .append_raw_history(fixture.run.id, 1, "original", 200)
        .unwrap();

    assert!(
        store
            .append_raw_history(fixture.run.id, 1, "replacement", 201)
            .is_err()
    );
    let connection = store.connection().unwrap();
    assert!(
        connection
            .execute("UPDATE raw_history SET payload = 'changed'", [])
            .is_err()
    );
    assert!(connection.execute("DELETE FROM raw_history", []).is_err());
    drop(connection);

    assert_eq!(
        store.raw_history(fixture.run.id).unwrap()[0].payload,
        "original"
    );
}

#[test]
fn file_databases_enable_wal_and_foreign_keys() {
    let directory = TempDir::new().unwrap();
    let store = SqliteStore::open(directory.path().join("toma.sqlite3")).unwrap();
    let connection = store.connection().unwrap();
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    let foreign_keys: i64 = connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap();
    let user_version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
    assert_eq!(foreign_keys, 1);
    assert_eq!(user_version, SCHEMA_VERSION);
}

#[test]
fn deterministic_demo_seed_is_idempotent() {
    let store = SqliteStore::open_in_memory().unwrap();
    let first = store.seed_demo_workspace("/repo").unwrap();
    let second = store.seed_demo_workspace("/different-repo").unwrap();
    assert_eq!(first, second);

    let snapshot = store.snapshot(first).unwrap();
    assert_eq!(
        snapshot.workspace.unwrap().repository_path,
        PathBuf::from("/repo")
    );
    assert_eq!(snapshot.channels.len(), 2);
    assert_eq!(snapshot.agents.len(), 3);
}

#[test]
fn missing_workspace_has_an_empty_snapshot() {
    let store = SqliteStore::open_in_memory().unwrap();
    assert_eq!(
        store.snapshot(WorkspaceId::new()).unwrap(),
        WorkspaceSnapshot::default()
    );
}

#[test]
fn run_usage_is_saved_once_per_run_and_loaded_with_the_workspace() {
    let store = SqliteStore::open_in_memory().unwrap();
    let fixture = Fixture::new(Path::new("/repo"));
    fixture.persist(&store);
    let mut usage = RunUsage {
        run_id: fixture.run.id,
        input_tokens: 1200,
        output_tokens: 80,
        cached_tokens: 900,
        cost_micros: Some(11_700),
        duration_ms: Some(2_500),
    };
    store.save_run_usage(&usage).unwrap();
    usage.output_tokens = 90;
    store.save_run_usage(&usage).unwrap();

    assert_eq!(
        store.snapshot(fixture.workspace.id).unwrap().usages,
        vec![usage]
    );
}
