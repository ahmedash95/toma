CREATE TABLE workspaces (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    repository_path TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE channels (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    position INTEGER NOT NULL,
    UNIQUE(workspace_id, name)
);

CREATE TABLE people (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    display_name TEXT NOT NULL
);

CREATE TABLE agents (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    role TEXT NOT NULL,
    instructions TEXT NOT NULL,
    provider TEXT NOT NULL,
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1))
);

CREATE TABLE messages (
    id TEXT PRIMARY KEY NOT NULL,
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    thread_id TEXT REFERENCES threads(id) ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED,
    author TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE attachments (
    id TEXT PRIMARY KEY NOT NULL,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    target TEXT NOT NULL
);

CREATE TABLE threads (
    id TEXT PRIMARY KEY NOT NULL,
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    root_message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY NOT NULL,
    thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE RESTRICT,
    provider_session_id TEXT,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE runs (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    status TEXT NOT NULL,
    started_at INTEGER,
    finished_at INTEGER,
    UNIQUE(session_id, sequence)
);

CREATE TABLE drafts (
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    thread_id TEXT REFERENCES threads(id) ON DELETE CASCADE,
    body TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX drafts_scope ON drafts(channel_id, IFNULL(thread_id, ''));

CREATE TABLE memories (
    id TEXT PRIMARY KEY NOT NULL,
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    fact TEXT NOT NULL,
    source_message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE RESTRICT,
    supersedes_id TEXT REFERENCES memories(id) ON DELETE SET NULL,
    active INTEGER NOT NULL CHECK(active IN (0, 1)),
    created_at INTEGER NOT NULL
);

CREATE TABLE worktrees (
    thread_id TEXT PRIMARY KEY NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    branch TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE raw_history (
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE RESTRICT,
    sequence INTEGER NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(run_id, sequence)
);

CREATE TRIGGER raw_history_no_update
BEFORE UPDATE ON raw_history BEGIN
    SELECT RAISE(ABORT, 'raw_history is immutable');
END;

CREATE TRIGGER raw_history_no_delete
BEFORE DELETE ON raw_history BEGIN
    SELECT RAISE(ABORT, 'raw_history is immutable');
END;

CREATE INDEX channels_workspace ON channels(workspace_id, position);
CREATE INDEX messages_channel ON messages(channel_id, created_at);
CREATE INDEX threads_channel ON threads(channel_id, created_at);
CREATE INDEX sessions_thread ON sessions(thread_id, created_at);
CREATE INDEX runs_session ON runs(session_id, sequence);
CREATE INDEX attachments_message ON attachments(message_id);
CREATE INDEX memories_channel ON memories(channel_id, created_at);
