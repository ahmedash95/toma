use crate::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AppCommand {
    OpenWorkspace {
        repository_path: PathBuf,
    },
    SelectChannel {
        channel_id: ChannelId,
    },
    OpenThread {
        thread_id: ThreadId,
    },
    CloseThread,
    SaveDraft {
        draft: Draft,
    },
    PostMessage {
        channel_id: ChannelId,
        thread_id: Option<ThreadId>,
        body: String,
        attachments: Vec<AttachmentTarget>,
    },
    AttachAgent {
        thread_id: ThreadId,
        agent_id: AgentId,
    },
    CancelRun {
        run_id: RunId,
    },
}
