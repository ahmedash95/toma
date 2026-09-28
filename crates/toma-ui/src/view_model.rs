use std::collections::HashMap;

use toma_domain::*;
use toma_storage::WorkspaceSnapshot;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ContextKey {
    pub channel_id: ChannelId,
    pub thread_id: Option<ThreadId>,
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
        self.context()
            .and_then(|key| self.drafts.get(&key).map(String::as_str))
            .unwrap_or("")
    }

    pub fn set_draft(&mut self, body: impl Into<String>) {
        if let Some(key) = self.context() {
            self.drafts.insert(key, body.into());
        }
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

    pub fn active_runs(&self) -> Vec<(&AgentDefinition, &Run)> {
        self.snapshot
            .runs
            .iter()
            .filter_map(|run| {
                let session = self
                    .snapshot
                    .sessions
                    .iter()
                    .find(|session| session.id == run.session_id)?;
                let agent = self
                    .snapshot
                    .agents
                    .iter()
                    .find(|agent| agent.id == session.agent_id)?;
                Some((agent, run))
            })
            .collect()
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
                    },
                    Channel {
                        id: first,
                        workspace_id,
                        name: "first".into(),
                        position: 1,
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
