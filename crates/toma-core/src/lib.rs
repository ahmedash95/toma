use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

use thiserror::Error;
use toma_domain::*;
use toma_runner::{AgentRunner, RunRequest, RunnerError, RunnerEvent};
use toma_storage::{StorageError, TomaStore, WorkspaceSnapshot};
use toma_worktree::{WorktreeError, WorktreeManager, WorktreeRequest};

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Runner(#[from] RunnerError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error("invalid command: {0}")]
    InvalidCommand(String),
}

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> TimestampMs;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> TimestampMs {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as TimestampMs)
    }
}

#[derive(Default)]
struct CoreState {
    snapshots: HashMap<WorkspaceId, WorkspaceSnapshot>,
}

pub struct TomaCore {
    pub store: Arc<dyn TomaStore>,
    pub worktrees: Arc<dyn WorktreeManager>,
    pub runners: Vec<Arc<dyn AgentRunner>>,
    pub clock: Arc<dyn Clock>,
    state: Mutex<CoreState>,
}

impl TomaCore {
    pub fn new(
        store: Arc<dyn TomaStore>,
        worktrees: Arc<dyn WorktreeManager>,
        runners: Vec<Arc<dyn AgentRunner>>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            worktrees,
            runners,
            clock,
            state: Mutex::new(CoreState::default()),
        }
    }

    /// Loads durable state and makes it available to subsequent ID-based commands.
    pub fn snapshot(&self, workspace_id: WorkspaceId) -> Result<WorkspaceSnapshot, CoreError> {
        let snapshot = self.store.snapshot(workspace_id)?;
        self.state()?
            .snapshots
            .insert(workspace_id, snapshot.clone());
        Ok(snapshot)
    }

    /// Loads the workspace after a restart. Runs that were executing belonged to CLI
    /// processes that died with the previous app, so they are failed rather than resumed.
    pub fn recover(&self, workspace_id: WorkspaceId) -> Result<WorkspaceSnapshot, CoreError> {
        let snapshot = self.snapshot(workspace_id)?;
        let mut events = Vec::new();
        for run in &snapshot.runs {
            if matches!(
                run.status,
                WorkStatus::Queued | WorkStatus::Reading | WorkStatus::Working
            ) {
                self.transition_run(run.id, WorkStatus::Failed, &mut events)?;
            }
        }
        if events.is_empty() {
            Ok(snapshot)
        } else {
            self.snapshot(workspace_id)
        }
    }

    pub fn dispatch(&self, command: AppCommand) -> Result<Vec<AppEvent>, CoreError> {
        match command {
            AppCommand::OpenWorkspace { repository_path } => self.open_workspace(&repository_path),
            AppCommand::SelectChannel { channel_id } => {
                self.workspace_for_channel(channel_id)?;
                Ok(vec![AppEvent::ChannelSelected { channel_id }])
            }
            AppCommand::OpenThread { thread_id } => {
                self.workspace_for_thread(thread_id)?;
                Ok(vec![AppEvent::ThreadOpened { thread_id }])
            }
            AppCommand::CloseThread => Ok(vec![AppEvent::ThreadClosed]),
            AppCommand::SaveDraft { draft } => self.save_draft(draft),
            AppCommand::PostMessage {
                channel_id,
                thread_id,
                body,
                attachments,
            } => self.post_message(channel_id, thread_id, body, attachments),
            AppCommand::AttachAgent {
                thread_id,
                agent_id,
            } => self.attach_agent(thread_id, agent_id),
            AppCommand::CancelRun { run_id } => self.cancel_run(run_id),
        }
    }

    fn state(&self) -> Result<MutexGuard<'_, CoreState>, CoreError> {
        self.state
            .lock()
            .map_err(|_| CoreError::InvalidCommand("core state lock is poisoned".into()))
    }

    fn open_workspace(&self, repository_path: &Path) -> Result<Vec<AppEvent>, CoreError> {
        let state = self.state()?;
        let workspace_id = state
            .snapshots
            .iter()
            .find_map(|(workspace_id, snapshot)| {
                snapshot
                    .workspace
                    .as_ref()
                    .filter(|workspace| workspace.repository_path == repository_path)
                    .map(|_| *workspace_id)
            })
            .ok_or_else(|| {
                CoreError::InvalidCommand(format!(
                    "workspace at {} has not been loaded by id",
                    repository_path.display()
                ))
            })?;
        Ok(vec![AppEvent::WorkspaceOpened { workspace_id }])
    }

    fn save_draft(&self, draft: Draft) -> Result<Vec<AppEvent>, CoreError> {
        let workspace_id = self.workspace_for_channel(draft.channel_id)?;
        self.validate_thread_channel(workspace_id, draft.thread_id, draft.channel_id)?;
        self.store.save_draft(&draft)?;

        let event = AppEvent::DraftSaved {
            channel_id: draft.channel_id,
            thread_id: draft.thread_id,
        };
        let mut state = self.state()?;
        let snapshot = state.snapshots.get_mut(&workspace_id).expect("validated");
        if let Some(existing) = snapshot.drafts.iter_mut().find(|existing| {
            existing.channel_id == draft.channel_id && existing.thread_id == draft.thread_id
        }) {
            *existing = draft;
        } else {
            snapshot.drafts.push(draft);
        }
        Ok(vec![event])
    }

    fn post_message(
        &self,
        channel_id: ChannelId,
        thread_id: Option<ThreadId>,
        body: String,
        targets: Vec<AttachmentTarget>,
    ) -> Result<Vec<AppEvent>, CoreError> {
        if body.trim().is_empty() && targets.is_empty() {
            return Err(CoreError::InvalidCommand(
                "a message must contain text or an attachment".into(),
            ));
        }

        let workspace_id = self.workspace_for_channel(channel_id)?;
        self.validate_thread_channel(workspace_id, thread_id, channel_id)?;
        let message = Message {
            id: MessageId::new(),
            channel_id,
            thread_id,
            author: self.default_author(workspace_id)?,
            body,
            created_at: self.clock.now_ms(),
        };
        let attachments: Vec<_> = targets
            .iter()
            .cloned()
            .map(|target| Attachment {
                id: AttachmentId::new(),
                message_id: message.id,
                target,
            })
            .collect();

        self.store.insert_message(&message, &attachments)?;
        {
            let mut state = self.state()?;
            let snapshot = state.snapshots.get_mut(&workspace_id).expect("validated");
            snapshot.messages.push(message.clone());
            snapshot.attachments.extend(attachments);
        }

        let mut events = vec![AppEvent::MessagePosted {
            message_id: message.id,
        }];
        let agent_ids = unique_agent_targets(&targets);
        if agent_ids.is_empty() {
            return Ok(events);
        }

        if let Some(thread_id) = thread_id {
            for agent_id in agent_ids {
                events.extend(self.attach_agent_with_prompt(
                    workspace_id,
                    thread_id,
                    agent_id,
                    &message.body,
                    false,
                )?);
            }
        } else {
            let created = self.create_thread(workspace_id, &message, agent_ids[0])?;
            events.extend(created.events);
            self.execute_run(
                workspace_id,
                created.thread_id,
                created.session_id,
                created.run_id,
                &message.body,
                &mut events,
            )?;
            for agent_id in agent_ids.into_iter().skip(1) {
                events.extend(self.attach_agent_with_prompt(
                    workspace_id,
                    created.thread_id,
                    agent_id,
                    &message.body,
                    false,
                )?);
            }
        }
        Ok(events)
    }

    fn attach_agent(
        &self,
        thread_id: ThreadId,
        agent_id: AgentId,
    ) -> Result<Vec<AppEvent>, CoreError> {
        let workspace_id = self.workspace_for_thread(thread_id)?;
        let prompt = self.root_prompt(workspace_id, thread_id)?;
        self.attach_agent_with_prompt(workspace_id, thread_id, agent_id, &prompt, true)
    }

    fn attach_agent_with_prompt(
        &self,
        workspace_id: WorkspaceId,
        thread_id: ThreadId,
        agent_id: AgentId,
        prompt: &str,
        reject_duplicate: bool,
    ) -> Result<Vec<AppEvent>, CoreError> {
        let agent = self.agent(workspace_id, agent_id)?;
        if !agent.enabled {
            return Err(CoreError::InvalidCommand(format!(
                "agent {} is disabled",
                agent.name
            )));
        }

        {
            let state = self.state()?;
            let snapshot = state.snapshots.get(&workspace_id).expect("validated");
            if reject_duplicate
                && snapshot
                    .sessions
                    .iter()
                    .any(|session| session.thread_id == thread_id && session.agent_id == agent_id)
            {
                return Err(CoreError::InvalidCommand(
                    "agent is already attached to this thread".into(),
                ));
            }
        }
        let session = AgentSession {
            id: SessionId::new(),
            thread_id,
            agent_id,
            provider_session_id: None,
            status: WorkStatus::Queued,
            created_at: self.clock.now_ms(),
        };
        let run = Run {
            id: RunId::new(),
            session_id: session.id,
            sequence: 1,
            status: WorkStatus::Queued,
            started_at: None,
            finished_at: None,
        };
        self.store.insert_session_bundle(&session, &run)?;
        {
            let mut state = self.state()?;
            let snapshot = state.snapshots.get_mut(&workspace_id).expect("validated");
            snapshot.sessions.push(session.clone());
            snapshot.runs.push(run.clone());
        }

        let mut events = vec![
            AppEvent::SessionAttached {
                session_id: session.id,
                thread_id,
            },
            AppEvent::RunCreated {
                run_id: run.id,
                session_id: session.id,
            },
        ];
        self.execute_run(
            workspace_id,
            thread_id,
            session.id,
            run.id,
            prompt,
            &mut events,
        )?;
        Ok(events)
    }

    fn create_thread(
        &self,
        workspace_id: WorkspaceId,
        root_message: &Message,
        agent_id: AgentId,
    ) -> Result<CreatedThread, CoreError> {
        let agent = self.agent(workspace_id, agent_id)?;
        if !agent.enabled {
            return Err(CoreError::InvalidCommand(format!(
                "agent {} is disabled",
                agent.name
            )));
        }

        let now = self.clock.now_ms();
        let thread = TaskThread {
            id: ThreadId::new(),
            channel_id: root_message.channel_id,
            root_message_id: root_message.id,
            title: thread_title(&root_message.body, &agent.name),
            status: WorkStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let session = AgentSession {
            id: SessionId::new(),
            thread_id: thread.id,
            agent_id,
            provider_session_id: None,
            status: WorkStatus::Queued,
            created_at: now,
        };
        let run = Run {
            id: RunId::new(),
            session_id: session.id,
            sequence: 1,
            status: WorkStatus::Queued,
            started_at: None,
            finished_at: None,
        };
        self.store.insert_thread_bundle(&thread, &session, &run)?;
        {
            let mut state = self.state()?;
            let snapshot = state.snapshots.get_mut(&workspace_id).expect("validated");
            snapshot.threads.push(thread.clone());
            snapshot.sessions.push(session.clone());
            snapshot.runs.push(run.clone());
        }

        Ok(CreatedThread {
            thread_id: thread.id,
            session_id: session.id,
            run_id: run.id,
            events: vec![
                AppEvent::ThreadCreated {
                    thread_id: thread.id,
                },
                AppEvent::SessionAttached {
                    session_id: session.id,
                    thread_id: thread.id,
                },
                AppEvent::RunCreated {
                    run_id: run.id,
                    session_id: session.id,
                },
            ],
        })
    }

    fn execute_run(
        &self,
        workspace_id: WorkspaceId,
        thread_id: ThreadId,
        session_id: SessionId,
        run_id: RunId,
        prompt: &str,
        events: &mut Vec<AppEvent>,
    ) -> Result<(), CoreError> {
        let (provider, provider_session_id, instructions) = {
            let state = self.state()?;
            let snapshot = state.snapshots.get(&workspace_id).expect("validated");
            let session = snapshot
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .expect("created session");
            let provider = snapshot
                .agents
                .iter()
                .find(|agent| agent.id == session.agent_id)
                .expect("validated agent")
                .provider;
            let instructions = snapshot
                .agents
                .iter()
                .find(|agent| agent.id == session.agent_id)
                .expect("validated agent")
                .instructions
                .clone();
            (provider, session.provider_session_id.clone(), instructions)
        };
        let runner = self.runner(provider)?;
        let worktree = match self.ensure_worktree(workspace_id, thread_id) {
            Ok(worktree) => worktree,
            Err(error) => {
                self.fail_run(run_id, error.to_string(), events)?;
                return Ok(());
            }
        };
        let _lease = match self
            .worktrees
            .acquire_write_lock(thread_id, &session_id.to_string())
        {
            Ok(lease) => lease,
            Err(error) => {
                self.fail_run(run_id, error.to_string(), events)?;
                return Ok(());
            }
        };
        if let Err(error) = runner.availability() {
            self.fail_run(run_id, error.to_string(), events)?;
            return Ok(());
        }

        let effective_prompt = if instructions.trim().is_empty() {
            prompt.to_owned()
        } else {
            format!("{instructions}\n\n{prompt}")
        };
        let mut emitted = Vec::new();
        let result = runner.run(
            RunRequest {
                run_id,
                session_id,
                provider_session_id: provider_session_id.as_deref(),
                working_directory: &worktree.path,
                prompt: &effective_prompt,
            },
            &mut |event| emitted.push(event),
        );
        for (index, event) in emitted.into_iter().enumerate() {
            self.handle_runner_event(run_id, index as i64 + 1, event, events)?;
        }
        if let Err(error) = result {
            self.fail_run(run_id, error.to_string(), events)?;
        }
        Ok(())
    }

    fn handle_runner_event(
        &self,
        run_id: RunId,
        sequence: i64,
        event: RunnerEvent,
        events: &mut Vec<AppEvent>,
    ) -> Result<(), CoreError> {
        let now = self.clock.now_ms();
        self.store
            .append_raw_history(run_id, sequence, &raw_payload(&event), now)?;

        match event {
            RunnerEvent::Started {
                provider_session_id,
            } => {
                if let Some(provider_session_id) = provider_session_id {
                    let session_id = self.set_provider_session_id(run_id, &provider_session_id)?;
                    self.store
                        .set_provider_session_id(session_id, &provider_session_id)?;
                }
                self.transition_run(run_id, WorkStatus::Working, events)?;
            }
            RunnerEvent::Output(text) => {
                self.ensure_run_started(run_id, events)?;
                events.push(AppEvent::RunnerOutput { run_id, text });
            }
            RunnerEvent::Reply(body) => {
                let message_id = self.post_agent_reply(run_id, body, now)?;
                events.push(AppEvent::MessagePosted { message_id });
            }
            RunnerEvent::WaitingForInput(text) => {
                self.ensure_run_started(run_id, events)?;
                events.push(AppEvent::RunnerOutput { run_id, text });
                self.transition_run(run_id, WorkStatus::WaitingForInput, events)?;
            }
            RunnerEvent::Completed => {
                self.ensure_run_started(run_id, events)?;
                self.transition_run(run_id, WorkStatus::Completed, events)?;
            }
            RunnerEvent::Failed(message) => {
                events.push(AppEvent::RunnerOutput {
                    run_id,
                    text: message,
                });
                self.transition_run(run_id, WorkStatus::Failed, events)?;
            }
            RunnerEvent::Cancelled => {
                self.transition_run(run_id, WorkStatus::Cancelled, events)?;
            }
        }
        Ok(())
    }

    fn fail_run(
        &self,
        run_id: RunId,
        message: String,
        events: &mut Vec<AppEvent>,
    ) -> Result<(), CoreError> {
        let now = self.clock.now_ms();
        self.store.append_raw_history(
            run_id,
            now,
            &format!("orchestrator_error:{message}"),
            now,
        )?;
        if !self.run_status(run_id)?.is_terminal() {
            self.transition_run(run_id, WorkStatus::Failed, events)?;
        }
        events.push(AppEvent::Error { message });
        Ok(())
    }

    fn ensure_run_started(
        &self,
        run_id: RunId,
        events: &mut Vec<AppEvent>,
    ) -> Result<(), CoreError> {
        if self.run_status(run_id)? == WorkStatus::Queued {
            self.transition_run(run_id, WorkStatus::Working, events)?;
        }
        Ok(())
    }

    fn transition_run(
        &self,
        run_id: RunId,
        next: WorkStatus,
        events: &mut Vec<AppEvent>,
    ) -> Result<(), CoreError> {
        let current = self.run_status(run_id)?;
        if current == next {
            return Ok(());
        }
        current
            .transition_to(next)
            .map_err(|error| CoreError::InvalidCommand(error.to_string()))?;
        let now = self.clock.now_ms();
        self.store.update_run_status(run_id, next, now)?;
        self.update_cached_run(run_id, next, now)?;
        events.push(AppEvent::StatusChanged {
            run_id,
            status: next,
        });
        Ok(())
    }

    fn cancel_run(&self, run_id: RunId) -> Result<Vec<AppEvent>, CoreError> {
        let (provider, status) = self.run_provider_and_status(run_id)?;
        if status.is_terminal() {
            return Err(CoreError::InvalidCommand(
                "a terminal run cannot be cancelled".into(),
            ));
        }
        self.runner(provider)?.cancel(run_id)?;
        let mut events = Vec::new();
        self.transition_run(run_id, WorkStatus::Cancelled, &mut events)?;
        Ok(events)
    }

    fn ensure_worktree(
        &self,
        workspace_id: WorkspaceId,
        thread_id: ThreadId,
    ) -> Result<WorktreeRecord, CoreError> {
        let (workspace, existing) = {
            let state = self.state()?;
            let snapshot = state.snapshots.get(&workspace_id).expect("validated");
            (
                snapshot.workspace.clone().ok_or_else(|| {
                    CoreError::InvalidCommand("workspace metadata is unavailable".into())
                })?,
                snapshot
                    .worktrees
                    .iter()
                    .find(|record| record.thread_id == thread_id)
                    .cloned(),
            )
        };
        if let Some(existing) = existing {
            return Ok(existing);
        }

        let root = workspace.repository_path.join(".toma").join("worktrees");
        let record = self.worktrees.ensure(WorktreeRequest {
            repository_path: &workspace.repository_path,
            worktree_root: &root,
            thread_id,
            base_ref: "HEAD",
            created_at: self.clock.now_ms(),
        })?;
        self.store.save_worktree(&record)?;
        self.state()?
            .snapshots
            .get_mut(&workspace_id)
            .expect("validated")
            .worktrees
            .push(record.clone());
        Ok(record)
    }

    fn workspace_for_channel(&self, channel_id: ChannelId) -> Result<WorkspaceId, CoreError> {
        self.state()?
            .snapshots
            .iter()
            .find_map(|(workspace_id, snapshot)| {
                snapshot
                    .channels
                    .iter()
                    .any(|channel| channel.id == channel_id)
                    .then_some(*workspace_id)
            })
            .ok_or_else(|| CoreError::InvalidCommand("unknown or unloaded channel".into()))
    }

    fn workspace_for_thread(&self, thread_id: ThreadId) -> Result<WorkspaceId, CoreError> {
        self.state()?
            .snapshots
            .iter()
            .find_map(|(workspace_id, snapshot)| {
                snapshot
                    .threads
                    .iter()
                    .any(|thread| thread.id == thread_id)
                    .then_some(*workspace_id)
            })
            .ok_or_else(|| CoreError::InvalidCommand("unknown or unloaded thread".into()))
    }

    fn validate_thread_channel(
        &self,
        workspace_id: WorkspaceId,
        thread_id: Option<ThreadId>,
        channel_id: ChannelId,
    ) -> Result<(), CoreError> {
        let Some(thread_id) = thread_id else {
            return Ok(());
        };
        let state = self.state()?;
        let valid = state
            .snapshots
            .get(&workspace_id)
            .expect("validated")
            .threads
            .iter()
            .any(|thread| thread.id == thread_id && thread.channel_id == channel_id);
        valid
            .then_some(())
            .ok_or_else(|| CoreError::InvalidCommand("thread does not belong to channel".into()))
    }

    fn default_author(&self, workspace_id: WorkspaceId) -> Result<MessageAuthor, CoreError> {
        Ok(self
            .state()?
            .snapshots
            .get(&workspace_id)
            .expect("validated")
            .people
            .first()
            .map(|person| MessageAuthor::Person(person.id))
            .unwrap_or(MessageAuthor::System))
    }

    fn agent(
        &self,
        workspace_id: WorkspaceId,
        agent_id: AgentId,
    ) -> Result<AgentDefinition, CoreError> {
        self.state()?
            .snapshots
            .get(&workspace_id)
            .expect("validated")
            .agents
            .iter()
            .find(|agent| agent.id == agent_id)
            .cloned()
            .ok_or_else(|| CoreError::InvalidCommand("agent does not belong to workspace".into()))
    }

    fn runner(&self, provider: RunnerProvider) -> Result<Arc<dyn AgentRunner>, CoreError> {
        self.runners
            .iter()
            .find(|runner| runner.provider() == provider)
            .cloned()
            .ok_or_else(|| {
                CoreError::InvalidCommand(format!("no runner configured for {provider:?}"))
            })
    }

    fn root_prompt(
        &self,
        workspace_id: WorkspaceId,
        thread_id: ThreadId,
    ) -> Result<String, CoreError> {
        let state = self.state()?;
        let snapshot = state.snapshots.get(&workspace_id).expect("validated");
        let thread = snapshot
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .expect("validated");
        snapshot
            .messages
            .iter()
            .find(|message| message.id == thread.root_message_id)
            .map(|message| message.body.clone())
            .ok_or_else(|| CoreError::InvalidCommand("thread root message is unavailable".into()))
    }

    fn run_status(&self, run_id: RunId) -> Result<WorkStatus, CoreError> {
        self.state()?
            .snapshots
            .values()
            .flat_map(|snapshot| snapshot.runs.iter())
            .find(|run| run.id == run_id)
            .map(|run| run.status)
            .ok_or_else(|| CoreError::InvalidCommand("unknown or unloaded run".into()))
    }

    fn run_provider_and_status(
        &self,
        run_id: RunId,
    ) -> Result<(RunnerProvider, WorkStatus), CoreError> {
        let state = self.state()?;
        for snapshot in state.snapshots.values() {
            if let Some(run) = snapshot.runs.iter().find(|run| run.id == run_id) {
                let session = snapshot
                    .sessions
                    .iter()
                    .find(|session| session.id == run.session_id)
                    .ok_or_else(|| {
                        CoreError::InvalidCommand("run session is unavailable".into())
                    })?;
                let agent = snapshot
                    .agents
                    .iter()
                    .find(|agent| agent.id == session.agent_id)
                    .ok_or_else(|| {
                        CoreError::InvalidCommand("session agent is unavailable".into())
                    })?;
                return Ok((agent.provider, run.status));
            }
        }
        Err(CoreError::InvalidCommand("unknown or unloaded run".into()))
    }

    fn update_cached_run(
        &self,
        run_id: RunId,
        status: WorkStatus,
        at: TimestampMs,
    ) -> Result<(), CoreError> {
        let mut state = self.state()?;
        for snapshot in state.snapshots.values_mut() {
            if let Some(run) = snapshot.runs.iter_mut().find(|run| run.id == run_id) {
                run.status = status;
                if status == WorkStatus::Working && run.started_at.is_none() {
                    run.started_at = Some(at);
                }
                if status.is_terminal() {
                    run.finished_at = Some(at);
                }
                return Ok(());
            }
        }
        Err(CoreError::InvalidCommand("unknown or unloaded run".into()))
    }

    /// Posts the agent's answer into the task thread the run belongs to.
    fn post_agent_reply(
        &self,
        run_id: RunId,
        body: String,
        now: TimestampMs,
    ) -> Result<MessageId, CoreError> {
        let mut state = self.state()?;
        for snapshot in state.snapshots.values_mut() {
            let Some(session) = snapshot
                .runs
                .iter()
                .find(|run| run.id == run_id)
                .and_then(|run| snapshot.sessions.iter().find(|s| s.id == run.session_id))
            else {
                continue;
            };
            let thread = snapshot
                .threads
                .iter()
                .find(|thread| thread.id == session.thread_id)
                .ok_or_else(|| CoreError::InvalidCommand("session has no thread".into()))?;
            let message = Message {
                id: MessageId::new(),
                channel_id: thread.channel_id,
                thread_id: Some(thread.id),
                author: MessageAuthor::Agent(session.agent_id),
                body,
                created_at: now,
            };
            self.store.insert_message(&message, &[])?;
            snapshot.messages.push(message.clone());
            return Ok(message.id);
        }
        Err(CoreError::InvalidCommand("unknown or unloaded run".into()))
    }

    fn set_provider_session_id(
        &self,
        run_id: RunId,
        provider_session_id: &str,
    ) -> Result<SessionId, CoreError> {
        let mut state = self.state()?;
        for snapshot in state.snapshots.values_mut() {
            let Some(session_id) = snapshot
                .runs
                .iter()
                .find(|run| run.id == run_id)
                .map(|run| run.session_id)
            else {
                continue;
            };
            if let Some(session) = snapshot
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
            {
                session.provider_session_id = Some(provider_session_id.to_owned());
                return Ok(session_id);
            }
        }
        Err(CoreError::InvalidCommand("unknown or unloaded run".into()))
    }
}

struct CreatedThread {
    thread_id: ThreadId,
    session_id: SessionId,
    run_id: RunId,
    events: Vec<AppEvent>,
}

fn unique_agent_targets(targets: &[AttachmentTarget]) -> Vec<AgentId> {
    let mut seen = HashSet::new();
    targets
        .iter()
        .filter_map(|target| match target {
            AttachmentTarget::Agent { agent_id } if seen.insert(*agent_id) => Some(*agent_id),
            _ => None,
        })
        .collect()
}

fn thread_title(body: &str, agent_name: &str) -> String {
    let title: String = body
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect();
    if title.is_empty() {
        format!("Task for {agent_name}")
    } else {
        title
    }
}

fn raw_payload(event: &RunnerEvent) -> String {
    match event {
        RunnerEvent::Started {
            provider_session_id,
        } => format!("started:{}", provider_session_id.as_deref().unwrap_or("")),
        RunnerEvent::Output(text) => text.clone(),
        RunnerEvent::Reply(text) => format!("reply:{text}"),
        RunnerEvent::WaitingForInput(text) => format!("waiting_for_input:{text}"),
        RunnerEvent::Completed => "completed".into(),
        RunnerEvent::Failed(message) => format!("failed:{message}"),
        RunnerEvent::Cancelled => "cancelled".into(),
    }
}

#[cfg(test)]
mod tests;
