//! Keyboard-first controls shared across the app. Anything focusable joins the window's tab
//! order, so Tab / Shift-Tab move focus, Enter or Space presses the focused button, and
//! Escape cancels the dialog that contains focus.

use std::rc::Rc;

use gpui::{
    App, Div, ElementId, FocusHandle, FontWeight, KeyBinding, SharedString, Stateful, Window,
    actions, div, prelude::*,
};

use crate::zoom::px;

use crate::Theme;

actions!(toma, [FocusNext, FocusPrevious, Confirm, Cancel]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrevious, None),
        KeyBinding::new("enter", Confirm, Some("Button")),
        KeyBinding::new("space", Confirm, Some("Button")),
        KeyBinding::new("escape", Cancel, Some("Dialog")),
    ]);
}

/// Handles Tab / Shift-Tab for everything inside the element that tracks it.
pub fn focus_navigation<E: InteractiveElement>(element: E) -> E {
    element
        .on_action(|_: &FocusNext, window, _| window.focus_next())
        .on_action(|_: &FocusPrevious, window, _| window.focus_prev())
}

#[derive(Clone, Copy, PartialEq)]
pub enum ButtonStyle {
    Primary,
    Secondary,
}

pub type OnPress = Rc<dyn Fn(&mut Window, &mut App)>;

/// A push button that is clickable and keyboard-operable, with a macOS-style focus ring.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    focus: &FocusHandle,
    style: ButtonStyle,
    theme: &Theme,
    on_press: OnPress,
) -> Stateful<Div> {
    let ring = theme.accent.opacity(0.5);
    let pressed = Rc::clone(&on_press);
    div()
        .id(id)
        .key_context("Button")
        .track_focus(focus)
        .px_3()
        .py(px(3.))
        .rounded(px(6.))
        .border_2()
        .border_color(gpui::transparent_black())
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .map(|button| match style {
            ButtonStyle::Primary => button.bg(theme.accent).text_color(gpui::white()),
            ButtonStyle::Secondary => button.bg(theme.selection).text_color(theme.text),
        })
        .hover(|button| button.opacity(0.85))
        .focus(move |button| button.border_color(ring))
        .on_click(move |_, window, cx| on_press(window, cx))
        .on_action(move |_: &Confirm, window, cx| pressed(window, cx))
        .child(label.into())
}
