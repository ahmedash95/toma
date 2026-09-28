use gpui::{AssetSource, Hsla, Result, SharedString, Styled, Svg, svg};
use std::borrow::Cow;

/// Line icons drawn on a 24px grid (Lucide style); GPUI tints them with the text color.
#[derive(Clone, Copy)]
pub enum Icon {
    Hash,
    Folder,
    Plus,
    Close,
    Replies,
    Clock,
    Tokens,
    Shield,
    Check,
    Ban,
}

impl Icon {
    fn body(self) -> &'static str {
        match self {
            Icon::Hash => r#"<path d="M4 9h16M4 15h16M10 3 8 21M16 3l-2 18"/>"#,
            Icon::Folder => {
                r#"<path d="M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2.5h8.5A1.5 1.5 0 0 1 21 9v9.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z"/>"#
            }
            Icon::Plus => r#"<path d="M12 5v14M5 12h14"/>"#,
            Icon::Close => r#"<path d="M6 6l12 12M18 6 6 18"/>"#,
            Icon::Replies => {
                r#"<path d="M21 11.5a8.4 8.4 0 0 1-12.2 7.5L3 21l2-5.2A8.5 8.5 0 1 1 21 11.5z"/>"#
            }
            Icon::Clock => r#"<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>"#,
            Icon::Tokens => {
                r#"<ellipse cx="12" cy="6" rx="7" ry="3"/><path d="M5 6v6c0 1.7 3.1 3 7 3s7-1.3 7-3V6M5 12v6c0 1.7 3.1 3 7 3s7-1.3 7-3v-6"/>"#
            }
            Icon::Shield => {
                r#"<path d="M12 3 5 6v5c0 4.5 3 8.3 7 10 4-1.7 7-5.5 7-10V6z"/><path d="m9 12 2 2 4-4"/>"#
            }
            Icon::Check => r#"<path d="m5 12.5 4.5 4.5L19 7"/>"#,
            Icon::Ban => r#"<circle cx="12" cy="12" r="9"/><path d="m5.6 5.6 12.8 12.8"/>"#,
        }
    }

    fn path(self) -> SharedString {
        format!("icons/{}.svg", self as u8).into()
    }

    /// SVGs do not inherit the text color, so every icon states its own.
    pub fn view(self, color: Hsla) -> Svg {
        svg().path(self.path()).flex_none().text_color(color)
    }
}

const ALL: [Icon; 10] = [
    Icon::Hash,
    Icon::Folder,
    Icon::Plus,
    Icon::Close,
    Icon::Replies,
    Icon::Clock,
    Icon::Tokens,
    Icon::Shield,
    Icon::Check,
    Icon::Ban,
];

/// Serves the icons to GPUI's `svg()` element.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ALL.iter().find(|icon| icon.path() == path).map(|icon| {
            Cow::Owned(
                format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
                    icon.body()
                )
                .into_bytes(),
            )
        }))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(ALL.iter().map(|icon| icon.path()).collect())
    }
}
