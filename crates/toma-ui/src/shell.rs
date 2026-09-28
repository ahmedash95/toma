use gpui::{
    Context, Entity, IntoElement, MouseButton, Render, Subscription, Window, div, prelude::*, px,
    rgb,
};
use std::{sync::Arc, time::Duration};
use toma_core::TomaCore;
use toma_domain::*;
use toma_storage::WorkspaceSnapshot;

use crate::composer::{Composer, ComposerEvent, MentionCandidate};
use crate::view_model::ShellViewModel;

pub struct TomaShell {
    model: ShellViewModel,
    backend: Option<(Arc<TomaCore>, WorkspaceId)>,
    composer: Entity<Composer>,
    _subscriptions: Vec<Subscription>,
}

impl TomaShell {
    pub fn new(
        snapshot: WorkspaceSnapshot,
        backend: Option<(Arc<TomaCore>, WorkspaceId)>,
        _window: &mut Window,
        cx: &mut gpui::App,
    ) -> Entity<Self> {
        let model = ShellViewModel::new(snapshot);
        let mentions = model
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
        let initial_draft = model.draft().to_owned();
        let composer = cx.new(|cx| {
            Composer::new(
                initial_draft,
                mentions,
                "Message this channel, or @mention an agent",
                cx,
            )
        });

        cx.new(|cx| {
            let subscription = cx.subscribe(&composer, |shell: &mut Self, _, event, cx| {
                match event {
                    ComposerEvent::Changed(body) => {
                        shell.model.set_draft(body.clone());
                        shell.save_draft(body);
                    }
                    ComposerEvent::Submitted(body) => shell.submit(body, cx),
                }
                cx.notify();
            });
            if let Some((core, workspace_id)) = backend.clone() {
                // ponytail: 1s full-snapshot poll; push AppEvents over a channel if it gets slow.
                cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
                    loop {
                        cx.background_executor().timer(Duration::from_secs(1)).await;
                        let Ok(snapshot) = core.store.snapshot(workspace_id) else {
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
                model,
                backend,
                composer,
                _subscriptions: vec![subscription],
            }
        })
    }

    fn save_draft(&self, body: &str) {
        let (Some((core, _)), Some(context)) = (&self.backend, self.model.context()) else {
            return;
        };
        let draft = Draft {
            channel_id: context.channel_id,
            thread_id: context.thread_id,
            body: body.to_owned(),
            updated_at: 0,
        };
        if let Err(error) = core.dispatch(AppCommand::SaveDraft { draft }) {
            eprintln!("toma: saving draft failed: {error}");
        }
    }

    fn submit(&mut self, body: &str, cx: &mut Context<Self>) {
        let Some((core, workspace_id)) = self.backend.clone() else {
            return self.append_local_message(body);
        };
        let Some(context) = self.model.context() else {
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
            channel_id: context.channel_id,
            thread_id: context.thread_id,
            body: body.to_owned(),
            attachments,
        };
        // Agent runs block until the CLI exits, so dispatch off the UI thread.
        let task = cx.background_spawn(async move {
            let result = core.dispatch(command);
            (result, core.store.snapshot(workspace_id))
        });
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let (result, snapshot) = task.await;
            if let Err(error) = result {
                eprintln!("toma: posting message failed: {error}");
            }
            if let Ok(snapshot) = snapshot {
                this.update(cx, |shell, cx| {
                    shell.model.replace_snapshot(snapshot);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn append_local_message(&mut self, body: &str) {
        let Some(context) = self.model.context() else {
            return;
        };
        let author = self
            .model
            .snapshot
            .people
            .first()
            .map(|person| MessageAuthor::Person(person.id))
            .unwrap_or(MessageAuthor::System);
        self.model.snapshot.messages.push(Message {
            id: MessageId::new(),
            channel_id: context.channel_id,
            thread_id: context.thread_id,
            author,
            body: body.to_owned(),
            created_at: 0,
        });
        self.model.set_draft("");
    }

    fn sync_composer(&mut self, cx: &mut Context<Self>) {
        let draft = self.model.draft().to_owned();
        self.composer
            .update(cx, |composer, cx| composer.set_text(draft, cx));
        cx.notify();
    }

    fn workspace_rail(&self) -> impl IntoElement {
        let initial = self
            .model
            .snapshot
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.name.chars().next())
            .unwrap_or('T');
        div()
            .w(px(58.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .bg(rgb(0x20252d))
            .pt(px(38.))
            .gap_3()
            .child(
                div()
                    .size(px(34.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(rgb(0xf0c765))
                    .text_color(rgb(0x20252d))
                    .text_sm()
                    .child(initial.to_string()),
            )
            .child(div().w(px(24.)).h(px(1.)).bg(rgb(0x48505b)))
            .child(
                div()
                    .size(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(rgb(0x39414c))
                    .text_color(rgb(0xcbd2da))
                    .text_xs()
                    .child("+"),
            )
            .child(div().flex_grow())
            .child(
                div()
                    .mb_3()
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(rgb(0x313842))
                    .text_color(rgb(0xabb4bf))
                    .text_xs()
                    .child("?"),
            )
    }

    fn channel_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace_name = self
            .model
            .snapshot
            .workspace
            .as_ref()
            .map(|workspace| workspace.name.as_str())
            .unwrap_or("Workspace")
            .to_owned();
        let selected = self.model.selected_channel_id();
        let channels = self.model.snapshot.channels.clone();
        let run_rows = self
            .model
            .active_runs()
            .into_iter()
            .map(|(agent, run)| (agent.name.clone(), run.status))
            .collect::<Vec<_>>();

        div()
            .w(px(232.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(rgb(0xf4f5f7))
            .border_r_1()
            .border_color(rgb(0xdfe2e6))
            .child(
                div()
                    .h(px(58.))
                    .flex_shrink_0()
                    .flex()
                    .items_end()
                    .px_4()
                    .pb_3()
                    .border_b_1()
                    .border_color(rgb(0xdfe2e6))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x1d2530))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(workspace_name),
                    ),
            )
            .child(
                div()
                    .id("channel-list")
                    .flex_grow()
                    .overflow_scroll()
                    .px_2()
                    .py_3()
                    .child(section_label("Channels"))
                    .children(channels.into_iter().map(|channel| {
                        let id = channel.id;
                        div()
                            .id(("channel", channel.position as u64))
                            .h(px(30.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .rounded_sm()
                            .cursor_pointer()
                            .when(selected == Some(id), |row| {
                                row.bg(rgb(0xe3e7ec)).text_color(rgb(0x19212b))
                            })
                            .when(selected != Some(id), |row| {
                                row.text_color(rgb(0x59616d))
                                    .hover(|row| row.bg(rgb(0xeaedf0)))
                            })
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.model.select_channel(id);
                                    this.sync_composer(cx);
                                }),
                            )
                            .child(div().w(px(14.)).text_color(rgb(0x8a929d)).child("#"))
                            .child(div().text_sm().truncate().child(channel.name))
                    }))
                    .child(div().mt_5().child(section_label("Agents & runs")))
                    .children(run_rows.into_iter().map(|(name, status)| {
                        let (label, color) = status_style(status);
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_2()
                            .child(div().size(px(7.)).rounded_full().bg(color))
                            .child(
                                div()
                                    .flex_grow()
                                    .text_sm()
                                    .text_color(rgb(0x3e4753))
                                    .truncate()
                                    .child(name),
                            )
                            .child(div().text_xs().text_color(rgb(0x7b8490)).child(label))
                    })),
            )
            .child(
                div()
                    .mx_3()
                    .mb_3()
                    .p_3()
                    .border_1()
                    .border_color(rgb(0xd9dde2))
                    .rounded_md()
                    .bg(rgb(0xfafafa))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x747d89))
                            .child("LOCAL SNAPSHOT"),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .text_color(rgb(0x414a55))
                            .child("Infrastructure disconnected"),
                    ),
            )
    }

    fn conversation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let channel_name = self
            .model
            .selected_channel()
            .map(|channel| channel.name.clone())
            .unwrap_or_else(|| "No channel".into());
        let messages = self.model.messages_for(None).cloned().collect::<Vec<_>>();
        let message_rows = messages
            .iter()
            .cloned()
            .map(|message| self.message_row(message, cx).into_any_element())
            .collect::<Vec<_>>();
        let show_composer = self.model.open_thread_id().is_none();
        div()
            .min_w(px(340.))
            .flex_grow()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0xffffff))
            .child(
                div()
                    .h(px(58.))
                    .flex_shrink_0()
                    .flex()
                    .items_end()
                    .justify_between()
                    .px_5()
                    .pb_3()
                    .border_b_1()
                    .border_color(rgb(0xe2e5e9))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_color(rgb(0x89919b)).child("#"))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0x1d2530))
                                    .child(channel_name),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x838c97))
                            .child(format!("{} messages", messages.len())),
                    ),
            )
            .child(
                div()
                    .id("messages")
                    .flex_grow()
                    .overflow_scroll()
                    .px_5()
                    .py_4()
                    .children(message_rows),
            )
            .children(show_composer.then(|| {
                div()
                    .flex_shrink_0()
                    .px_5()
                    .pb_4()
                    .pt_2()
                    .child(self.composer.clone())
            }))
    }

    fn message_row(&self, message: Message, cx: &mut Context<Self>) -> impl IntoElement {
        let author = self.model.author_name(message.author).to_owned();
        let initial = author.chars().next().unwrap_or('T').to_string();
        let thread = self
            .model
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.root_message_id == message.id)
            .cloned();
        div()
            .flex()
            .gap_3()
            .py_3()
            .border_b_1()
            .border_color(rgb(0xf0f1f3))
            .child(
                div()
                    .mt_1()
                    .size(px(30.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(avatar_color(message.author))
                    .text_xs()
                    .text_color(rgb(0xffffff))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(initial),
            )
            .child(
                div()
                    .min_w(px(0.))
                    .flex_grow()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0x242b35))
                                    .child(author),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x99a0a9))
                                    .child(time_label(message.created_at)),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_sm()
                            .line_height(px(20.))
                            .text_color(rgb(0x414954))
                            .whitespace_normal()
                            .child(message.body),
                    )
                    .children(thread.map(|thread| {
                        let id = thread.id;
                        let replies = self
                            .model
                            .snapshot
                            .messages
                            .iter()
                            .filter(|message| message.thread_id == Some(id))
                            .count();
                        div()
                            .id("open-thread")
                            .mt_2()
                            .text_xs()
                            .text_color(rgb(0x466d9f))
                            .cursor_pointer()
                            .hover(|row| row.text_color(rgb(0x264b7d)))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.model.open_thread(id);
                                    this.sync_composer(cx);
                                }),
                            )
                            .child(format!("{} replies  Open thread", replies))
                    })),
            )
    }

    fn thread_pane(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let thread_id = self.model.open_thread_id()?;
        let thread = self
            .model
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)?
            .clone();
        let messages = self
            .model
            .messages_for(Some(thread_id))
            .cloned()
            .collect::<Vec<_>>();
        let (status, color) = status_style(thread.status);
        Some(
            div()
                .w(px(350.))
                .h_full()
                .flex_shrink_0()
                .flex()
                .flex_col()
                .bg(rgb(0xfafbfc))
                .border_l_1()
                .border_color(rgb(0xdfe3e7))
                .child(
                    div()
                        .h(px(58.))
                        .flex_shrink_0()
                        .flex()
                        .items_end()
                        .justify_between()
                        .px_4()
                        .pb_3()
                        .border_b_1()
                        .border_color(rgb(0xe2e5e9))
                        .child(
                            div()
                                .min_w(px(0.))
                                .child(div().text_xs().text_color(rgb(0x858e99)).child("THREAD"))
                                .child(
                                    div()
                                        .text_sm()
                                        .truncate()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgb(0x222a34))
                                        .child(thread.title),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(div().size(px(7.)).rounded_full().bg(color))
                                        .child(
                                            div().text_xs().text_color(rgb(0x707985)).child(status),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("close-thread")
                                        .size(px(24.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_sm()
                                        .cursor_pointer()
                                        .text_color(rgb(0x69727d))
                                        .hover(|button| button.bg(rgb(0xe9ecef)))
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                this.model.close_thread();
                                                this.sync_composer(cx);
                                            }),
                                        )
                                        .child("x"),
                                ),
                        ),
                )
                .child(
                    div()
                        .id("thread-messages")
                        .flex_grow()
                        .overflow_scroll()
                        .px_4()
                        .py_3()
                        .children(
                            messages
                                .into_iter()
                                .map(|message| self.compact_message(message)),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .px_4()
                        .pb_4()
                        .pt_2()
                        .child(self.composer.clone()),
                ),
        )
    }

    fn compact_message(&self, message: Message) -> impl IntoElement {
        let author = self.model.author_name(message.author).to_owned();
        div()
            .py_3()
            .border_b_1()
            .border_color(rgb(0xe9ebee))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0x303843))
                            .child(author),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x9aa1aa))
                            .child(time_label(message.created_at)),
                    ),
            )
            .child(
                div()
                    .mt_1()
                    .text_sm()
                    .line_height(px(20.))
                    .text_color(rgb(0x4a525d))
                    .whitespace_normal()
                    .child(message.body),
            )
    }
}

impl Render for TomaShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x202731))
            .child(self.workspace_rail())
            .child(self.channel_sidebar(cx))
            .child(self.conversation(cx))
            .children(self.thread_pane(cx))
    }
}

fn section_label(label: &'static str) -> impl IntoElement {
    div()
        .px_2()
        .pb_2()
        .text_xs()
        .text_color(rgb(0x7f8893))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(label)
}

fn time_label(timestamp: TimestampMs) -> String {
    if timestamp <= 0 {
        "now".into()
    } else {
        format!("{:02}:{:02}", 9 + (timestamp / 60) % 9, timestamp % 60)
    }
}

fn avatar_color(author: MessageAuthor) -> gpui::Rgba {
    match author {
        MessageAuthor::Person(_) => rgb(0x4c7b72),
        MessageAuthor::Agent(_) => rgb(0x667da5),
        MessageAuthor::System => rgb(0x737b85),
    }
}

fn status_style(status: WorkStatus) -> (&'static str, gpui::Rgba) {
    match status {
        WorkStatus::Queued => ("Queued", rgb(0x9aa1aa)),
        WorkStatus::Reading => ("Reading", rgb(0x5683ad)),
        WorkStatus::Working => ("Working", rgb(0x3f8a70)),
        WorkStatus::WaitingForInput => ("Waiting", rgb(0xc08a35)),
        WorkStatus::Blocked => ("Blocked", rgb(0xb45f55)),
        WorkStatus::Completed => ("Done", rgb(0x668c61)),
        WorkStatus::Failed => ("Failed", rgb(0xb75151)),
        WorkStatus::Cancelled => ("Cancelled", rgb(0x8b8f96)),
    }
}
