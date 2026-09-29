use std::collections::HashMap;

use toma_domain::*;
use toma_storage::WorkspaceSnapshot;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ContextKey {
    pub channel_id: ChannelId,
    pub thread_id: Option<ThreadId>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ThreadStats {
    pub runs: usize,
    pub tokens: i64,
    pub cost_micros: Option<i64>,
    pub duration_ms: i64,
}

pub struct ShellViewModel {
    pub snapshot: WorkspaceSnapshot,
    selected_channel: Option<ChannelId>,
    open_thread: Option<ThreadId>,
    drafts: HashMap<ContextKey, String>,
}

impl ShellViewModel {
    pub fn new(mut snapshot: WorkspaceSnapshot) -> Self {
        snapshot.channels.sort_by_key(|channel| channel.position);
        let selected_channel = snapshot.channels.first().map(|channel| channel.id);
        let drafts = snapshot
            .drafts
            .iter()
            .map(|draft| {
                (
                    ContextKey {
                        channel_id: draft.channel_id,
                        thread_id: draft.thread_id,
                    },
                    draft.body.clone(),
                )
            })
            .collect();
        Self {
            snapshot,
            selected_channel,
            open_thread: None,
            drafts,
        }
    }

    /// Swaps in fresh durable state, keeping selection and the in-memory drafts,
    /// which are newer than anything the store holds.
    pub fn replace_snapshot(&mut self, mut snapshot: WorkspaceSnapshot) {
        snapshot.channels.sort_by_key(|channel| channel.position);
        self.snapshot = snapshot;
    }

    pub fn selected_channel_id(&self) -> Option<ChannelId> {
        self.selected_channel
    }
    pub fn open_thread_id(&self) -> Option<ThreadId> {
        self.open_thread
    }

    pub fn context(&self) -> Option<ContextKey> {
        self.selected_channel.map(|channel_id| ContextKey {
            channel_id,
            thread_id: self.open_thread,
        })
    }

    pub fn draft(&self) -> &str {
        self.context().map_or("", |key| self.draft_for(key))
    }

    pub fn set_draft(&mut self, body: impl Into<String>) {
        if let Some(key) = self.context() {
            self.set_draft_for(key, body);
        }
    }

    pub fn draft_for(&self, key: ContextKey) -> &str {
        self.drafts.get(&key).map_or("", String::as_str)
    }

    pub fn set_draft_for(&mut self, key: ContextKey, body: impl Into<String>) {
        self.drafts.insert(key, body.into());
    }

    /// Threads for the sidebar, most recently active first.
    pub fn recent_threads(&self) -> Vec<&TaskThread> {
        let mut threads: Vec<_> = self.snapshot.threads.iter().collect();
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
        threads
    }

    /// Agents in a thread with the status of their latest run, in the order they joined.
    pub fn thread_agents(&self, thread_id: ThreadId) -> Vec<(&AgentDefinition, WorkStatus)> {
        self.snapshot
            .sessions
            .iter()
            .filter(|session| session.thread_id == thread_id)
            .filter_map(|session| {
                let agent = self
                    .snapshot
                    .agents
                    .iter()
                    .find(|a| a.id == session.agent_id)?;
                let status = self
                    .snapshot
                    .runs
                    .iter()
                    .filter(|run| run.session_id == session.id)
                    .max_by_key(|run| run.sequence)
                    .map_or(session.status, |run| run.status);
                Some((agent, status))
            })
            .collect()
    }

    /// What all runs in a thread consumed. Time falls back to run timestamps when the
    /// provider does not report a duration.
    pub fn thread_stats(&self, thread_id: ThreadId) -> ThreadStats {
        let mut stats = ThreadStats::default();
        for run in self.snapshot.runs.iter().filter(|run| {
            self.snapshot
                .sessions
                .iter()
                .any(|s| s.id == run.session_id && s.thread_id == thread_id)
        }) {
            self.add_run_stats(&mut stats, run);
        }
        stats
    }

    /// What a single run consumed.
    pub fn run_stats(&self, run_id: RunId) -> ThreadStats {
        let mut stats = ThreadStats::default();
        if let Some(run) = self.run(run_id) {
            self.add_run_stats(&mut stats, run);
        }
        stats
    }

    fn add_run_stats(&self, stats: &mut ThreadStats, run: &Run) {
        stats.runs += 1;
        let usage = self.snapshot.usages.iter().find(|u| u.run_id == run.id);
        if let Some(usage) = usage {
            stats.tokens += usage.input_tokens + usage.output_tokens;
            if let Some(cost) = usage.cost_micros {
                *stats.cost_micros.get_or_insert(0) += cost;
            }
        }
        stats.duration_ms += usage
            .and_then(|u| u.duration_ms)
            .or_else(|| Some(run.finished_at? - run.started_at?))
            .unwrap_or(0);
    }

    pub fn run(&self, run_id: RunId) -> Option<&Run> {
        self.snapshot.runs.iter().find(|run| run.id == run_id)
    }

    /// The agent whose session `run_id` belongs to.
    pub fn run_agent(&self, run_id: RunId) -> Option<AgentId> {
        let run = self.run(run_id)?;
        self.snapshot
            .sessions
            .iter()
            .find(|session| session.id == run.session_id)
            .map(|session| session.agent_id)
    }

    /// Names of agents with a run still in progress in `thread_id`.
    pub fn working_agents(&self, thread_id: ThreadId) -> Vec<&str> {
        self.snapshot
            .sessions
            .iter()
            .filter(|session| session.thread_id == thread_id)
            .filter(|session| {
                self.snapshot.runs.iter().any(|run| {
                    run.session_id == session.id
                        && matches!(
                            run.status,
                            WorkStatus::Queued | WorkStatus::Reading | WorkStatus::Working
                        )
                })
            })
            .map(|session| self.author_name(MessageAuthor::Agent(session.agent_id)))
            .collect()
    }

    pub fn select_channel(&mut self, channel_id: ChannelId) {
        if self
            .snapshot
            .channels
            .iter()
            .any(|channel| channel.id == channel_id)
        {
            self.selected_channel = Some(channel_id);
            self.open_thread = None;
        }
    }

    pub fn open_thread(&mut self, thread_id: ThreadId) {
        if let Some(thread) = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
        {
            self.selected_channel = Some(thread.channel_id);
            self.open_thread = Some(thread_id);
        }
    }

    pub fn close_thread(&mut self) {
        self.open_thread = None;
    }

    pub fn selected_channel(&self) -> Option<&Channel> {
        let id = self.selected_channel?;
        self.snapshot
            .channels
            .iter()
            .find(|channel| channel.id == id)
    }

    pub fn messages_for(&self, thread_id: Option<ThreadId>) -> impl Iterator<Item = &Message> {
        let channel_id = self.selected_channel;
        self.snapshot.messages.iter().filter(move |message| {
            Some(message.channel_id) == channel_id && message.thread_id == thread_id
        })
    }

    /// Names that `@name` can refer to in a message: every agent and person.
    pub fn mention_names(&self) -> Vec<&str> {
        let agents = self.snapshot.agents.iter().map(|a| a.name.as_str());
        let people = self.snapshot.people.iter().map(|p| p.display_name.as_str());
        agents.chain(people).collect()
    }

    pub fn author_name(&self, author: MessageAuthor) -> &str {
        match author {
            MessageAuthor::Person(id) => self
                .snapshot
                .people
                .iter()
                .find(|person| person.id == id)
                .map(|person| person.display_name.as_str())
                .unwrap_or("Teammate"),
            MessageAuthor::Agent(id) => self
                .snapshot
                .agents
                .iter()
                .find(|agent| agent.id == id)
                .map(|agent| agent.name.as_str())
                .unwrap_or("Agent"),
            MessageAuthor::System => "Toma",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> (WorkspaceSnapshot, ChannelId, ChannelId, ThreadId) {
        let workspace_id = WorkspaceId::new();
        let first = ChannelId::new();
        let second = ChannelId::new();
        let thread = ThreadId::new();
        (
            WorkspaceSnapshot {
                channels: vec![
                    Channel {
                        id: second,
                        workspace_id,
                        name: "second".into(),
                        position: 2,
                        repository_path: None,
                    },
                    Channel {
                        id: first,
                        workspace_id,
                        name: "first".into(),
                        position: 1,
                        repository_path: None,
                    },
                ],
                threads: vec![TaskThread {
                    id: thread,
                    channel_id: first,
                    root_message_id: MessageId::new(),
                    title: "Thread".into(),
                    status: WorkStatus::Working,
                    created_at: 0,
                    updated_at: 0,
                    permission_mode: PermissionMode::Ask,
                }],
                ..Default::default()
            },
            first,
            second,
            thread,
        )
    }

    #[test]
    fn drafts_are_preserved_per_channel_and_thread() {
        let (snapshot, first, second, thread) = snapshot();
        let mut model = ShellViewModel::new(snapshot);
        assert_eq!(model.selected_channel_id(), Some(first));
        model.set_draft("channel one");
        model.select_channel(second);
        model.set_draft("channel two");
        model.select_channel(first);
        assert_eq!(model.draft(), "channel one");
        model.open_thread(thread);
        model.set_draft("thread reply");
        model.close_thread();
        assert_eq!(model.draft(), "channel one");
        model.open_thread(thread);
        assert_eq!(model.draft(), "thread reply");
    }

    #[test]
    fn selecting_a_channel_collapses_the_thread() {
        let (snapshot, _, second, thread) = snapshot();
        let mut model = ShellViewModel::new(snapshot);
        model.open_thread(thread);
        model.select_channel(second);
        assert_eq!(model.open_thread_id(), None);
        assert_eq!(model.selected_channel_id(), Some(second));
    }
}
