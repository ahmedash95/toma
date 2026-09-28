use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
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
    /// The in-progress reply message of each streaming run; persisted once the run replies.
    streaming: HashMap<RunId, MessageId>,
}

pub struct TomaCore {
    pub store: Arc<dyn TomaStore>,
    pub worktrees: Arc<dyn WorktreeManager>,
    pub runners: Vec<Arc<dyn AgentRunner>>,
    pub clock: Arc<dyn Clock>,
    state: Mutex<CoreState>,
    revision: AtomicU64,
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
            revision: AtomicU64::new(0),
        }
    }

    /// Increases whenever cached state changes, so views can poll cheaply.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// The live state, including replies that are still streaming.
    pub fn cached_snapshot(&self, workspace_id: WorkspaceId) -> Option<WorkspaceSnapshot> {
        self.state().ok()?.snapshots.get(&workspace_id).cloned()
    }

    fn changed(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
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
        let result = self.dispatch_inner(command);
        self.changed();
        result
    }

    fn dispatch_inner(&self, command: AppCommand) -> Result<Vec<AppEvent>, CoreError> {
        match command {
            AppCommand::CreateChannel {
                workspace_id,
                repository_path,
            } => self.create_channel(workspace_id, repository_path),
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
        self.changed();

        let mut events = vec![AppEvent::MessagePosted {
            message_id: message.id,
        }];
        let mut agent_ids = unique_agent_targets(&targets);
        if let Some(thread_id) = thread_id {
            // A plain reply in a thread goes to every agent already working in it.
            let sessions: Vec<(AgentId, SessionId)> = {
                let state = self.state()?;
                let snapshot = state.snapshots.get(&workspace_id).expect("validated");
                snapshot
                    .sessions
                    .iter()
                    .filter(|session| session.thread_id == thread_id)
                    .map(|session| (session.agent_id, session.id))
                    .collect()
            };
            if agent_ids.is_empty() {
                agent_ids = sessions.iter().map(|(agent_id, _)| *agent_id).collect();
            }
            for agent_id in agent_ids {
                match sessions.iter().find(|(id, _)| *id == agent_id) {
                    Some((_, session_id)) => events.extend(self.continue_session(
                        workspace_id,
                        thread_id,
                        *session_id,
                        &message.body,
                    )?),
                    None => events.extend(self.attach_agent_with_prompt(
                        workspace_id,
                        thread_id,
                        agent_id,
                        &message.body,
                        false,
                    )?),
                }
            }
        } else if !agent_ids.is_empty() {
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

    /// Starts the next run of an existing session, which resumes the provider's conversation.
    fn continue_session(
        &self,
        workspace_id: WorkspaceId,
        thread_id: ThreadId,
        session_id: SessionId,
        prompt: &str,
    ) -> Result<Vec<AppEvent>, CoreError> {
        let sequence = {
            let state = self.state()?;
            let snapshot = state.snapshots.get(&workspace_id).expect("validated");
            snapshot
                .runs
                .iter()
                .filter(|run| run.session_id == session_id)
                .map(|run| run.sequence)
                .max()
                .unwrap_or(0)
                + 1
        };
        let run = Run {
            id: RunId::new(),
            session_id,
            sequence,
            status: WorkStatus::Queued,
            started_at: None,
            finished_at: None,
        };
        self.store.insert_run(&run)?;
        self.state()?
            .snapshots
            .get_mut(&workspace_id)
            .expect("validated")
            .runs
            .push(run.clone());
        let mut events = vec![AppEvent::RunCreated {
            run_id: run.id,
            session_id,
        }];
        self.execute_run(
            workspace_id,
            thread_id,
            session_id,
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
        let working_directory = match self.working_directory(workspace_id, thread_id) {
            Ok(path) => path,
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
        // Events are applied as they arrive so replies stream into the cached snapshot.
        let mut sequence = 0;
        let mut handled = Ok(());
        let result = runner.run(
            RunRequest {
                run_id,
                session_id,
                provider_session_id: provider_session_id.as_deref(),
                working_directory: &working_directory,
                prompt: &effective_prompt,
            },
            &mut |event| {
                sequence += 1;
                if handled.is_ok() {
                    handled = self.handle_runner_event(run_id, sequence, event, events);
                    self.changed();
                }
            },
        );
        handled?;
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
        if let RunnerEvent::ReplyDelta(text) = &event {
            // Deltas are derived from raw output lines, which are already in history.
            return self.stream_reply(run_id, text, now);
        }
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
            RunnerEvent::ReplyDelta(_) => unreachable!("handled above"),
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
        repository: Option<PathBuf>,
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

        let repository = repository.unwrap_or(workspace.repository_path);
        let root = repository.join(".toma").join("worktrees");
        let record = self.worktrees.ensure(WorktreeRequest {
            repository_path: &repository,
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
                // Mirrors the store, which moves the session and thread with their run.
                let session_id = run.session_id;
                let Some(session) = snapshot.sessions.iter_mut().find(|s| s.id == session_id)
                else {
                    return Ok(());
                };
                session.status = status;
                let thread_id = session.thread_id;
                if let Some(thread) = snapshot.threads.iter_mut().find(|t| t.id == thread_id) {
                    thread.status = status;
                    thread.updated_at = at;
                }
                return Ok(());
            }
        }
        Err(CoreError::InvalidCommand("unknown or unloaded run".into()))
    }

    /// Appends a streamed fragment to the run's in-progress reply, creating it on first text.
    fn stream_reply(&self, run_id: RunId, text: &str, now: TimestampMs) -> Result<(), CoreError> {
        let mut state = self.state()?;
        let state = &mut *state;
        if let Some(message_id) = state.streaming.get(&run_id) {
            if let Some(message) = state
                .snapshots
                .values_mut()
                .flat_map(|snapshot| snapshot.messages.iter_mut())
                .find(|message| message.id == *message_id)
            {
                message.body.push_str(text);
            }
            return Ok(());
        }
        let text = text.trim_start();
        if text.is_empty() {
            return Ok(());
        }
        for snapshot in state.snapshots.values_mut() {
            if let Some(message) = reply_message(snapshot, run_id, text.to_owned(), now)? {
                state.streaming.insert(run_id, message.id);
                snapshot.messages.push(message);
                return Ok(());
            }
        }
        Err(CoreError::InvalidCommand("unknown or unloaded run".into()))
    }

    /// Persists the agent's answer in its task thread. A streamed reply keeps the text the
    /// user watched arrive, which also covers narration between tool calls.
    fn post_agent_reply(
        &self,
        run_id: RunId,
        body: String,
        now: TimestampMs,
    ) -> Result<MessageId, CoreError> {
        let mut state = self.state()?;
        if let Some(message_id) = state.streaming.remove(&run_id) {
            let message = state
                .snapshots
                .values()
                .flat_map(|snapshot| &snapshot.messages)
                .find(|message| message.id == message_id)
                .cloned()
                .ok_or_else(|| CoreError::InvalidCommand("streamed reply vanished".into()))?;
            self.store.insert_message(&message, &[])?;
            return Ok(message_id);
        }
        for snapshot in state.snapshots.values_mut() {
            if let Some(message) = reply_message(snapshot, run_id, body.clone(), now)? {
                self.store.insert_message(&message, &[])?;
                snapshot.messages.push(message.clone());
                return Ok(message.id);
            }
        }
        Err(CoreError::InvalidCommand("unknown or unloaded run".into()))
    }

    fn create_channel(
        &self,
        workspace_id: WorkspaceId,
        repository_path: PathBuf,
    ) -> Result<Vec<AppEvent>, CoreError> {
        let channel = {
            let state = self.state()?;
            let snapshot = state
                .snapshots
                .get(&workspace_id)
                .ok_or_else(|| CoreError::InvalidCommand("unknown or unloaded workspace".into()))?;
            let base = channel_name(&repository_path);
            let name = (1..)
                .map(|n| {
                    if n == 1 {
                        base.clone()
                    } else {
                        format!("{base}-{n}")
                    }
                })
                .find(|name| snapshot.channels.iter().all(|c| &c.name != name))
                .expect("unbounded");
            Channel {
                id: ChannelId::new(),
                workspace_id,
                name,
                position: snapshot
                    .channels
                    .iter()
                    .map(|c| c.position + 1)
                    .max()
                    .unwrap_or(0),
                repository_path: Some(repository_path),
            }
        };
        self.store.insert_channel(&channel)?;
        let channel_id = channel.id;
        self.state()?
            .snapshots
            .get_mut(&workspace_id)
            .expect("validated")
            .channels
            .push(channel);
        Ok(vec![AppEvent::ChannelCreated { channel_id }])
    }

    /// Git folders get an isolated worktree per thread; plain folders are used directly.
    fn working_directory(
        &self,
        workspace_id: WorkspaceId,
        thread_id: ThreadId,
    ) -> Result<PathBuf, CoreError> {
        let folder = {
            let state = self.state()?;
            let snapshot = state.snapshots.get(&workspace_id).expect("validated");
            snapshot
                .threads
                .iter()
                .find(|thread| thread.id == thread_id)
                .and_then(|thread| snapshot.channels.iter().find(|c| c.id == thread.channel_id))
                .and_then(|channel| channel.repository_path.clone())
        };
        match folder {
            Some(folder) if !folder.join(".git").exists() => Ok(folder),
            folder => Ok(self.ensure_worktree(workspace_id, thread_id, folder)?.path),
        }
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

/// A short title from the first line of the request, without @mentions.
fn thread_title(body: &str, agent_name: &str) -> String {
    const MAX: usize = 48;
    let words: Vec<_> = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .split_whitespace()
        .filter(|word| !word.starts_with('@'))
        .collect();
    let mut title = String::new();
    for word in words {
        if !title.is_empty() && title.chars().count() + 1 + word.chars().count() > MAX {
            title.push('…');
            break;
        }
        if !title.is_empty() {
            title.push(' ');
        }
        title.push_str(word);
    }
    let title = title.trim_end_matches(['.', ',', ':', ';']);
    let mut chars = title.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => format!("Task for {agent_name}"),
    }
}

fn channel_name(folder: &Path) -> String {
    let name: String = folder
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.is_empty() {
        "folder".into()
    } else {
        name
    }
}

/// Builds the agent's reply message for `run_id` if the run belongs to `snapshot`.
fn reply_message(
    snapshot: &WorkspaceSnapshot,
    run_id: RunId,
    body: String,
    now: TimestampMs,
) -> Result<Option<Message>, CoreError> {
    let Some(session) = snapshot
        .runs
        .iter()
        .find(|run| run.id == run_id)
        .and_then(|run| snapshot.sessions.iter().find(|s| s.id == run.session_id))
    else {
        return Ok(None);
    };
    let thread = snapshot
        .threads
        .iter()
        .find(|thread| thread.id == session.thread_id)
        .ok_or_else(|| CoreError::InvalidCommand("session has no thread".into()))?;
    Ok(Some(Message {
        id: MessageId::new(),
        channel_id: thread.channel_id,
        thread_id: Some(thread.id),
        author: MessageAuthor::Agent(session.agent_id),
        body,
        created_at: now,
    }))
}

fn raw_payload(event: &RunnerEvent) -> String {
    match event {
        RunnerEvent::Started {
            provider_session_id,
        } => format!("started:{}", provider_session_id.as_deref().unwrap_or("")),
        RunnerEvent::Output(text) => text.clone(),
        RunnerEvent::Reply(text) => format!("reply:{text}"),
        RunnerEvent::ReplyDelta(text) => text.clone(),
        RunnerEvent::WaitingForInput(text) => format!("waiting_for_input:{text}"),
        RunnerEvent::Completed => "completed".into(),
        RunnerEvent::Failed(message) => format!("failed:{message}"),
        RunnerEvent::Cancelled => "cancelled".into(),
    }
}

#[cfg(test)]
mod tests;
