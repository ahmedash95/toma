//! Markdown for chat messages: `parse` (pure) into a small block model, then `render_markdown`.

use std::ops::Range;
use std::rc::Rc;

use std::cell::RefCell;
use std::time::Duration;

use gpui::{
    AnyElement, App, ClipboardItem, ElementId, FontStyle, FontWeight, Hsla, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement, Styled, TextRun, UnderlineStyle,
    Window, div, font, prelude::FluentBuilder,
};

use crate::zoom::px;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::Theme;
use crate::icons::Icon;
use crate::selectable::Doc;

#[derive(Clone, Debug, Default, PartialEq)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    mention: bool,
    link: Option<String>,
}

/// Plain text plus non-overlapping, sorted style spans (unstyled gaps are implicit).
#[derive(Clone, Debug, Default, PartialEq)]
struct Inline {
    text: String,
    spans: Vec<(Range<usize>, Style)>,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::enum_variant_names)]
enum Block {
    Paragraph(Inline),
    Heading(u8, Inline),
    CodeBlock {
        lang: Option<String>,
        text: String,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<Vec<Block>>,
    },
    Quote(Vec<Block>),
    Rule,
    /// First row is the header.
    Table {
        rows: Vec<Vec<Inline>>,
    },
}

type Events<'a> = std::iter::Peekable<Parser<'a>>;

fn parse(source: &str) -> Vec<Block> {
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    blocks(&mut Parser::new_ext(source, opts).peekable())
}

/// Reads blocks until the enclosing container's `End` (consumed) or end of input.
fn blocks(ev: &mut Events) -> Vec<Block> {
    let mut out = Vec::new();
    while let Some(e) = ev.peek() {
        match e {
            Event::End(_) => {
                ev.next();
                break;
            }
            // Tight list items carry inline events with no paragraph wrapper.
            Event::Text(_)
            | Event::Code(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::Start(Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. }) => {
                out.push(Block::Paragraph(inline(ev)));
            }
            _ => match ev.next() {
                Some(Event::Start(Tag::Paragraph)) => out.push(Block::Paragraph(inline(ev))),
                Some(Event::Start(Tag::Heading { level, .. })) => {
                    let n = match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        _ => 3,
                    };
                    out.push(Block::Heading(n, inline(ev)));
                }
                Some(Event::Start(Tag::BlockQuote(_))) => out.push(Block::Quote(blocks(ev))),
                Some(Event::Start(Tag::CodeBlock(kind))) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(l) if !l.is_empty() => Some(l.to_string()),
                        _ => None,
                    };
                    let mut text = String::new();
                    while let Some(Event::Text(t)) = ev.peek() {
                        text.push_str(t);
                        ev.next();
                    }
                    ev.next(); // End(CodeBlock)
                    out.push(Block::CodeBlock { lang, text });
                }
                Some(Event::Start(Tag::List(start))) => {
                    let mut items = Vec::new();
                    while let Some(Event::Start(Tag::Item)) = ev.peek() {
                        ev.next();
                        items.push(blocks(ev));
                    }
                    ev.next(); // End(List)
                    out.push(Block::List {
                        ordered: start.is_some(),
                        start: start.unwrap_or(1),
                        items,
                    });
                }
                Some(Event::Start(Tag::Table(_))) => {
                    let mut rows: Vec<Vec<Inline>> = Vec::new();
                    while let Some(e) = ev.next() {
                        match e {
                            Event::Start(Tag::TableHead | Tag::TableRow) => rows.push(Vec::new()),
                            Event::Start(Tag::TableCell) => {
                                let cell = inline(ev);
                                if let Some(row) = rows.last_mut() {
                                    row.push(cell);
                                }
                            }
                            Event::End(TagEnd::Table) => break,
                            _ => {}
                        }
                    }
                    out.push(Block::Table { rows });
                }
                Some(Event::Rule) => out.push(Block::Rule),
                _ => {}
            },
        }
    }
    out
}

/// Reads inline events until a block-level event or the closing `End` of the container
/// (consumed only when it closes a paragraph/heading/cell).
fn inline(ev: &mut Events) -> Inline {
    let mut out = Inline::default();
    let mut st = Style::default();
    let mut depth = 0;
    let push = |out: &mut Inline, t: &str, st: &Style| {
        let start = out.text.len();
        out.text.push_str(t);
        if *st != Style::default() && !t.is_empty() {
            out.spans.push((start..out.text.len(), st.clone()));
        }
    };
    while let Some(e) = ev.peek() {
        match e {
            Event::Text(t) => push(&mut out, t, &st),
            Event::Code(t) => push(
                &mut out,
                t,
                &Style {
                    code: true,
                    ..st.clone()
                },
            ),
            Event::SoftBreak => push(&mut out, " ", &Style::default()),
            Event::HardBreak => push(&mut out, "\n", &Style::default()),
            Event::Start(
                tag @ (Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. }),
            ) => {
                depth += 1;
                match tag {
                    Tag::Emphasis => st.italic = true,
                    Tag::Strong => st.bold = true,
                    Tag::Strikethrough => st.strike = true,
                    Tag::Link { dest_url, .. } => st.link = Some(dest_url.to_string()),
                    _ => {}
                }
            }
            Event::End(
                end @ (TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link),
            ) if depth > 0 => {
                depth -= 1;
                match end {
                    TagEnd::Emphasis => st.italic = false,
                    TagEnd::Strong => st.bold = false,
                    TagEnd::Strikethrough => st.strike = false,
                    _ => st.link = None,
                }
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::TableCell) => {
                ev.next();
                break;
            }
            _ => break,
        }
        ev.next();
    }
    out
}

/// Byte ranges of `@name` mentions of `names` in `text`, bounded by non-word characters.
fn mention_ranges(text: &str, names: &[&str]) -> Vec<Range<usize>> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices('@')
        .filter(|(at, _)| !text[..*at].chars().next_back().is_some_and(word))
        .filter_map(|(at, _)| {
            let rest = &text[at + 1..];
            names
                .iter()
                .filter(|n| {
                    !n.is_empty()
                        && rest.starts_with(**n)
                        && !rest[n.len()..].chars().next().is_some_and(word)
                })
                .map(|n| at..at + 1 + n.len())
                .max_by_key(|r| r.len())
        })
        .collect()
}

/// Styles `@name` mentions outside code and links.
fn mark_mentions(i: &mut Inline, names: &[&str]) {
    let mentions: Vec<_> = mention_ranges(&i.text, names)
        .into_iter()
        .filter(|m| {
            !i.spans
                .iter()
                .any(|(r, s)| (s.code || s.link.is_some()) && r.start < m.end && m.start < r.end)
        })
        .collect();
    if mentions.is_empty() {
        return;
    }
    let mut cuts: Vec<usize> = i
        .spans
        .iter()
        .map(|(r, _)| r)
        .chain(&mentions)
        .flat_map(|r| [r.start, r.end])
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    i.spans = cuts
        .windows(2)
        .filter_map(|w| {
            let seg = w[0]..w[1];
            let inside = |r: &Range<usize>| r.start <= seg.start && seg.end <= r.end;
            let mut st = i
                .spans
                .iter()
                .find(|(r, _)| inside(r))
                .map(|(_, s)| s.clone())
                .unwrap_or_default();
            st.mention = mentions.iter().any(inside);
            (st != Style::default()).then_some((seg, st))
        })
        .collect();
}

fn mark_all(bs: &mut [Block], names: &[&str]) {
    for b in bs {
        match b {
            Block::Paragraph(i) | Block::Heading(_, i) => mark_mentions(i, names),
            Block::List { items, .. } => items.iter_mut().for_each(|it| mark_all(it, names)),
            Block::Quote(inner) => mark_all(inner, names),
            Block::Table { rows } => rows
                .iter_mut()
                .flatten()
                .for_each(|c| mark_mentions(c, names)),
            Block::CodeBlock { .. } | Block::Rule => {}
        }
    }
}

// ---- rendering ----

/// Called with a clicked link's URL; the host decides where it opens.
pub type OnLink = Rc<dyn Fn(&str, &mut Window, &mut App)>;

thread_local! {
    /// The code block whose copy button was just pressed, shown as copied for a moment.
    static COPIED: RefCell<Option<ElementId>> = const { RefCell::new(None) };
}

/// Renders Markdown `source` as GPUI elements, highlighting `@name` for each of `mentions`.
/// `id` must be unique among siblings.
pub fn render_markdown(
    id: impl Into<ElementId>,
    source: &str,
    mentions: &[&str],
    theme: &Theme,
    on_link: OnLink,
) -> AnyElement {
    let doc = Doc::new(id.into(), on_link);
    let mut blocks = parse(source);
    mark_all(&mut blocks, mentions);
    div()
        .w_full()
        .min_w(px(0.))
        .text_size(px(13.))
        .line_height(px(20.))
        .text_color(theme.text)
        .child(block_list(&doc, &doc.id, &blocks, theme, theme.text))
        .into_any_element()
}

fn sub(id: &ElementId, i: usize) -> ElementId {
    ElementId::from((id.clone(), format!("{i}")))
}

fn font_w(family: &'static str, weight: FontWeight) -> gpui::Font {
    let mut f = font(family);
    f.weight = weight;
    f
}

fn block_list(doc: &Doc, id: &ElementId, bs: &[Block], t: &Theme, color: Hsla) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .w_full()
        .min_w(px(0.))
        .children(
            bs.iter()
                .enumerate()
                .map(|(i, b)| block(doc, sub(id, i), b, t, color)),
        )
        .into_any_element()
}

fn block(doc: &Doc, id: ElementId, b: &Block, t: &Theme, color: Hsla) -> AnyElement {
    match b {
        Block::Paragraph(i) => text(doc, i, t, color, FontWeight::NORMAL),
        Block::Heading(n, i) => {
            let size = match n {
                1 => 18.,
                2 => 16.,
                _ => 14.,
            };
            div()
                .mt(px(4.))
                .text_size(px(size))
                .child(text(doc, i, t, color, FontWeight::SEMIBOLD))
                .into_any_element()
        }
        Block::CodeBlock { text, .. } => {
            let code = text.trim_end_matches('\n').to_string();
            div()
                .group("code")
                .relative()
                .w_full()
                .min_w(px(0.))
                .p(px(8.))
                .pr(px(32.))
                .rounded(px(6.))
                .bg(t.code_bg)
                .font_family("Menlo")
                .text_size(px(12.))
                .line_height(px(18.))
                .child(doc.leaf(code.clone(), None, Vec::new()))
                .child(copy_button(sub(&id, 0), code, t))
                .into_any_element()
        }
        Block::List {
            ordered,
            start,
            items,
        } => div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .w_full()
            .min_w(px(0.))
            .children(items.iter().enumerate().map(|(n, item)| {
                let marker = if *ordered {
                    format!("{}.", start + n as u64)
                } else {
                    "•".into()
                };
                div()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .w(px(22.))
                            .flex_none()
                            .text_color(t.text_secondary)
                            .child(marker),
                    )
                    .child(div().flex_1().min_w(px(0.)).child(block_list(
                        doc,
                        &sub(&id, n),
                        item,
                        t,
                        color,
                    )))
            }))
            .into_any_element(),
        Block::Quote(inner) => div()
            .flex()
            .flex_row()
            .w_full()
            .min_w(px(0.))
            .child(div().w(px(3.)).flex_none().rounded(px(1.5)).bg(t.separator))
            .child(div().flex_1().min_w(px(0.)).pl(px(8.)).child(block_list(
                doc,
                &id,
                inner,
                t,
                t.text_secondary,
            )))
            .into_any_element(),
        Block::Rule => div()
            .w_full()
            .h(px(1.))
            .my(px(4.))
            .bg(t.separator)
            .into_any_element(),
        Block::Table { rows } => div()
            .w_full()
            .min_w(px(0.))
            .border_1()
            .border_color(t.separator)
            .children(rows.iter().enumerate().map(|(r, row)| {
                let weight = if r == 0 {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                };
                let mut line = div().flex().flex_row().border_color(t.separator);
                if r + 1 < rows.len() {
                    line = line.border_b_1();
                }
                line.children(row.iter().map(|cell| {
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .px(px(6.))
                        .py(px(3.))
                        .child(text(doc, cell, t, color, weight))
                }))
            }))
            .into_any_element(),
    }
}

/// Copies `code` to the clipboard; appears when the code block is hovered.
fn copy_button(id: ElementId, code: String, t: &Theme) -> impl IntoElement {
    let copied = COPIED.with_borrow(|c| c.as_ref() == Some(&id));
    let (icon, color) = if copied {
        (Icon::Check, t.green)
    } else {
        (Icon::Copy, t.text_secondary)
    };
    div()
        .id(id.clone())
        .absolute()
        .top(px(6.))
        .right(px(6.))
        .p(px(4.))
        .rounded(px(4.))
        .cursor_pointer()
        .occlude()
        .when(!copied, |d| {
            d.opacity(0.).group_hover("code", |s| s.opacity(1.))
        })
        .hover(|s| s.bg(t.hover))
        .on_click(move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
            COPIED.set(Some(id.clone()));
            window.refresh();
            let id = id.clone();
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(1500))
                    .await;
                if COPIED.with_borrow(|c| c.as_ref() == Some(&id)) {
                    COPIED.set(None);
                    cx.update(|cx| cx.refresh_windows()).ok();
                }
            })
            .detach();
        })
        .child(icon.view(color).size(px(14.)))
}

/// Styled inline text; link ranges become clickable.
fn text(doc: &Doc, i: &Inline, t: &Theme, color: Hsla, weight: FontWeight) -> AnyElement {
    let base = |len| TextRun {
        len,
        font: font_w(".SystemUIFont", weight),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let (mut runs, mut pos) = (Vec::new(), 0);
    let mut links = Vec::new();
    for (r, s) in &i.spans {
        if r.start > pos {
            runs.push(base(r.start - pos));
        }
        let mut run = base(r.len());
        if s.code {
            run.font = font_w("Menlo", weight);
            run.background_color = Some(t.code_bg);
        }
        if s.bold {
            run.font.weight = FontWeight::BOLD;
        }
        if s.italic {
            run.font.style = FontStyle::Italic;
        }
        if s.strike {
            run.strikethrough = Some(Default::default());
        }
        if s.mention {
            run.color = t.accent;
            run.font.weight = FontWeight::SEMIBOLD;
            run.background_color = Some(t.accent.opacity(if t.dark { 0.25 } else { 0.12 }));
        }
        if let Some(url) = &s.link {
            run.color = t.accent;
            run.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(t.accent),
                wavy: false,
            });
            links.push((r.clone(), url.clone()));
        }
        runs.push(run);
        pos = r.end;
    }
    if pos < i.text.len() {
        runs.push(base(i.text.len() - pos));
    }
    div()
        .w_full()
        .min_w(px(0.))
        .child(doc.leaf(i.text.clone(), Some(runs), links))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(src: &str) -> Inline {
        match parse(src).remove(0) {
            Block::Paragraph(i) => i,
            b => panic!("{b:?}"),
        }
    }

    #[test]
    fn inline_spans() {
        let i = para("a **b** *c* `d` ~~e~~");
        assert_eq!(i.text, "a b c d e");
        let flags: Vec<_> = i
            .spans
            .iter()
            .map(|(r, s)| (&i.text[r.clone()], s.bold, s.italic, s.code, s.strike))
            .collect();
        assert_eq!(
            flags,
            [
                ("b", true, false, false, false),
                ("c", false, true, false, false),
                ("d", false, false, true, false),
                ("e", false, false, false, true)
            ]
        );
    }

    #[test]
    fn fenced_code() {
        let b = parse("```rust\nfn main() {}\n```");
        assert_eq!(
            b,
            [Block::CodeBlock {
                lang: Some("rust".into()),
                text: "fn main() {}\n".into()
            }]
        );
    }

    #[test]
    fn nested_list() {
        let b = parse("1. one\n   - inner\n2. two");
        let Block::List {
            ordered: true,
            start: 1,
            items,
        } = &b[0]
        else {
            panic!("{b:?}")
        };
        assert_eq!(items.len(), 2);
        assert!(
            matches!(&items[0][1], Block::List { ordered: false, items, .. } if items.len() == 1)
        );
    }

    #[test]
    fn link_url() {
        let i = para("see [docs](https://x.dev/a) now");
        assert_eq!(i.spans[0].1.link.as_deref(), Some("https://x.dev/a"));
        assert_eq!(&i.text[i.spans[0].0.clone()], "docs");
    }

    #[test]
    fn mentions() {
        let mut i = para("hi @Claude and **@Codex**, not `@Claude`, a@Claude, @Claudette");
        mark_mentions(&mut i, &["Claude", "Codex"]);
        let marked: Vec<_> = i
            .spans
            .iter()
            .filter(|(_, s)| s.mention)
            .map(|(r, s)| (&i.text[r.clone()], s.bold))
            .collect();
        assert_eq!(marked, [("@Claude", false), ("@Codex", true)]);
    }

    #[test]
    fn table_rows() {
        let b = parse("|a|b|\n|-|-|\n|1|2|\n|3|4|");
        let Block::Table { rows } = &b[0] else {
            panic!("{b:?}")
        };
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2][1].text, "4");
    }
}
