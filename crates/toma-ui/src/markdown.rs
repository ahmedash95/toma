//! Markdown for chat messages: `parse` (pure) into a small block model, then `render_markdown`.

use std::{ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, ElementId, FontStyle, FontWeight, Hsla, InteractiveText, IntoElement,
    ParentElement, SharedString, Styled, StyledText, TextRun, UnderlineStyle, Window, div, font,
};

use crate::zoom::px;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::Theme;

#[derive(Clone, Debug, Default, PartialEq)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
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

// ---- rendering ----

/// Called with a clicked link's URL; the host decides where it opens.
pub type OnLink = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// Renders Markdown `source` as GPUI elements. `id` must be unique among siblings.
pub fn render_markdown(
    id: impl Into<ElementId>,
    source: &str,
    theme: &Theme,
    on_link: OnLink,
) -> AnyElement {
    div()
        .w_full()
        .min_w(px(0.))
        .text_size(px(13.))
        .line_height(px(20.))
        .text_color(theme.text)
        .child(block_list(
            &id.into(),
            &parse(source),
            theme,
            &on_link,
            theme.text,
        ))
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

fn block_list(
    id: &ElementId,
    bs: &[Block],
    t: &Theme,
    on_link: &OnLink,
    color: Hsla,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .w_full()
        .min_w(px(0.))
        .children(
            bs.iter()
                .enumerate()
                .map(|(i, b)| block(sub(id, i), b, t, on_link, color)),
        )
        .into_any_element()
}

fn block(id: ElementId, b: &Block, t: &Theme, on_link: &OnLink, color: Hsla) -> AnyElement {
    match b {
        Block::Paragraph(i) => text(id, i, t, on_link, color, FontWeight::NORMAL),
        Block::Heading(n, i) => {
            let size = match n {
                1 => 18.,
                2 => 16.,
                _ => 14.,
            };
            div()
                .mt(px(4.))
                .text_size(px(size))
                .child(text(id, i, t, on_link, color, FontWeight::SEMIBOLD))
                .into_any_element()
        }
        Block::CodeBlock { text, .. } => div()
            .w_full()
            .min_w(px(0.))
            .p(px(8.))
            .rounded(px(6.))
            .bg(t.code_bg)
            .font_family("Menlo")
            .text_size(px(12.))
            .line_height(px(18.))
            .child(SharedString::from(text.trim_end_matches('\n').to_string()))
            .into_any_element(),
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
                        &sub(&id, n),
                        item,
                        t,
                        on_link,
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
                &id,
                inner,
                t,
                on_link,
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
                let row_id = sub(&id, r);
                let mut line = div().flex().flex_row().border_color(t.separator);
                if r + 1 < rows.len() {
                    line = line.border_b_1();
                }
                line.children(row.iter().enumerate().map(|(c, cell)| {
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .px(px(6.))
                        .py(px(3.))
                        .child(text(sub(&row_id, c), cell, t, on_link, color, weight))
                }))
            }))
            .into_any_element(),
    }
}

/// Styled inline text; link ranges become clickable.
fn text(
    id: ElementId,
    i: &Inline,
    t: &Theme,
    on_link: &OnLink,
    color: Hsla,
    weight: FontWeight,
) -> AnyElement {
    let base = |len| TextRun {
        len,
        font: font_w(".SystemUIFont", weight),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let (mut runs, mut pos) = (Vec::new(), 0);
    let (mut links, mut urls) = (Vec::new(), Vec::new());
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
        if let Some(url) = &s.link {
            run.color = t.accent;
            run.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(t.accent),
                wavy: false,
            });
            links.push(r.clone());
            urls.push(url.clone());
        }
        runs.push(run);
        pos = r.end;
    }
    if pos < i.text.len() {
        runs.push(base(i.text.len() - pos));
    }
    let styled = StyledText::new(i.text.clone()).with_runs(runs);
    let el = div().w_full().min_w(px(0.));
    if links.is_empty() {
        el.child(styled).into_any_element()
    } else {
        let on_link = on_link.clone();
        el.child(InteractiveText::new(id, styled).on_click(
            links,
            move |ix, window, cx: &mut App| {
                on_link(&urls[ix], window, cx);
            },
        ))
        .into_any_element()
    }
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
    fn table_rows() {
        let b = parse("|a|b|\n|-|-|\n|1|2|\n|3|4|");
        let Block::Table { rows } = &b[0] else {
            panic!("{b:?}")
        };
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2][1].text, "4");
    }
}
