use gpui::{
    Animation, AnimationExt, AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight,
    Hsla, IntoElement, MouseButton, MouseMoveEvent, PathPromptOptions, Render, ScrollHandle,
    SharedString, Subscription, Window, div, prelude::*, pulsating_between, rgb,
};

use crate::zoom::px;
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::Duration,
};
use toma_core::TomaCore;
use toma_domain::*;
use toma_storage::WorkspaceSnapshot;

use crate::Theme;
use crate::composer::{Composer, ComposerEvent, MentionCandidate};
use crate::controls::{ButtonStyle, Cancel, OnPress, button, focus_navigation};
use crate::icons::Icon;
use crate::markdown::render_markdown;
use crate::view_model::{ContextKey, ShellViewModel, ThreadStats};

/// Height of the unified title bar strip; the traffic lights sit inside it.
const TITLEBAR: f32 = 52.;
const THREAD_WIDTH: (f32, f32) = (300., 900.);

pub struct TomaShell {
    model: ShellViewModel,
    backend: Option<(Arc<TomaCore>, WorkspaceId)>,
    channel_composer: Entity<Composer>,
    thread_composer: Entity<Composer>,
    channel_scroll: ScrollHandle,
    thread_scroll: ScrollHandle,
    seen_messages: usize,
    thread_width: f32,
    resizing_thread: bool,
    /// Focus to move to on the next frame, once the target element exists.
    pending_focus: Option<FocusHandle>,
    /// Deny, Allow, Always allow, Switch to Auto buttons of each pending permission request.
    request_buttons: HashMap<String, [FocusHandle; 4]>,
    /// Requests that already took focus once, so they don't grab it again.
    focused_requests: HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

impl TomaShell {
    pub fn new(
        snapshot: WorkspaceSnapshot,
        backend: Option<(Arc<TomaCore>, WorkspaceId)>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let model = ShellViewModel::new(snapshot);
        let mentions: Vec<_> = model
            .snapshot
            .agents
            .iter()
            .filter(|agent| agent.enabled)
            .map(|agent| MentionCandidate {
                id: agent.id,
                name: agent.name.clone(),
                role: agent.role.clone(),
            })
            .collect();
        let channel_draft = model.draft().to_owned();
        let channel_composer = cx.new(|cx| {
            Composer::new(
                channel_draft,
                mentions.clone(),
                "Message the channel, or @mention an agent",
                cx,
            )
        });
        let thread_composer =
            cx.new(|cx| Composer::new(String::new(), mentions, "Reply in thread", cx));
        window.focus(&channel_composer.focus_handle(cx));

        cx.new(|cx| {
            let subscriptions = vec![
                cx.subscribe(&channel_composer, |shell: &mut Self, _, event, cx| {
                    let key = shell.channel_key();
                    shell.composer_event(key, event, cx);
                }),
                cx.subscribe(&thread_composer, |shell: &mut Self, _, event, cx| {
                    let key = shell.thread_key();
                    shell.composer_event(key, event, cx);
                }),
                cx.observe_window_appearance(window, |_, window, cx| {
                    cx.set_global(Theme::for_appearance(window.appearance()));
                    cx.notify();
                }),
            ];
            if let Some((core, workspace_id)) = backend.clone() {
                // ponytail: polls a revision counter and clones the whole cached snapshot on
                // change; push AppEvents over a channel if histories get large.
                cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
                    let mut seen = u64::MAX;
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(60))
                            .await;
                        let revision = core.revision();
                        if revision == seen {
                            continue;
                        }
                        seen = revision;
                        let Some(snapshot) = core.cached_snapshot(workspace_id) else {
                            continue;
                        };
                        let updated = this.update(cx, |shell, cx| {
                            shell.model.replace_snapshot(snapshot);
                            cx.notify();
                        });
                        if updated.is_err() {
                            break;
                        }
                    }
                })
                .detach();
            }
            Self {
                seen_messages: model.snapshot.messages.len(),
                model,
                backend,
                channel_composer,
                thread_composer,
                channel_scroll: ScrollHandle::new(),
                thread_scroll: ScrollHandle::new(),
                thread_width: 400.,
                resizing_thread: false,
                pending_focus: None,
                request_buttons: HashMap::new(),
                focused_requests: HashSet::new(),
                _subscriptions: subscriptions,
            }
        })
    }

    fn channel_key(&self) -> Option<ContextKey> {
        self.model
            .selected_channel_id()
            .map(|channel_id| ContextKey {
                channel_id,
                thread_id: None,
            })
    }

    fn thread_key(&self) -> Option<ContextKey> {
        let thread_id = self.model.open_thread_id()?;
        self.channel_key().map(|key| ContextKey {
            thread_id: Some(thread_id),
            ..key
        })
    }

    fn composer_event(
        &mut self,
        key: Option<ContextKey>,
        event: &ComposerEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = key else { return };
        match event {
            ComposerEvent::Changed(body) => {
                self.model.set_draft_for(key, body.clone());
                self.save_draft(key, body);
            }
            ComposerEvent::Submitted(body) => self.submit(key, body),
        }
        cx.notify();
    }

    fn save_draft(&self, key: ContextKey, body: &str) {
        let Some((core, _)) = &self.backend else {
            return;
        };
        let draft = Draft {
            channel_id: key.channel_id,
            thread_id: key.thread_id,
            body: body.to_owned(),
            updated_at: 0,
        };
        if let Err(error) = core.dispatch(AppCommand::SaveDraft { draft }) {
            eprintln!("toma: saving draft failed: {error}");
        }
    }

    fn submit(&mut self, key: ContextKey, body: &str) {
        let attachments = self
            .model
            .snapshot
            .agents
            .iter()
            .filter(|agent| agent.enabled && body.contains(&format!("@{}", agent.name)))
            .map(|agent| AttachmentTarget::Agent { agent_id: agent.id })
            .collect();
        self.dispatch_in_background(AppCommand::PostMessage {
            channel_id: key.channel_id,
            thread_id: key.thread_id,
            body: body.to_owned(),
            attachments,
        });
    }

    /// Commands that may run an agent block until its CLI exits, so they run off the UI
    /// thread; their progress arrives through the revision poll.
    fn dispatch_in_background(&self, command: AppCommand) {
        let Some((core, _)) = self.backend.clone() else {
            return;
        };
        std::thread::spawn(move || {
            if let Err(error) = core.dispatch(command) {
                eprintln!("toma: command failed: {error}");
            }
        });
    }

    fn answer_permission(
        &mut self,
        request: &PermissionRequest,
        decision: PermissionDecision,
        cx: &mut App,
    ) {
        // The card is about to disappear; hand focus back to where the person replies.
        self.pending_focus = Some(self.thread_composer.focus_handle(cx));
        self.dispatch_in_background(AppCommand::AnswerPermission {
            run_id: request.run_id,
            request_id: request.id.clone(),
            decision,
        });
    }

    fn new_channel(&mut self, cx: &mut Context<Self>) {
        let Some((core, workspace_id)) = self.backend.clone() else {
            return;
        };
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Create Channel".into()),
        });
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(repository_path) = paths.into_iter().next() else {
                return;
            };
            match core.dispatch(AppCommand::CreateChannel {
                workspace_id,
                repository_path,
            }) {
                Ok(events) => {
                    let snapshot = core.cached_snapshot(workspace_id);
                    this.update(cx, |shell, cx| {
                        if let Some(snapshot) = snapshot {
                            shell.model.replace_snapshot(snapshot);
                        }
                        if let Some(AppEvent::ChannelCreated { channel_id }) = events.first() {
                            shell.select_channel(*channel_id, cx);
                        }
                    })
                    .ok();
                }
                Err(error) => eprintln!("toma: creating channel failed: {error}"),
            }
        })
        .detach();
    }

    fn select_channel(&mut self, channel_id: ChannelId, cx: &mut Context<Self>) {
        self.model.select_channel(channel_id);
        let draft = self.model.draft().to_owned();
        self.channel_composer
            .update(cx, |composer, cx| composer.set_text(draft, cx));
        self.channel_scroll.scroll_to_bottom();
        self.pending_focus = Some(self.channel_composer.focus_handle(cx));
        cx.notify();
    }

    fn open_thread(&mut self, thread_id: ThreadId, cx: &mut Context<Self>) {
        let previous_channel = self.model.selected_channel_id();
        self.model.open_thread(thread_id);
        if self.model.selected_channel_id() != previous_channel {
            let draft = self
                .channel_key()
                .map(|key| self.model.draft_for(key).to_owned())
                .unwrap_or_default();
            self.channel_composer
                .update(cx, |composer, cx| composer.set_text(draft, cx));
            self.channel_scroll.scroll_to_bottom();
        }
        let draft = self.model.draft().to_owned();
        self.thread_composer
            .update(cx, |composer, cx| composer.set_text(draft, cx));
        self.thread_scroll.scroll_to_bottom();
        self.pending_focus = Some(self.thread_composer.focus_handle(cx));
        cx.notify();
    }

    fn close_thread(&mut self, cx: &mut Context<Self>) {
        self.model.close_thread();
        self.pending_focus = Some(self.channel_composer.focus_handle(cx));
        cx.notify();
    }

    /// A round badge: the provider's logo for agents, an initial for people.
    fn avatar(&self, author: MessageAuthor, size: f32, theme: &Theme) -> AnyElement {
        let provider = match author {
            MessageAuthor::Agent(id) => self
                .model
                .snapshot
                .agents
                .iter()
                .find(|agent| agent.id == id)
                .map(|agent| agent.provider),
            _ => None,
        };
        let (background, logo): (Hsla, _) = match (author, provider) {
            (_, Some(RunnerProvider::ClaudeCodeCli)) => (rgb(0xd97757).into(), Some(Icon::Claude)),
            (_, Some(RunnerProvider::CursorCli)) => (rgb(0x14120b).into(), Some(Icon::Cursor)),
            (_, Some(RunnerProvider::CodexCli)) => (rgb(0x0d0d0d).into(), Some(Icon::OpenAi)),
            (MessageAuthor::Person(_), _) => (theme.accent, None),
            _ => (theme.gray, None),
        };
        let badge = div()
            .size(px(size))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(background);
        match logo {
            Some(logo) => badge
                .child(logo.view(gpui::white()).size(px(size * 0.6)))
                .into_any_element(),
            None => badge
                .text_size(px(size * 0.45))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(gpui::white())
                .child(initial(self.model.author_name(author)))
                .into_any_element(),
        }
    }

    /// Overlapping badges, like the viewers in a shared document.
    fn avatar_stack(&self, authors: &[MessageAuthor], size: f32, theme: &Theme) -> AnyElement {
        div()
            .flex()
            .flex_shrink_0()
            .children(authors.iter().enumerate().map(|(index, author)| {
                div()
                    .when(index > 0, |badge| badge.ml(px(-size * 0.3)))
                    .rounded_full()
                    .border_2()
                    .border_color(theme.content_bg)
                    .child(self.avatar(*author, size, theme))
            }))
            .into_any_element()
    }

    /// Gives each visible permission request its buttons, and focuses Allow the first time
    /// the request is shown, unless the person is in the middle of typing a reply.
    fn prepare_request_focus(&mut self, window: &Window, cx: &mut Context<Self>) {
        let live: HashSet<String> = self
            .model
            .snapshot
            .permission_requests
            .iter()
            .map(|request| request.id.clone())
            .collect();
        self.request_buttons.retain(|id, _| live.contains(id));
        self.focused_requests.retain(|id| live.contains(id));
        let Some(thread_id) = self.model.open_thread_id() else {
            return;
        };
        let visible: Vec<String> = self
            .model
            .snapshot
            .permission_requests
            .iter()
            .filter(|request| request.thread_id == thread_id)
            .map(|request| request.id.clone())
            .collect();
        let typing = self.thread_composer.focus_handle(cx).is_focused(window)
            && self
                .thread_key()
                .is_some_and(|key| !self.model.draft_for(key).is_empty());
        for id in visible {
            let allow = self.request_buttons.entry(id.clone()).or_insert_with(|| {
                [1, 2, 3, 4].map(|index| cx.focus_handle().tab_stop(true).tab_index(index))
            })[1]
                .clone();
            if !typing && self.focused_requests.insert(id) {
                self.pending_focus = Some(allow);
            }
        }
    }

    fn sidebar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_channel = self.model.selected_channel_id();
        let open_thread = self.model.open_thread_id();
        let channels = self.model.snapshot.channels.clone();
        let threads: Vec<_> = self
            .model
            .recent_threads()
            .into_iter()
            .map(|thread| {
                let agents: Vec<_> = self
                    .model
                    .thread_agents(thread.id)
                    .into_iter()
                    .map(|(agent, _)| MessageAuthor::Agent(agent.id))
                    .collect();
                (
                    thread.id,
                    thread.title.clone(),
                    thread.status,
                    self.avatar_stack(&agents, 16., theme),
                )
            })
            .collect();

        div()
            .w(px(240.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(theme.sidebar_bg)
            .border_r_1()
            .border_color(theme.separator)
            .child(titlebar_drag_area().h(px(TITLEBAR)).flex_shrink_0())
            .child(
                div()
                    .id("sidebar")
                    .flex_grow()
                    .overflow_y_scroll()
                    .px_2()
                    .pb_3()
                    .child(
                        section_header("Channels", theme).child(
                            div()
                                .id("new-channel")
                                .size(px(20.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(5.))
                                .text_color(theme.text_secondary)
                                .cursor_pointer()
                                .hover(|button| button.bg(theme.hover))
                                .tooltip(|_window, cx| {
                                    cx.new(|_| Tooltip("New channel from a folder…".into()))
                                        .into()
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.new_channel(cx)))
                                .child(Icon::Plus.view(theme.text_secondary).size(px(14.))),
                        ),
                    )
                    .children(channels.into_iter().enumerate().map(|(index, channel)| {
                        let id = channel.id;
                        let selected = selected_channel == Some(id) && open_thread.is_none();
                        let icon = if channel.repository_path.is_some() {
                            Icon::Folder
                        } else {
                            Icon::Hash
                        };
                        sidebar_row(("channel", index), selected, theme)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.model.close_thread();
                                this.select_channel(id, cx);
                            }))
                            .child(icon.view(theme.text_secondary).size(px(14.)))
                            .child(div().flex_grow().truncate().child(channel.name))
                    }))
                    .child(div().h(px(14.)))
                    .child(section_header("Threads", theme))
                    .children(threads.is_empty().then(|| {
                        div()
                            .px_2()
                            .py_1()
                            .text_size(px(12.))
                            .text_color(theme.text_tertiary)
                            .child("Mention an agent to start one")
                    }))
                    .children(threads.into_iter().enumerate().map(
                        |(index, (id, title, status, agents))| {
                            sidebar_row(("thread", index), open_thread == Some(id), theme)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.open_thread(id, cx)),
                                )
                                .child(status_dot(status, theme))
                                .child(div().flex_grow().min_w(px(0.)).truncate().child(title))
                                .child(agents)
                        },
                    )),
            )
    }

    fn conversation(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let channel = self.model.selected_channel().cloned();
        let messages: Vec<_> = self.model.messages_for(None).cloned().collect();
        let rows: Vec<_> = messages
            .into_iter()
            .map(|message| self.message_row(message, theme, cx))
            .collect();
        let empty = rows.is_empty();
        let name = channel.as_ref().map_or("".into(), |c| c.name.clone());
        let is_folder = channel
            .as_ref()
            .is_some_and(|c| c.repository_path.is_some());
        let folder = channel
            .as_ref()
            .and_then(|c| c.repository_path.clone())
            .or_else(|| {
                self.model
                    .snapshot
                    .workspace
                    .as_ref()
                    .map(|w| w.repository_path.clone())
            })
            .map(|path| path.display().to_string());

        div()
            .min_w(px(360.))
            .flex_grow()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.content_bg)
            .child(
                titlebar_drag_area()
                    .h(px(TITLEBAR))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .px_5()
                    .border_b_1()
                    .border_color(theme.separator)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                if is_folder { Icon::Folder } else { Icon::Hash }
                                    .view(theme.text_secondary)
                                    .size(px(14.)),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(name.clone()),
                            ),
                    )
                    .children(folder.clone().map(|folder| {
                        div()
                            .text_size(px(11.))
                            .text_color(theme.text_secondary)
                            .truncate()
                            .child(folder)
                    })),
            )
            .child(
                div()
                    .id("messages")
                    .flex_grow()
                    .overflow_y_scroll()
                    .track_scroll(&self.channel_scroll)
                    .px_5()
                    .py_3()
                    .children(empty.then(|| {
                        div()
                            .pt(px(24.))
                            .child(
                                div()
                                    .text_size(px(20.))
                                    .font_weight(FontWeight::BOLD)
                                    .child(format!("Welcome to #{name}")),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_size(px(13.))
                                    .text_color(theme.text_secondary)
                                    .child(match &folder {
                                        Some(folder) => format!(
                                            "Agents mentioned here work in {folder}. Try @Claude or @Cursor."
                                        ),
                                        None => "Mention an agent to start a task.".into(),
                                    }),
                            )
                    }))
                    .children(rows),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .px_5()
                    .pb_4()
                    .pt_1()
                    .child(self.channel_composer.clone()),
            )
    }

    fn message_row(&self, message: Message, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let author = self.model.author_name(message.author).to_owned();
        let thread = self
            .model
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.root_message_id == message.id)
            .cloned();
        let footer = thread.map(|thread| {
            let id = thread.id;
            let replies = self
                .model
                .snapshot
                .messages
                .iter()
                .filter(|message| message.thread_id == Some(id))
                .count();
            let working = self.model.working_agents(id);
            let agents: Vec<_> = self
                .model
                .thread_agents(id)
                .into_iter()
                .map(|(agent, _)| MessageAuthor::Agent(agent.id))
                .collect();
            let agents = self.avatar_stack(&agents, 18., theme);
            let stats = self.model.thread_stats(id);
            let needs_approval = self
                .model
                .snapshot
                .permission_requests
                .iter()
                .any(|request| request.thread_id == id);
            div()
                .mt_1()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .id(SharedString::from(format!("replies-{id}")))
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.accent)
                        .cursor_pointer()
                        .hover(|link| link.underline())
                        .on_click(cx.listener(move |this, _, _, cx| this.open_thread(id, cx)))
                        .child(agents)
                        .child(Icon::Replies.view(theme.accent).size(px(13.)))
                        .child(match replies {
                            0 => "Open thread".to_owned(),
                            1 => "1 reply".to_owned(),
                            n => format!("{n} replies"),
                        }),
                )
                .children(needs_approval.then(|| {
                    div()
                        .id(SharedString::from(format!("approve-{id}")))
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.orange)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.open_thread(id, cx)))
                        .child(Icon::Shield.view(theme.orange).size(px(13.)))
                        .child("Needs your approval")
                }))
                .children(
                    (!working.is_empty())
                        .then(|| typing_indicator(&working, &format!("typing-{id}"), theme)),
                )
                .children(stats_row(stats, theme))
        });

        div()
            .flex()
            .gap_3()
            .py_2()
            .child(
                div()
                    .mt(px(2.))
                    .child(self.avatar(message.author, 28., theme)),
            )
            .child(
                div()
                    .min_w(px(0.))
                    .flex_grow()
                    .child(message_header(&author, message.created_at, theme))
                    .child(render_markdown(
                        SharedString::from(format!("m-{}", message.id)),
                        &message.body,
                        theme,
                    ))
                    .children(footer),
            )
            .into_any_element()
    }

    fn thread_pane(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let thread_id = self.model.open_thread_id()?;
        let thread = self
            .model
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)?
            .clone();
        let root = self
            .model
            .snapshot
            .messages
            .iter()
            .find(|message| message.id == thread.root_message_id)
            .cloned();
        let replies: Vec<_> = self.model.messages_for(Some(thread_id)).cloned().collect();
        let reply_count = replies.len();
        let working = self.model.working_agents(thread_id);
        let stats = self.model.thread_stats(thread_id);
        let mode = thread.permission_mode;
        let requests: Vec<_> = self
            .model
            .snapshot
            .permission_requests
            .iter()
            .filter(|request| request.thread_id == thread_id)
            .cloned()
            .collect();
        let cards: Vec<_> = requests
            .into_iter()
            .filter_map(|request| {
                let buttons = self.request_buttons.get(&request.id)?.clone();
                Some(
                    self.permission_card(request, buttons, theme, cx)
                        .into_any_element(),
                )
            })
            .collect();

        Some(
            div()
                .relative()
                .w(px(self.thread_width))
                .h_full()
                .flex_shrink_0()
                .flex()
                .flex_col()
                .bg(theme.content_bg)
                .border_l_1()
                .border_color(theme.separator)
                .child(
                    titlebar_drag_area()
                        .h(px(TITLEBAR))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .pl_4()
                        .pr_2()
                        .border_b_1()
                        .border_color(theme.separator)
                        .child(
                            div()
                                .min_w(px(0.))
                                .flex_grow()
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child(thread.title),
                                )
                                // One line that clips at the pane edge instead of wrapping
                                // into the title.
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_size(px(11.))
                                        .text_color(theme.text_secondary)
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .child(status_badge(thread.status, theme)),
                                        )
                                        .children(stats_row(stats, theme)),
                                ),
                        )
                        .child(
                            div()
                                .id("close-thread")
                                .size(px(24.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.))
                                .text_color(theme.text_secondary)
                                .cursor_pointer()
                                .hover(|button| button.bg(theme.hover))
                                .on_click(cx.listener(|this, _, _, cx| this.close_thread(cx)))
                                .child(Icon::Close.view(theme.text_secondary).size(px(14.))),
                        ),
                )
                .child(
                    div()
                        .id("thread-messages")
                        .flex_grow()
                        .overflow_y_scroll()
                        .track_scroll(&self.thread_scroll)
                        .px_4()
                        .py_3()
                        .children(root.map(|message| self.thread_message(message, theme)))
                        .child(
                            div()
                                .my_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(11.))
                                .text_color(theme.text_tertiary)
                                .child(match reply_count {
                                    1 => "1 reply".to_owned(),
                                    n => format!("{n} replies"),
                                })
                                .child(div().flex_grow().h(px(1.)).bg(theme.separator)),
                        )
                        .children(
                            replies
                                .into_iter()
                                .map(|message| self.thread_message(message, theme)),
                        )
                        .children(cards)
                        .children((!working.is_empty()).then(|| {
                            div()
                                .py_2()
                                .child(typing_indicator(&working, "thread-typing", theme))
                        })),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .px_4()
                        .pb_4()
                        .pt_1()
                        .child(self.thread_composer.clone())
                        // The mode sits with the input, like Codex and Claude: it governs
                        // what the next message is allowed to do.
                        .child(
                            div()
                                .mt_2()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(self.mode_picker(thread_id, mode, theme, cx))
                                .child(
                                    div()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(px(11.))
                                        .text_color(theme.text_tertiary)
                                        .child("⏎ send · ⇧⏎ new line"),
                                ),
                        ),
                )
                // Drag handle over the divider; the root follows the drag.
                .child(
                    div()
                        .id("thread-divider")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(-3.))
                        .w(px(6.))
                        .cursor_col_resize()
                        .when(self.resizing_thread, |handle| handle.bg(theme.accent))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.resizing_thread = true;
                                cx.notify();
                            }),
                        ),
                ),
        )
    }

    /// Ask / Auto / Plan for the thread; each provider maps it to its own native mode.
    fn mode_picker(
        &self,
        thread_id: ThreadId,
        current: PermissionMode,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let modes = [
            (
                PermissionMode::Ask,
                Icon::Hand,
                "Ask before commands and edits outside the folder",
            ),
            (
                PermissionMode::Auto,
                Icon::Zap,
                "The agent's own reviewer answers for you",
            ),
            (
                PermissionMode::Plan,
                Icon::Plan,
                "Read-only: plan and explain, no edits",
            ),
        ];
        div()
            .flex_shrink_0()
            .flex()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(7.))
            .bg(theme.selection)
            .children(modes.into_iter().map(|(mode, icon, tip)| {
                let selected = mode == current;
                let label = format!("{mode:?}");
                div()
                    .id(SharedString::from(format!("mode-{label}")))
                    .px_2()
                    .py(px(1.))
                    .rounded(px(5.))
                    .text_size(px(11.))
                    .cursor_pointer()
                    .when(selected, |chip| {
                        chip.bg(theme.content_bg)
                            .shadow_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                    })
                    .when(!selected, |chip| {
                        chip.text_color(theme.text_secondary)
                            .hover(|chip| chip.text_color(theme.text))
                    })
                    .tooltip(move |_window, cx| cx.new(|_| Tooltip(tip.into())).into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dispatch_in_background(AppCommand::SetPermissionMode {
                            thread_id,
                            mode,
                        });
                        cx.notify();
                    }))
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(icon.view(if selected { theme.text } else { theme.text_secondary })
                        .size(px(12.)))
                    .child(label)
            }))
    }

    fn permission_card(
        &self,
        request: PermissionRequest,
        [deny_focus, allow_focus, always_focus, auto_focus]: [FocusHandle; 4],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let agent = self
            .model
            .author_name(MessageAuthor::Agent(request.agent_id))
            .to_owned();
        let entity = cx.entity();
        let answer = |decision: PermissionDecision| -> OnPress {
            let entity = entity.clone();
            let request = request.clone();
            Rc::new(move |_, cx| {
                entity.update(cx, |shell, cx| {
                    shell.answer_permission(&request, decision, cx);
                    cx.notify();
                })
            })
        };
        let deny = answer(PermissionDecision::Deny);
        let escape = Rc::clone(&deny);
        div()
            .key_context("Dialog")
            .on_action(move |_: &Cancel, window, cx| escape(window, cx))
            .my_2()
            .p_3()
            .rounded(px(8.))
            .border_1()
            .border_color(theme.orange)
            .bg(theme.control_bg)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::Shield.view(theme.orange).size(px(15.)))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{agent} wants to use {}", request.tool)),
                    ),
            )
            .child(
                div()
                    .mt_2()
                    .px_2()
                    .py_1()
                    .rounded(px(5.))
                    .bg(theme.code_bg)
                    .font_family("Menlo")
                    .text_size(px(12.))
                    .child(request.detail.clone()),
            )
            .child(
                div()
                    .mt_3()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(button(
                        SharedString::from(format!("deny-{}", request.id)),
                        "Deny",
                        &deny_focus,
                        ButtonStyle::Secondary,
                        theme,
                        deny,
                    ))
                    .child(button(
                        SharedString::from(format!("allow-{}", request.id)),
                        "Allow once",
                        &allow_focus,
                        ButtonStyle::Primary,
                        theme,
                        answer(PermissionDecision::Allow),
                    ))
                    .child(button(
                        SharedString::from(format!("always-{}", request.id)),
                        format!("Always allow {}", request.rule),
                        &always_focus,
                        ButtonStyle::Secondary,
                        theme,
                        answer(PermissionDecision::AlwaysAllow),
                    ))
                    .child(button(
                        SharedString::from(format!("auto-{}", request.id)),
                        "Allow & switch to Auto",
                        &auto_focus,
                        ButtonStyle::Secondary,
                        theme,
                        answer(PermissionDecision::SwitchToAuto),
                    )),
            )
            .child(
                div()
                    .mt_2()
                    .text_size(px(11.))
                    .text_color(theme.text_tertiary)
                    .child("⏎ choose · ⇥ next · esc deny"),
            )
    }

    fn thread_message(&self, message: Message, theme: &Theme) -> impl IntoElement {
        let author = self.model.author_name(message.author).to_owned();
        div()
            .flex()
            .gap_2()
            .py_2()
            .child(
                div()
                    .mt(px(2.))
                    .child(self.avatar(message.author, 28., theme)),
            )
            .child(
                div()
                    .min_w(px(0.))
                    .flex_grow()
                    .child(message_header(&author, message.created_at, theme))
                    .child(render_markdown(
                        SharedString::from(format!("t-{}", message.id)),
                        &message.body,
                        theme,
                    )),
            )
    }
}

impl Render for TomaShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx);
        let messages = self.model.snapshot.messages.len();
        if messages != self.seen_messages {
            self.seen_messages = messages;
            self.channel_scroll.scroll_to_bottom();
            self.thread_scroll.scroll_to_bottom();
        }
        // Scales rem-based sizes (text_sm and friends) along with px().
        window.set_rem_size(gpui::px(16.) * crate::zoom::zoom());
        self.prepare_request_focus(window, cx);
        if let Some(focus) = self.pending_focus.take() {
            window.focus(&focus);
        }
        focus_navigation(div())
            .size_full()
            .flex()
            .font_family(".SystemUIFont")
            .text_size(px(13.))
            .text_color(theme.text)
            .when(self.resizing_thread, |root| root.cursor_col_resize())
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if this.resizing_thread {
                    // Stored unzoomed, since px() applies the zoom when drawing.
                    let width = (window.viewport_size().width - event.position.x)
                        / gpui::px(1.)
                        / crate::zoom::zoom();
                    this.thread_width = width.clamp(THREAD_WIDTH.0, THREAD_WIDTH.1);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.resizing_thread {
                        this.resizing_thread = false;
                        cx.notify();
                    }
                }),
            )
            .child(self.sidebar(&theme, cx))
            .child(self.conversation(&theme, cx))
            .children(self.thread_pane(&theme, cx))
    }
}

/// Empty space in the title bar strip: drags the window, double-click zooms, like AppKit.
fn titlebar_drag_area() -> gpui::Div {
    div().on_mouse_down(MouseButton::Left, |event, window, _| {
        if event.click_count == 2 {
            window.titlebar_double_click();
        } else {
            window.start_window_move();
        }
    })
}

fn section_header(label: &'static str, theme: &Theme) -> gpui::Div {
    div()
        .h(px(26.))
        .flex()
        .items_center()
        .justify_between()
        .px_2()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text_secondary)
        .child(label)
}

fn sidebar_row(
    id: impl Into<gpui::ElementId>,
    selected: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .h(px(28.))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .rounded(px(6.))
        .cursor_pointer()
        .when(selected, |row| row.bg(theme.selection))
        .when(!selected, |row| row.hover(move |row| row.bg(hover)))
}

fn initial(name: &str) -> String {
    name.chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string()
}

fn message_header(author: &str, created_at: TimestampMs, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .items_baseline()
        .gap_2()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(author.to_owned()),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme.text_tertiary)
                .child(time_label(created_at)),
        )
}

/// Time, tokens and cost across a thread's runs, omitting what providers did not report.
fn stats_row(stats: ThreadStats, theme: &Theme) -> Option<impl IntoElement> {
    if stats.runs == 0 {
        return None;
    }
    let chip = |icon: Icon, text: String| {
        div()
            .flex()
            .items_center()
            .gap(px(3.))
            .child(icon.view(theme.text_tertiary).size(px(11.)))
            .child(text)
    };
    Some(
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(11.))
            .text_color(theme.text_tertiary)
            .child(chip(Icon::Clock, format_duration(stats.duration_ms)))
            .children((stats.tokens > 0).then(|| {
                chip(
                    Icon::Tokens,
                    format!("{} tokens", format_tokens(stats.tokens)),
                )
            }))
            .children(
                stats
                    .cost_micros
                    .map(|cost| div().child(format!("${:.2}", cost as f64 / 1_000_000.))),
            ),
    )
}

fn format_duration(ms: i64) -> String {
    let seconds = ms / 1000;
    match seconds {
        ..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

fn format_tokens(tokens: i64) -> String {
    match tokens {
        ..1000 => tokens.to_string(),
        1000..1_000_000 => format!("{:.1}k", tokens as f64 / 1000.),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.),
    }
}

fn typing_indicator(agents: &[&str], id: &str, theme: &Theme) -> AnyElement {
    let who = match agents {
        [one] => format!("{one} is working"),
        [first, second] => format!("{first} and {second} are working"),
        _ => "Several agents are working".into(),
    };
    div()
        .flex()
        .items_center()
        .gap(px(3.))
        .text_size(px(12.))
        .text_color(theme.text_secondary)
        .children((0..3).map(|dot| {
            div()
                .size(px(5.))
                .rounded_full()
                .bg(theme.text_secondary)
                .with_animation(
                    SharedString::from(format!("{id}-{dot}")),
                    Animation::new(Duration::from_millis(1100))
                        .repeat()
                        .with_easing(pulsating_between(0.2, 1.0)),
                    move |dot_el, delta| {
                        // Offset each dot so the pulse travels left to right.
                        let phase = (delta + dot as f32 * 0.25) % 1.0;
                        dot_el.opacity(phase)
                    },
                )
        }))
        .child(div().ml_1().child(who))
        .into_any_element()
}

fn status_dot(status: WorkStatus, theme: &Theme) -> impl IntoElement {
    let color = status_color(status, theme);
    div().size(px(7.)).flex_shrink_0().rounded_full().bg(color)
}

fn status_color(status: WorkStatus, theme: &Theme) -> Hsla {
    match status {
        WorkStatus::Queued | WorkStatus::Cancelled => theme.gray,
        WorkStatus::Reading | WorkStatus::Working => theme.accent,
        WorkStatus::WaitingForInput | WorkStatus::Blocked => theme.orange,
        WorkStatus::Completed => theme.green,
        WorkStatus::Failed => theme.red,
    }
}

/// Status as a tinted icon and label, e.g. a green check and "Done".
fn status_badge(status: WorkStatus, theme: &Theme) -> impl IntoElement {
    let (icon, label) = match status {
        WorkStatus::Queued => (Icon::Hourglass, "Queued"),
        WorkStatus::Reading => (Icon::Eye, "Reading"),
        WorkStatus::Working => (Icon::Zap, "Working"),
        WorkStatus::WaitingForInput => (Icon::Hand, "Waiting for you"),
        WorkStatus::Blocked => (Icon::Alert, "Blocked"),
        WorkStatus::Completed => (Icon::CircleCheck, "Done"),
        WorkStatus::Failed => (Icon::CircleX, "Failed"),
        WorkStatus::Cancelled => (Icon::Ban, "Cancelled"),
    };
    div()
        .flex()
        .items_center()
        .gap(px(3.))
        .child(icon.view(status_color(status, theme)).size(px(12.)))
        .child(label)
}

/// Local wall-clock time, or the date for messages older than today.
fn time_label(timestamp: TimestampMs) -> String {
    let local = |seconds: libc::time_t| {
        // SAFETY: localtime_r only writes into the zeroed struct we own.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&seconds, &mut tm) };
        tm
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as libc::time_t);
    let then = local((timestamp / 1000) as libc::time_t);
    let today = local(now);
    if (then.tm_year, then.tm_yday) == (today.tm_year, today.tm_yday) {
        format!("{:02}:{:02}", then.tm_hour, then.tm_min)
    } else {
        format!("{}/{}", then.tm_mon + 1, then.tm_mday)
    }
}

struct Tooltip(SharedString);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx);
        div()
            .px_2()
            .py_1()
            .rounded(px(5.))
            .bg(theme.control_bg)
            .border_1()
            .border_color(theme.separator)
            .shadow_md()
            .text_size(px(11.))
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_are_formatted_compactly() {
        assert_eq!(format_duration(42_000), "42s");
        assert_eq!(format_duration(192_000), "3m 12s");
        assert_eq!(format_duration(3_900_000), "1h 5m");
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_400), "12.4k");
        assert_eq!(format_tokens(2_300_000), "2.3M");
    }
}
