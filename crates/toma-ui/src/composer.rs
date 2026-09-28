use gpui::{
    Context, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton, Render,
    SharedString, Window, div, prelude::*, px, rgb,
};
use toma_domain::AgentId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MentionCandidate {
    pub id: AgentId,
    pub name: String,
    pub role: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComposerModel {
    text: String,
    cursor: usize,
    mentions: Vec<MentionCandidate>,
    mention_start: Option<usize>,
    selected_mention: usize,
}

impl ComposerModel {
    pub fn new(text: impl Into<String>, mentions: Vec<MentionCandidate>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            mentions,
            mention_start: None,
            selected_mention: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn palette_open(&self) -> bool {
        self.mention_start.is_some()
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.mention_start = None;
        self.selected_mention = 0;
    }

    pub fn insert(&mut self, value: &str) {
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
        if value == "@" {
            self.mention_start = Some(self.cursor - 1);
        }
        self.refresh_palette();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let previous = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(offset, _)| offset)
            .unwrap_or(0);
        self.text.replace_range(previous..self.cursor, "");
        self.cursor = previous;
        self.refresh_palette();
    }

    pub fn move_left(&mut self) {
        self.cursor = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(offset, _)| offset)
            .unwrap_or(0);
        self.refresh_palette();
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor += self.text[self.cursor..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(0);
        }
        self.refresh_palette();
    }

    pub fn filtered_mentions(&self) -> Vec<&MentionCandidate> {
        let query = self.mention_query().unwrap_or_default().to_lowercase();
        self.mentions
            .iter()
            .filter(|candidate| {
                candidate.name.to_lowercase().contains(&query)
                    || candidate.role.to_lowercase().contains(&query)
            })
            .collect()
    }

    pub fn select_next(&mut self, delta: isize) {
        let count = self.filtered_mentions().len();
        if count == 0 {
            return;
        }
        self.selected_mention =
            (self.selected_mention as isize + delta).rem_euclid(count as isize) as usize;
    }

    pub fn accept_mention(&mut self) -> bool {
        let Some(start) = self.mention_start else {
            return false;
        };
        let selected = self
            .filtered_mentions()
            .get(self.selected_mention)
            .cloned()
            .cloned();
        let Some(candidate) = selected else {
            return false;
        };
        let replacement = format!("@{} ", candidate.name);
        self.text.replace_range(start..self.cursor, &replacement);
        self.cursor = start + replacement.len();
        self.mention_start = None;
        self.selected_mention = 0;
        true
    }

    pub fn dismiss_palette(&mut self) {
        self.mention_start = None;
    }

    pub fn take_submission(&mut self) -> Option<String> {
        let body = self.text.trim().to_owned();
        if body.is_empty() {
            return None;
        }
        self.text.clear();
        self.cursor = 0;
        self.mention_start = None;
        Some(body)
    }

    fn mention_query(&self) -> Option<&str> {
        let start = self.mention_start?;
        self.text.get(start + 1..self.cursor)
    }

    fn refresh_palette(&mut self) {
        if let Some(start) = self.mention_start {
            let valid = start < self.cursor
                && self.text.get(start..=start) == Some("@")
                && self.text[start + 1..self.cursor]
                    .chars()
                    .all(|character| !character.is_whitespace());
            if !valid {
                self.mention_start = None;
            }
        }
        let count = self.filtered_mentions().len();
        if count == 0 {
            self.selected_mention = 0;
        } else {
            self.selected_mention %= count;
        }
    }
}

pub enum ComposerEvent {
    Changed(String),
    Submitted(String),
}

pub struct Composer {
    model: ComposerModel,
    focus_handle: FocusHandle,
    placeholder: SharedString,
}

impl Composer {
    pub fn new(
        text: impl Into<String>,
        mentions: Vec<MentionCandidate>,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            model: ComposerModel::new(text, mentions),
            focus_handle: cx.focus_handle(),
            placeholder: placeholder.into(),
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.model.set_text(text);
        cx.notify();
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(ComposerEvent::Changed(self.model.text().to_owned()));
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if self.model.palette_open() {
            match key {
                "up" => {
                    self.model.select_next(-1);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "down" => {
                    self.model.select_next(1);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "escape" => {
                    self.model.dismiss_palette();
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "enter" if !event.keystroke.modifiers.shift && self.model.accept_mention() => {
                    cx.stop_propagation();
                    self.changed(cx);
                    return;
                }
                _ => {}
            }
        }
        match key {
            "enter" if event.keystroke.modifiers.shift => self.model.insert("\n"),
            "enter" => {
                if let Some(body) = self.model.take_submission() {
                    cx.emit(ComposerEvent::Changed(String::new()));
                    cx.emit(ComposerEvent::Submitted(body));
                    cx.notify();
                }
                cx.stop_propagation();
                return;
            }
            "backspace" => self.model.backspace(),
            "left" => self.model.move_left(),
            "right" => self.model.move_right(),
            _ if !event.keystroke.modifiers.control && !event.keystroke.modifiers.platform => {
                if let Some(character) = event.keystroke.key_char.as_deref() {
                    self.model.insert(character);
                } else {
                    return;
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        self.changed(cx);
    }
}

impl EventEmitter<ComposerEvent> for Composer {}
impl Focusable for Composer {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let display = if self.model.text().is_empty() {
            vec![self.placeholder.to_string()]
        } else {
            let mut text = self.model.text().to_owned();
            if focused {
                text.insert(self.model.cursor(), '|');
            }
            text.split('\n').map(str::to_owned).collect()
        };
        let mentions: Vec<_> = self
            .model
            .filtered_mentions()
            .into_iter()
            .cloned()
            .collect();
        let selected = self.model.selected_mention;

        div()
            .relative()
            .w_full()
            .children(
                (self.model.palette_open() && !mentions.is_empty()).then(|| {
                    div()
                        .absolute()
                        .bottom(px(96.))
                        .left_0()
                        .w(px(340.))
                        .bg(rgb(0xffffff))
                        .border_1()
                        .border_color(rgb(0xd9dde3))
                        .rounded_md()
                        .shadow_lg()
                        .p_1()
                        .children(mentions.into_iter().enumerate().map(|(index, mention)| {
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .px_3()
                                .py_2()
                                .rounded_sm()
                                .when(index == selected, |row| row.bg(rgb(0xeef3fb)))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(0x1e2936))
                                        .child(format!("@{}", mention.name)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(0x77808c))
                                        .child(mention.role),
                                )
                        }))
                }),
            )
            .child(
                div()
                    .id("composer")
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(Self::key_down))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, _| window.focus(&this.focus_handle)),
                    )
                    .min_h(px(88.))
                    .w_full()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(if focused {
                        rgb(0x7691b2)
                    } else {
                        rgb(0xcfd5dc)
                    })
                    .bg(rgb(0xffffff))
                    .text_sm()
                    .line_height(px(20.))
                    .text_color(if self.model.text().is_empty() {
                        rgb(0x929aa5)
                    } else {
                        rgb(0x202631)
                    })
                    .children(display.into_iter().map(|line| {
                        div().min_h(px(20.)).child(if line.is_empty() {
                            " ".to_owned()
                        } else {
                            line
                        })
                    }))
                    .child(
                        div()
                            .absolute()
                            .right(px(10.))
                            .bottom(px(8.))
                            .text_xs()
                            .text_color(rgb(0x929aa5))
                            .child("Enter to send"),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> Vec<MentionCandidate> {
        vec![
            MentionCandidate {
                id: AgentId::new(),
                name: "Builder".into(),
                role: "Implementation".into(),
            },
            MentionCandidate {
                id: AgentId::new(),
                name: "Reviewer".into(),
                role: "Code review".into(),
            },
        ]
    }

    #[test]
    fn mention_palette_filters_and_inserts_structured_candidate() {
        let mut model = ComposerModel::new("", candidates());
        model.insert("@");
        model.insert("rev");
        assert_eq!(model.filtered_mentions()[0].name, "Reviewer");
        assert!(model.accept_mention());
        assert_eq!(model.text(), "@Reviewer ");
    }

    #[test]
    fn multiline_submission_trims_and_resets() {
        let mut model = ComposerModel::new("first\nsecond  ", candidates());
        assert_eq!(model.take_submission().as_deref(), Some("first\nsecond"));
        assert_eq!(model.text(), "");
    }

    #[test]
    fn empty_submission_is_ignored() {
        let mut model = ComposerModel::new("  \n", candidates());
        assert_eq!(model.take_submission(), None);
    }
}
