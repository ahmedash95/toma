use std::ops::Range;
use std::time::Duration;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render, ScrollWheelEvent, SharedString,
    Style, Task, TextRun, UTF16Selection, UnderlineStyle, WeakEntity, Window, WrappedLine, div,
    fill, point, prelude::*, px, relative, size,
};
use toma_domain::AgentId;

use crate::Theme;

const FONT_SIZE: Pixels = px(13.);
const LINE_HEIGHT: Pixels = px(20.);
const MAX_LINES: f32 = 8.;
const BLINK: Duration = Duration::from_millis(530);

mod act {
    gpui::actions!(
        composer,
        [
            Enter,
            Newline,
            Escape,
            Tab,
            Up,
            Down,
            Left,
            Right,
            SelectLeft,
            SelectRight,
            WordLeft,
            WordRight,
            SelectWordLeft,
            SelectWordRight,
            LineStart,
            LineEnd,
            SelectLineStart,
            SelectLineEnd,
            SelectAll,
            Backspace,
            Delete,
            DeleteToLineStart,
            Copy,
            Cut,
            Paste,
            ShowCharacterPalette,
        ]
    );
}

/// Registers the composer key bindings. Call once at app startup.
pub fn bind_keys(cx: &mut App) {
    use act::*;
    let c = Some("Composer");
    cx.bind_keys([
        KeyBinding::new("enter", Enter, c),
        KeyBinding::new("shift-enter", Newline, c),
        KeyBinding::new("escape", Escape, c),
        KeyBinding::new("tab", Tab, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("alt-left", WordLeft, c),
        KeyBinding::new("alt-right", WordRight, c),
        KeyBinding::new("alt-shift-left", SelectWordLeft, c),
        KeyBinding::new("alt-shift-right", SelectWordRight, c),
        KeyBinding::new("cmd-left", LineStart, c),
        KeyBinding::new("cmd-right", LineEnd, c),
        KeyBinding::new("home", LineStart, c),
        KeyBinding::new("end", LineEnd, c),
        KeyBinding::new("cmd-shift-left", SelectLineStart, c),
        KeyBinding::new("cmd-shift-right", SelectLineEnd, c),
        KeyBinding::new("cmd-a", SelectAll, c),
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, c),
        KeyBinding::new("cmd-c", Copy, c),
        KeyBinding::new("cmd-x", Cut, c),
        KeyBinding::new("cmd-v", Paste, c),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, c),
    ]);
}

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
    /// Other end of the selection; equal to `cursor` when nothing is selected.
    anchor: usize,
    mentions: Vec<MentionCandidate>,
    mention_start: Option<usize>,
    selected_mention: usize,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl ComposerModel {
    pub fn new(text: impl Into<String>, mentions: Vec<MentionCandidate>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            anchor: cursor,
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
    pub fn anchor(&self) -> usize {
        self.anchor
    }
    pub fn selection(&self) -> Range<usize> {
        self.cursor.min(self.anchor)..self.cursor.max(self.anchor)
    }
    pub fn palette_open(&self) -> bool {
        self.mention_start.is_some()
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.anchor = self.cursor;
        self.mention_start = None;
        self.selected_mention = 0;
    }

    /// Sets the selection; offsets are clamped to the text and char boundaries.
    pub fn set_selection(&mut self, anchor: usize, cursor: usize) {
        self.anchor = self.clamp(anchor);
        self.cursor = self.clamp(cursor);
        self.refresh_palette();
    }

    pub fn move_to(&mut self, offset: usize, extend: bool) {
        self.cursor = self.clamp(offset);
        if !extend {
            self.anchor = self.cursor;
        }
        self.refresh_palette();
    }

    /// Replaces `range` with `value` and puts the caret after it.
    pub fn replace_range(&mut self, range: Range<usize>, value: &str) {
        let start = self.clamp(range.start);
        let end = self.clamp(range.end).max(start);
        if self.mention_start.is_some_and(|s| start <= s) {
            self.mention_start = None;
        }
        self.text.replace_range(start..end, value);
        self.cursor = start + value.len();
        self.anchor = self.cursor;
        if value == "@" {
            self.mention_start = Some(start);
        }
        self.refresh_palette();
    }

    pub fn insert(&mut self, value: &str) {
        self.replace_range(self.selection(), value);
    }

    pub fn backspace(&mut self) {
        if self.selection().is_empty() {
            self.anchor = self.prev_boundary(self.cursor);
        }
        self.insert("");
    }

    pub fn delete_forward(&mut self) {
        if self.selection().is_empty() {
            self.anchor = self.next_boundary(self.cursor);
        }
        self.insert("");
    }

    pub fn delete_to_line_start(&mut self) {
        if self.selection().is_empty() {
            let start = self.line_start(self.cursor);
            // At a line start, delete the preceding newline instead.
            self.anchor = if start == self.cursor {
                self.prev_boundary(self.cursor)
            } else {
                start
            };
        }
        self.insert("");
    }

    /// Moves one character; a plain move over a selection collapses it to that edge.
    pub fn step(&mut self, forward: bool, extend: bool) {
        let selection = self.selection();
        let target = match (selection.is_empty() || extend, forward) {
            (true, true) => self.next_boundary(self.cursor),
            (true, false) => self.prev_boundary(self.cursor),
            (false, true) => selection.end,
            (false, false) => selection.start,
        };
        self.move_to(target, extend);
    }

    pub fn move_left(&mut self) {
        self.step(false, false);
    }

    pub fn move_right(&mut self) {
        self.step(true, false);
    }

    pub fn prev_boundary(&self, offset: usize) -> usize {
        self.text[..self.clamp(offset)]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    pub fn next_boundary(&self, offset: usize) -> usize {
        let offset = self.clamp(offset);
        offset + self.text[offset..].chars().next().map_or(0, char::len_utf8)
    }

    pub fn word_left(&self, offset: usize) -> usize {
        let head = &self.text[..self.clamp(offset)];
        let trimmed = head.trim_end_matches(|c: char| !is_word(c));
        trimmed.trim_end_matches(is_word).len()
    }

    pub fn word_right(&self, offset: usize) -> usize {
        let offset = self.clamp(offset);
        let rest = self.text[offset..].trim_start_matches(|c: char| !is_word(c));
        let rest = rest.trim_start_matches(is_word);
        self.text.len() - rest.len()
    }

    pub fn line_start(&self, offset: usize) -> usize {
        self.text[..self.clamp(offset)]
            .rfind('\n')
            .map_or(0, |i| i + 1)
    }

    pub fn line_end(&self, offset: usize) -> usize {
        let offset = self.clamp(offset);
        self.text[offset..]
            .find('\n')
            .map_or(self.text.len(), |i| offset + i)
    }

    fn clamp(&self, mut offset: usize) -> usize {
        offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
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
        self.anchor = self.cursor;
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
        self.anchor = 0;
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

/// Text laid out by the last paint, in content coordinates (origin = top-left of the text area).
#[derive(Default)]
struct Layout {
    lines: Vec<WrappedLine>,
    bounds: Bounds<Pixels>,
}

impl Layout {
    fn line_starts(&self) -> impl Iterator<Item = (usize, &WrappedLine)> {
        let mut start = 0;
        self.lines.iter().map(move |line| {
            let s = start;
            start += line.len() + 1;
            (s, line)
        })
    }

    fn point_for_index(&self, index: usize) -> Point<Pixels> {
        let mut y = px(0.);
        for (start, line) in self.line_starts() {
            if index <= start + line.len() {
                let p = line
                    .position_for_index(index - start, LINE_HEIGHT)
                    .unwrap_or_default();
                return point(p.x, y + p.y);
            }
            y += line.size(LINE_HEIGHT).height;
        }
        point(px(0.), y)
    }

    fn index_for_point(&self, p: Point<Pixels>) -> usize {
        let mut y = px(0.);
        let mut end = 0;
        for (start, line) in self.line_starts() {
            let height = line.size(LINE_HEIGHT).height;
            end = start + line.len();
            if p.y < y + height {
                let local = point(p.x, (p.y - y).max(px(0.)));
                let found = line.closest_index_for_position(local, LINE_HEIGHT);
                return start + found.unwrap_or_else(|e| e);
            }
            y += height;
        }
        end
    }

    fn height(&self) -> Pixels {
        self.lines
            .iter()
            .fold(px(0.), |acc, l| acc + l.size(LINE_HEIGHT).height)
    }
}

pub struct Composer {
    model: ComposerModel,
    focus_handle: FocusHandle,
    placeholder: SharedString,
    marked_range: Option<Range<usize>>,
    layout: Layout,
    scroll_y: Pixels,
    scroll_to_caret: bool,
    is_selecting: bool,
    caret_visible: bool,
    blink_epoch: usize,
    blink_reset: bool,
    blink_task: Option<Task<()>>,
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
            marked_range: None,
            layout: Layout::default(),
            scroll_y: px(0.),
            scroll_to_caret: true,
            is_selecting: false,
            caret_visible: true,
            blink_epoch: 0,
            blink_reset: true,
            blink_task: None,
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.model.set_text(text);
        self.marked_range = None;
        self.moved(cx);
    }

    /// Caret moved or text edited: solid caret, scroll it into view.
    fn moved(&mut self, cx: &mut Context<Self>) {
        self.blink_reset = true;
        self.scroll_to_caret = true;
        cx.notify();
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(ComposerEvent::Changed(self.model.text().to_owned()));
        self.moved(cx);
    }

    fn restart_blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.blink_reset = false;
        self.caret_visible = true;
        self.blink_epoch += 1;
        let epoch = self.blink_epoch;
        self.blink_task = Some(
            cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| {
                loop {
                    cx.background_executor().timer(BLINK).await;
                    let keep = this.update_in(cx, |this, window, cx| {
                        if this.blink_epoch != epoch {
                            return false;
                        }
                        if this.focus_handle.is_focused(window) {
                            this.caret_visible = !this.caret_visible;
                            cx.notify();
                        }
                        true
                    });
                    if !matches!(keep, Ok(true)) {
                        break;
                    }
                }
            }),
        );
    }

    fn selected_text(&self) -> Option<String> {
        let range = self.model.selection();
        (!range.is_empty()).then(|| self.model.text()[range].to_owned())
    }

    fn move_vertical(&mut self, delta: f32, cx: &mut Context<Self>) {
        let p = self.layout.point_for_index(self.model.cursor());
        let y = p.y + LINE_HEIGHT * (delta + 0.5);
        let index = if y < px(0.) {
            0
        } else {
            self.layout.index_for_point(point(p.x, y))
        };
        self.model.move_to(index, false);
        self.moved(cx);
    }

    fn enter(&mut self, _: &act::Enter, _: &mut Window, cx: &mut Context<Self>) {
        if self.model.palette_open() && self.model.accept_mention() {
            self.marked_range = None;
            return self.changed(cx);
        }
        if let Some(body) = self.model.take_submission() {
            self.marked_range = None;
            cx.emit(ComposerEvent::Changed(String::new()));
            cx.emit(ComposerEvent::Submitted(body));
            self.moved(cx);
        }
    }

    fn newline(&mut self, _: &act::Newline, _: &mut Window, cx: &mut Context<Self>) {
        self.model.insert("\n");
        self.changed(cx);
    }

    fn escape(&mut self, _: &act::Escape, _: &mut Window, cx: &mut Context<Self>) {
        if self.model.palette_open() {
            self.model.dismiss_palette();
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    fn tab(&mut self, _: &act::Tab, _: &mut Window, cx: &mut Context<Self>) {
        if self.model.palette_open() && self.model.accept_mention() {
            self.changed(cx);
        } else {
            cx.propagate();
        }
    }

    fn up(&mut self, _: &act::Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.model.palette_open() {
            self.model.select_next(-1);
            cx.notify();
        } else {
            self.move_vertical(-1., cx);
        }
    }

    fn down(&mut self, _: &act::Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.model.palette_open() {
            self.model.select_next(1);
            cx.notify();
        } else {
            self.move_vertical(1., cx);
        }
    }

    fn step(&mut self, forward: bool, extend: bool, cx: &mut Context<Self>) {
        self.model.step(forward, extend);
        self.moved(cx);
    }

    fn left(&mut self, _: &act::Left, _: &mut Window, cx: &mut Context<Self>) {
        self.step(false, false, cx);
    }
    fn right(&mut self, _: &act::Right, _: &mut Window, cx: &mut Context<Self>) {
        self.step(true, false, cx);
    }
    fn select_left(&mut self, _: &act::SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.step(false, true, cx);
    }
    fn select_right(&mut self, _: &act::SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.step(true, true, cx);
    }

    fn move_by(
        &mut self,
        target: impl Fn(&ComposerModel, usize) -> usize,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        let to = target(&self.model, self.model.cursor());
        self.model.move_to(to, extend);
        self.moved(cx);
    }

    fn word_left(&mut self, _: &act::WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_by(ComposerModel::word_left, false, cx);
    }
    fn word_right(&mut self, _: &act::WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_by(ComposerModel::word_right, false, cx);
    }
    fn select_word_left(
        &mut self,
        _: &act::SelectWordLeft,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_by(ComposerModel::word_left, true, cx);
    }
    fn select_word_right(
        &mut self,
        _: &act::SelectWordRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_by(ComposerModel::word_right, true, cx);
    }
    fn line_start(&mut self, _: &act::LineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_by(ComposerModel::line_start, false, cx);
    }
    fn line_end(&mut self, _: &act::LineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_by(ComposerModel::line_end, false, cx);
    }
    fn select_line_start(
        &mut self,
        _: &act::SelectLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_by(ComposerModel::line_start, true, cx);
    }
    fn select_line_end(&mut self, _: &act::SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_by(ComposerModel::line_end, true, cx);
    }
    fn select_all(&mut self, _: &act::SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.model.set_selection(0, self.model.text().len());
        self.moved(cx);
    }

    fn edit(&mut self, f: impl FnOnce(&mut ComposerModel), cx: &mut Context<Self>) {
        f(&mut self.model);
        self.marked_range = None;
        self.changed(cx);
    }

    fn backspace(&mut self, _: &act::Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(ComposerModel::backspace, cx);
    }
    fn delete(&mut self, _: &act::Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(ComposerModel::delete_forward, cx);
    }
    fn delete_to_line_start(
        &mut self,
        _: &act::DeleteToLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit(ComposerModel::delete_to_line_start, cx);
    }

    fn copy(&mut self, _: &act::Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }
    fn cut(&mut self, _: &act::Cut, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.replace_text_in_range(None, "", window, cx);
        }
    }
    fn paste(&mut self, _: &act::Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.replace_text_in_range(None, &text, window, cx);
        }
    }
    fn show_character_palette(
        &mut self,
        _: &act::ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn mouse_index(&self, position: Point<Pixels>) -> usize {
        let local = position - self.layout.bounds.origin;
        self.layout
            .index_for_point(point(local.x, local.y + self.scroll_y))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.is_selecting = true;
        let index = self.mouse_index(event.position);
        self.model.move_to(index, event.modifiers.shift);
        self.marked_range = None;
        self.moved(cx);
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            let index = self.mouse_index(event.position);
            self.model.move_to(index, true);
            self.moved(cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_y -= event.delta.pixel_delta(LINE_HEIGHT).y;
        cx.notify();
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.model.text().chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.model.text().chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

impl EntityInputHandler for Composer {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.model.text()[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.model.selection()),
            reversed: self.model.anchor() > self.model.cursor(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or_else(|| self.model.selection());
        self.model.replace_range(range, new_text);
        self.marked_range = None;
        self.changed(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or_else(|| self.model.selection());
        self.model.replace_range(range.clone(), new_text);
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        if let Some(sel) = new_selected_range_utf16 {
            let start = range.start;
            // The IME's selection is relative to the marked text.
            let len_utf16 = new_text.encode_utf16().count();
            let (a, b) = (sel.start.min(len_utf16), sel.end.min(len_utf16));
            let to_byte = |units: usize| {
                let mut count = 0;
                for (i, ch) in new_text.char_indices() {
                    if count >= units {
                        return i;
                    }
                    count += ch.len_utf16();
                }
                new_text.len()
            };
            self.model
                .set_selection(start + to_byte(a), start + to_byte(b));
        }
        self.changed(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let start = self.layout.point_for_index(range.start);
        let end = self.layout.point_for_index(range.end);
        let origin = bounds.origin - point(px(0.), self.scroll_y);
        Some(Bounds::from_corners(
            origin + start,
            origin + point(end.x.max(start.x + px(1.)), end.y + LINE_HEIGHT),
        ))
    }

    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.mouse_index(p)))
    }
}

impl EventEmitter<ComposerEvent> for Composer {}
impl Focusable for Composer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn shape(
    window: &mut Window,
    text: SharedString,
    color: Hsla,
    marked: Option<Range<usize>>,
    font: Font,
    width: Option<Pixels>,
) -> Vec<WrappedLine> {
    let run = TextRun {
        len: text.len(),
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let runs: Vec<TextRun> = match marked.filter(|m| m.end <= text.len()) {
        Some(m) => vec![
            TextRun {
                len: m.start,
                ..run.clone()
            },
            TextRun {
                len: m.end - m.start,
                underline: Some(UnderlineStyle {
                    color: Some(color),
                    thickness: px(1.),
                    wavy: false,
                }),
                ..run.clone()
            },
            TextRun {
                len: text.len() - m.end,
                ..run
            },
        ],
        None => vec![run],
    };
    window
        .text_system()
        .shape_text(text, FONT_SIZE, &runs, width, None)
        .map(|lines| lines.into_vec())
        .unwrap_or_default()
}

fn display_text(input: &Composer, cx: &App) -> (SharedString, Hsla) {
    let theme = Theme::get(cx);
    if input.model.text().is_empty() {
        (input.placeholder.clone(), theme.text_tertiary)
    } else {
        (input.model.text().to_owned().into(), theme.text)
    }
}

struct TextElement {
    input: Entity<Composer>,
}

struct Prepaint {
    lines: Vec<WrappedLine>,
    selection: Vec<PaintQuad>,
    caret: Option<PaintQuad>,
    scroll_y: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let input = self.input.read(cx);
        let (text, color) = display_text(input, cx);
        let marked = input.marked_range.clone();
        let font = window.text_style().font();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let id = window.request_measured_layout(style, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                gpui::AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let lines = shape(
                window,
                text.clone(),
                color,
                marked.clone(),
                font.clone(),
                width,
            );
            let content: Pixels = lines
                .iter()
                .fold(px(0.), |acc, l| acc + l.size(LINE_HEIGHT).height);
            size(
                width.unwrap_or_default(),
                content.min(LINE_HEIGHT * MAX_LINES).max(LINE_HEIGHT),
            )
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let input = self.input.read(cx);
        let theme = Theme::get(cx);
        let (text, color) = display_text(input, cx);
        let empty = input.model.text().is_empty();
        let font = window.text_style().font();
        let layout = Layout {
            lines: shape(
                window,
                text,
                color,
                input.marked_range.clone(),
                font,
                Some(bounds.size.width),
            ),
            bounds,
        };
        let cursor = input.model.cursor();
        let selection = input.model.selection();

        let max_scroll = (layout.height() - bounds.size.height).max(px(0.));
        let mut scroll_y = input.scroll_y;
        if input.scroll_to_caret {
            let y = layout.point_for_index(cursor).y;
            if y < scroll_y {
                scroll_y = y;
            } else if y + LINE_HEIGHT > scroll_y + bounds.size.height {
                scroll_y = y + LINE_HEIGHT - bounds.size.height;
            }
        }
        let scroll_y = scroll_y.clamp(px(0.), max_scroll);
        let origin = bounds.origin - point(px(0.), scroll_y);

        let mut quads = Vec::new();
        if !empty && !selection.is_empty() {
            let fill_color = theme.accent.opacity(0.25);
            let start = layout.point_for_index(selection.start);
            let end = layout.point_for_index(selection.end);
            let mut y = start.y;
            while y <= end.y {
                let left = if y == start.y { start.x } else { px(0.) };
                let right = if y == end.y { end.x } else { bounds.size.width };
                quads.push(fill(
                    Bounds::new(
                        origin + point(left, y),
                        size((right - left).max(px(4.)), LINE_HEIGHT),
                    ),
                    fill_color,
                ));
                y += LINE_HEIGHT;
            }
        }
        let caret = selection.is_empty().then(|| {
            fill(
                Bounds::new(
                    origin + layout.point_for_index(cursor),
                    size(px(2.), LINE_HEIGHT),
                ),
                theme.accent,
            )
        });
        Prepaint {
            lines: layout.lines,
            selection: quads,
            caret,
            scroll_y,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus_handle.clone();
        let visible = self.input.read(cx).caret_visible;
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in prepaint.selection.drain(..) {
                window.paint_quad(quad);
            }
            let mut origin = bounds.origin - point(px(0.), prepaint.scroll_y);
            for line in &prepaint.lines {
                line.paint(origin, LINE_HEIGHT, gpui::TextAlign::Left, None, window, cx)
                    .ok();
                origin.y += line.size(LINE_HEIGHT).height;
            }
            if focus.is_focused(window)
                && visible
                && let Some(caret) = prepaint.caret.take()
            {
                window.paint_quad(caret);
            }
        });
        let lines = std::mem::take(&mut prepaint.lines);
        let scroll_y = prepaint.scroll_y;
        self.input.update(cx, |input, _| {
            input.layout = Layout { lines, bounds };
            input.scroll_y = scroll_y;
            input.scroll_to_caret = false;
        });
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.blink_task.is_none() || self.blink_reset {
            self.restart_blink(window, cx);
        }
        let theme = Theme::get(cx);
        let focused = self.focus_handle.is_focused(window);
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
                        .bottom_full()
                        .mb(px(6.))
                        .left_0()
                        .w(px(340.))
                        .bg(theme.control_bg)
                        .border_1()
                        .border_color(theme.separator)
                        .rounded(px(8.))
                        .shadow_lg()
                        .p_1()
                        .children(mentions.into_iter().enumerate().map(|(index, mention)| {
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .px_3()
                                .py_1()
                                .rounded(px(6.))
                                .when(index == selected, |row| row.bg(theme.selection))
                                .child(
                                    div()
                                        .text_size(FONT_SIZE)
                                        .text_color(theme.text)
                                        .child(format!("@{}", mention.name)),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(theme.text_secondary)
                                        .child(mention.role),
                                )
                        }))
                }),
            )
            .child(
                div()
                    .id("composer")
                    .key_context("Composer")
                    .track_focus(&self.focus_handle)
                    .cursor(CursorStyle::IBeam)
                    .on_action(cx.listener(Self::enter))
                    .on_action(cx.listener(Self::newline))
                    .on_action(cx.listener(Self::escape))
                    .on_action(cx.listener(Self::tab))
                    .on_action(cx.listener(Self::up))
                    .on_action(cx.listener(Self::down))
                    .on_action(cx.listener(Self::left))
                    .on_action(cx.listener(Self::right))
                    .on_action(cx.listener(Self::select_left))
                    .on_action(cx.listener(Self::select_right))
                    .on_action(cx.listener(Self::word_left))
                    .on_action(cx.listener(Self::word_right))
                    .on_action(cx.listener(Self::select_word_left))
                    .on_action(cx.listener(Self::select_word_right))
                    .on_action(cx.listener(Self::line_start))
                    .on_action(cx.listener(Self::line_end))
                    .on_action(cx.listener(Self::select_line_start))
                    .on_action(cx.listener(Self::select_line_end))
                    .on_action(cx.listener(Self::select_all))
                    .on_action(cx.listener(Self::backspace))
                    .on_action(cx.listener(Self::delete))
                    .on_action(cx.listener(Self::delete_to_line_start))
                    .on_action(cx.listener(Self::copy))
                    .on_action(cx.listener(Self::cut))
                    .on_action(cx.listener(Self::paste))
                    .on_action(cx.listener(Self::show_character_palette))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_scroll_wheel(cx.listener(Self::on_scroll))
                    .w_full()
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if focused {
                        theme.accent
                    } else {
                        theme.separator
                    })
                    .bg(theme.control_bg)
                    .text_size(FONT_SIZE)
                    .line_height(LINE_HEIGHT)
                    .text_color(theme.text)
                    .child(TextElement { input: cx.entity() }),
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

    #[test]
    fn selection_replace_and_collapse() {
        let mut model = ComposerModel::new("hello world", vec![]);
        model.set_selection(0, 5);
        assert_eq!(model.selection(), 0..5);
        model.insert("bye");
        assert_eq!(model.text(), "bye world");
        assert_eq!(model.selection(), 3..3);
        model.set_selection(4, 9);
        model.step(false, false);
        assert_eq!(model.cursor(), 4);
        model.set_selection(4, 9);
        model.step(true, false);
        assert_eq!(model.cursor(), 9);
        model.step(true, true);
        assert_eq!(model.selection(), 9..9);
    }

    #[test]
    fn backspace_and_delete_respect_multibyte_chars() {
        let mut model = ComposerModel::new("aé", vec![]);
        model.backspace();
        assert_eq!(model.text(), "a");
        model.move_to(0, false);
        model.delete_forward();
        assert_eq!(model.text(), "");
    }

    #[test]
    fn word_and_line_motion() {
        let model = ComposerModel::new("foo bar\nbaz  qux", vec![]);
        assert_eq!(model.word_left(7), 4);
        assert_eq!(model.word_left(4), 0);
        assert_eq!(model.word_right(0), 3);
        assert_eq!(model.word_right(3), 7);
        assert_eq!(model.line_start(10), 8);
        assert_eq!(model.line_end(2), 7);
        assert_eq!(model.line_end(10), 16);
    }

    #[test]
    fn delete_to_line_start_stays_on_line() {
        let mut model = ComposerModel::new("one\ntwo", vec![]);
        model.delete_to_line_start();
        assert_eq!(model.text(), "one\n");
        model.delete_to_line_start();
        assert_eq!(model.text(), "one");
    }

    #[test]
    fn editing_before_mention_closes_palette() {
        let mut model = ComposerModel::new("x @", candidates());
        model.move_to(3, false);
        model.insert("@");
        assert!(model.palette_open());
        model.replace_range(0..1, "");
        assert!(!model.palette_open());
    }
}
