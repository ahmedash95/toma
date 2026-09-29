//! Mouse text selection across the text pieces of one rendered message, copied with Cmd+C.
//!
//! A message is a *document* made of numbered *leaves* (paragraphs, cells, code blocks).
//! The selection is two `(leaf, byte offset)` points in one document, kept in a global so
//! every leaf can paint its share of it.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Bounds, ClipboardItem, CursorStyle, DispatchPhase, Element, ElementId, Global,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString, StyledText,
    TextLayout, TextRun, Window, actions, fill, point, size,
};

use crate::Theme;
use crate::markdown::OnLink;

actions!(toma, [CopySelection]);

pub fn init(cx: &mut App) {
    cx.set_global(Selection::default());
    cx.bind_keys([KeyBinding::new("cmd-c", CopySelection, None)]);
    cx.on_action(|_: &CopySelection, cx| {
        if let Some(text) = cx.global::<Selection>().text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    });
}

/// The leaves of one document, collected while it renders.
#[derive(Clone)]
pub struct Doc {
    pub id: ElementId,
    texts: Rc<RefCell<Vec<SharedString>>>,
    on_link: OnLink,
}

impl Doc {
    pub fn new(id: ElementId, on_link: OnLink) -> Self {
        Self {
            id,
            texts: Rc::default(),
            on_link,
        }
    }

    /// Registers the next leaf; `links` are `(range, url)` pairs opened on click.
    pub fn leaf(
        &self,
        text: impl Into<SharedString>,
        runs: Option<Vec<TextRun>>,
        links: Vec<(Range<usize>, String)>,
    ) -> SelectableText {
        let text = text.into();
        let mut texts = self.texts.borrow_mut();
        texts.push(text.clone());
        let styled = StyledText::new(text);
        SelectableText {
            doc: self.clone(),
            leaf: texts.len() - 1,
            text: match runs {
                Some(runs) => styled.with_runs(runs),
                None => styled,
            },
            links,
        }
    }
}

type Point = (usize, usize);

#[derive(Default)]
struct Selection {
    doc: Option<Doc>,
    anchor: Point,
    head: Point,
    dragging: bool,
}

impl Global for Selection {}

impl Selection {
    fn is(&self, doc: &Doc) -> bool {
        self.doc.as_ref().is_some_and(|d| d.id == doc.id)
    }

    fn ordered(&self) -> (Point, Point) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }

    /// The selected byte range inside `leaf`, if any.
    fn range(&self, doc: &Doc, leaf: usize, len: usize) -> Option<Range<usize>> {
        if !self.is(doc) {
            return None;
        }
        let (start, end) = self.ordered();
        if leaf < start.0 || leaf > end.0 {
            return None;
        }
        let from = if leaf == start.0 { start.1 } else { 0 };
        let to = if leaf == end.0 { end.1 } else { len };
        (from < to).then_some(from..to.min(len))
    }

    fn text(&self) -> Option<String> {
        let doc = self.doc.as_ref()?;
        let texts = doc.texts.borrow();
        let (start, end) = self.ordered();
        let pieces: Vec<&str> = (start.0..=end.0)
            .filter_map(|leaf| {
                let text = texts.get(leaf)?;
                text.get(self.range(doc, leaf, text.len())?)
            })
            .collect();
        (!pieces.is_empty()).then(|| pieces.join("\n"))
    }
}

/// A `StyledText` leaf that takes part in its document's selection.
pub struct SelectableText {
    doc: Doc,
    leaf: usize,
    text: StyledText,
    links: Vec<(Range<usize>, String)>,
}

impl IntoElement for SelectableText {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

fn index(layout: &TextLayout, position: gpui::Point<Pixels>) -> usize {
    layout
        .index_for_position(position)
        .unwrap_or_else(|ix| ix)
        .min(layout.len())
}

impl Element for SelectableText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.text.request_layout(None, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.text
            .prepaint(None, inspector_id, bounds, state, window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.text.layout().clone();
        let selected = cx
            .global::<Selection>()
            .range(&self.doc, self.leaf, layout.len());
        if let Some(range) = selected
            && let (Some(start), Some(end)) = (
                layout.position_for_index(range.start),
                layout.position_for_index(range.end),
            )
        {
            let color = Theme::get(cx).accent.opacity(0.25);
            let line = layout.line_height();
            let rows = ((end.y - start.y) / line).round() as usize;
            for row in 0..=rows {
                let left = if row == 0 { start.x } else { bounds.left() };
                let right = if row == rows { end.x } else { bounds.right() };
                let top = start.y + line * row as f32;
                window.paint_quad(fill(
                    Bounds::new(point(left, top), size(right - left, line)),
                    color,
                ));
            }
        }
        self.text
            .paint(None, inspector_id, bounds, state, &mut (), window, cx);

        let over_link = hitbox.is_hovered(window)
            && layout
                .index_for_position(window.mouse_position())
                .is_ok_and(|ix| self.links.iter().any(|(r, _)| r.contains(&ix)));
        let cursor = if over_link {
            CursorStyle::PointingHand
        } else {
            CursorStyle::IBeam
        };
        window.set_cursor_style(cursor, hitbox);

        let (doc, leaf) = (self.doc.clone(), self.leaf);
        window.on_mouse_event({
            let (layout, hitbox, doc) = (layout.clone(), hitbox.clone(), doc.clone());
            move |event: &MouseDownEvent, phase, window, cx| {
                if event.button != MouseButton::Left {
                    return;
                }
                let selection = cx.global_mut::<Selection>();
                let extend = event.modifiers.shift && selection.is(&doc);
                // Any click clears the selection; the clicked leaf then starts a new one.
                if phase == DispatchPhase::Capture {
                    if !extend && selection.doc.take().is_some() {
                        window.refresh();
                    }
                    return;
                }
                if !hitbox.is_hovered(window) {
                    return;
                }
                let at = (leaf, index(&layout, event.position));
                if !extend {
                    selection.anchor = at;
                }
                selection.head = at;
                selection.doc = Some(doc.clone());
                selection.dragging = true;
                // Cmd+C goes to the selection rather than a focused composer.
                window.blur();
                window.refresh();
            }
        });
        window.on_mouse_event({
            let (layout, doc) = (layout.clone(), doc.clone());
            move |event: &MouseMoveEvent, phase, window, cx| {
                let selection = cx.global_mut::<Selection>();
                if phase != DispatchPhase::Bubble
                    || !event.dragging()
                    || !selection.dragging
                    || !selection.is(&doc)
                {
                    return;
                }
                // Match on rows only, so dragging past the text's sides keeps selecting.
                let b = layout.bounds();
                if event.position.y < b.top() || event.position.y >= b.bottom() {
                    return;
                }
                let at = (leaf, index(&layout, event.position));
                if selection.head != at {
                    selection.head = at;
                    window.refresh();
                }
            }
        });
        let links = std::mem::take(&mut self.links);
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            let selection = cx.global_mut::<Selection>();
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !selection.dragging
                || !selection.is(&doc)
            {
                return;
            }
            let click = selection.anchor == selection.head;
            // A plain click selects nothing; the clicked leaf checks it for a link.
            if click && selection.anchor.0 != leaf {
                return;
            }
            selection.dragging = false;
            if click {
                let ix = selection.anchor.1;
                selection.doc = None;
                window.refresh();
                if let Some((_, url)) = links.iter().find(|(r, _)| r.contains(&ix)) {
                    (doc.on_link)(url, window, cx);
                }
            }
        });
    }
}
