use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AppEvent {
    WorkspaceOpened {
        workspace_id: WorkspaceId,
    },
    ChannelCreated {
        channel_id: ChannelId,
    },
    ChannelSelected {
        channel_id: ChannelId,
    },
    MessagePosted {
        message_id: MessageId,
    },
    ThreadCreated {
        thread_id: ThreadId,
    },
    ThreadOpened {
        thread_id: ThreadId,
    },
    ThreadClosed,
    SessionAttached {
        session_id: SessionId,
        thread_id: ThreadId,
    },
    RunCreated {
        run_id: RunId,
        session_id: SessionId,
    },
    StatusChanged {
        run_id: RunId,
        status: WorkStatus,
    },
    DraftSaved {
        channel_id: ChannelId,
        thread_id: Option<ThreadId>,
    },
    RunnerOutput {
        run_id: RunId,
        text: String,
    },
    Error {
        message: String,
    },
}
