use gpui::{
    Animation, AnimationExt, AnyElement, Context, Entity, Focusable, FontWeight, Hsla, IntoElement,
    MouseButton, PathPromptOptions, Render, ScrollHandle, SharedString, Subscription, Window, div,
    prelude::*, pulsating_between, px,
};
use std::{sync::Arc, time::Duration};
use toma_core::TomaCore;
use toma_domain::*;
use toma_storage::WorkspaceSnapshot;

use crate::Theme;
use crate::composer::{Composer, ComposerEvent, MentionCandidate};
use crate::markdown::render_markdown;
use crate::view_model::{ContextKey, ShellViewModel};

/// Height of the unified title bar strip; the traffic lights sit inside it.
const TITLEBAR: f32 = 52.;

pub struct TomaShell {
    model: ShellViewModel,
    backend: Option<(Arc<TomaCore>, WorkspaceId)>,
    channel_composer: Entity<Composer>,
    thread_composer: Entity<Composer>,
    channel_scroll: ScrollHandle,
    thread_scroll: ScrollHandle,
    seen_messages: usize,
    _subscriptions: Vec<Subscription>,
}

impl TomaShell {
    pub fn new(
        snapshot: WorkspaceSnapshot,
        backend: Option<(Arc<TomaCore>, WorkspaceId)>,
        window: &mut Window,
        cx: &mut gpui::App,
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
        let Some((core, _)) = self.backend.clone() else {
            return;
        };
        let attachments = self
            .model
            .snapshot
            .agents
            .iter()
            .filter(|agent| agent.enabled && body.contains(&format!("@{}", agent.name)))
            .map(|agent| AttachmentTarget::Agent { agent_id: agent.id })
            .collect();
        let command = AppCommand::PostMessage {
            channel_id: key.channel_id,
            thread_id: key.thread_id,
            body: body.to_owned(),
            attachments,
        };
        // Agent runs block until the CLI exits; progress arrives through the revision poll.
        std::thread::spawn(move || {
            if let Err(error) = core.dispatch(command) {
                eprintln!("toma: posting message failed: {error}");
            }
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
        cx.notify();
    }

    fn close_thread(&mut self, cx: &mut Context<Self>) {
        self.model.close_thread();
        cx.notify();
    }

    fn sidebar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_channel = self.model.selected_channel_id();
        let open_thread = self.model.open_thread_id();
        let channels = self.model.snapshot.channels.clone();
        let threads: Vec<_> = self
            .model
            .recent_threads()
            .into_iter()
            .map(|thread| (thread.id, thread.title.clone(), thread.status))
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
                                .text_size(px(15.))
                                .text_color(theme.text_secondary)
                                .cursor_pointer()
                                .hover(|button| button.bg(theme.hover))
                                .tooltip(|_window, cx| {
                                    cx.new(|_| Tooltip("New channel from a folder…".into()))
                                        .into()
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.new_channel(cx)))
                                .child("+"),
                        ),
                    )
                    .children(channels.into_iter().enumerate().map(|(index, channel)| {
                        let id = channel.id;
                        let selected = selected_channel == Some(id) && open_thread.is_none();
                        sidebar_row(("channel", index), selected, theme)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.model.close_thread();
                                this.select_channel(id, cx);
                            }))
                            .child(div().w(px(16.)).text_color(theme.text_tertiary).child("#"))
                            .child(div().flex_grow().truncate().child(channel.name))
                            .children(channel.repository_path.map(|_| {
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.text_tertiary)
                                    .child("folder")
                            }))
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
                        |(index, (id, title, status))| {
                            sidebar_row(("thread", index), open_thread == Some(id), theme)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.open_thread(id, cx)),
                                )
                                .child(status_dot(status, theme))
                                .child(div().flex_grow().truncate().child(title))
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
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("# {name}")),
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
                                    .child(format!("#{name}")),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_size(px(13.))
                                    .text_color(theme.text_secondary)
                                    .child(match &folder {
                                        Some(folder) => format!(
                                            "Agents mentioned here work in {folder}. Try @Claude."
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
            div()
                .mt_1()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .id(SharedString::from(format!("replies-{id}")))
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.accent)
                        .cursor_pointer()
                        .hover(|link| link.underline())
                        .on_click(cx.listener(move |this, _, _, cx| this.open_thread(id, cx)))
                        .child(match replies {
                            0 => "Open thread".to_owned(),
                            1 => "1 reply".to_owned(),
                            n => format!("{n} replies"),
                        }),
                )
                .children(
                    (!working.is_empty())
                        .then(|| typing_indicator(&working, &format!("typing-{id}"), theme)),
                )
        });

        div()
            .flex()
            .gap_3()
            .py_2()
            .child(avatar(&author, message.author, theme))
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

        Some(
            div()
                .w(px(400.))
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
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_size(px(11.))
                                        .text_color(theme.text_secondary)
                                        .child(status_dot(thread.status, theme))
                                        .child(status_label(thread.status)),
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
                                .text_size(px(12.))
                                .text_color(theme.text_secondary)
                                .cursor_pointer()
                                .hover(|button| button.bg(theme.hover))
                                .on_click(cx.listener(|this, _, _, cx| this.close_thread(cx)))
                                .child("✕"),
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
                        .child(self.thread_composer.clone()),
                ),
        )
    }

    fn thread_message(&self, message: Message, theme: &Theme) -> impl IntoElement {
        let author = self.model.author_name(message.author).to_owned();
        div()
            .flex()
            .gap_2()
            .py_2()
            .child(avatar(&author, message.author, theme))
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx);
        let messages = self.model.snapshot.messages.len();
        if messages != self.seen_messages {
            self.seen_messages = messages;
            self.channel_scroll.scroll_to_bottom();
            self.thread_scroll.scroll_to_bottom();
        }
        div()
            .size_full()
            .flex()
            .font_family(".SystemUIFont")
            .text_size(px(13.))
            .text_color(theme.text)
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
        .text_color(theme.text_tertiary)
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

fn avatar(name: &str, author: MessageAuthor, theme: &Theme) -> impl IntoElement {
    let color: Hsla = match author {
        MessageAuthor::Agent(_) => theme.accent,
        MessageAuthor::Person(_) => theme.green,
        MessageAuthor::System => theme.gray,
    };
    div()
        .mt(px(2.))
        .size(px(28.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .bg(color)
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(gpui::white())
        .child(
            name.chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string(),
        )
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
    let color = match status {
        WorkStatus::Queued | WorkStatus::Cancelled => theme.gray,
        WorkStatus::Reading | WorkStatus::Working => theme.accent,
        WorkStatus::WaitingForInput | WorkStatus::Blocked => theme.orange,
        WorkStatus::Completed => theme.green,
        WorkStatus::Failed => theme.red,
    };
    div().size(px(7.)).flex_shrink_0().rounded_full().bg(color)
}

fn status_label(status: WorkStatus) -> &'static str {
    match status {
        WorkStatus::Queued => "Queued",
        WorkStatus::Reading => "Reading",
        WorkStatus::Working => "Working",
        WorkStatus::WaitingForInput => "Waiting for you",
        WorkStatus::Blocked => "Blocked",
        WorkStatus::Completed => "Done",
        WorkStatus::Failed => "Failed",
        WorkStatus::Cancelled => "Cancelled",
    }
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
