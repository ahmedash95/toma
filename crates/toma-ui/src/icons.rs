use gpui::{AssetSource, Hsla, Result, SharedString, Styled, Svg, svg};
use std::borrow::Cow;

/// Declares the icon set: each variant is a vendored SVG file served to GPUI's `svg()`.
macro_rules! icons {
    ($($name:ident => $file:literal,)*) => {
        #[derive(Clone, Copy, PartialEq)]
        pub enum Icon { $($name,)* }

        const ALL: &[(Icon, &str, &[u8])] =
            &[$((Icon::$name, $file, include_bytes!(concat!("../assets/", $file))),)*];
    };
}

// Line icons from Lucide (ISC, assets/lucide/LICENSE); brand marks from Simple Icons (CC0).
icons! {
    Hash => "lucide/hash.svg",
    Folder => "lucide/folder.svg",
    Plus => "lucide/plus.svg",
    Close => "lucide/x.svg",
    Replies => "lucide/message-circle.svg",
    Clock => "lucide/clock.svg",
    Tokens => "lucide/coins.svg",
    Shield => "lucide/shield-check.svg",
    Check => "lucide/check.svg",
    Copy => "lucide/copy.svg",
    Ban => "lucide/ban.svg",
    Hourglass => "lucide/hourglass.svg",
    Eye => "lucide/eye.svg",
    Zap => "lucide/zap.svg",
    Hand => "lucide/hand.svg",
    Alert => "lucide/octagon-alert.svg",
    CircleCheck => "lucide/circle-check.svg",
    CircleX => "lucide/circle-x.svg",
    Plan => "lucide/clipboard-list.svg",
    Sidebar => "lucide/panel-left.svg",
    Back => "lucide/arrow-left.svg",
    Forward => "lucide/arrow-right.svg",
    Reload => "lucide/rotate-cw.svg",
    External => "lucide/external-link.svg",
    Globe => "lucide/globe.svg",
    PanelRight => "lucide/panel-right.svg",
    PanelRightClose => "lucide/panel-right-close.svg",
    Claude => "logos/claude.svg",
    Cursor => "logos/cursor.svg",
    OpenAi => "logos/openai.svg",
}

impl Icon {
    fn path(self) -> SharedString {
        ALL.iter()
            .find(|(icon, _, _)| *icon == self)
            .map(|(_, file, _)| SharedString::from(*file))
            .expect("every icon is listed")
    }

    /// SVGs do not inherit the text color, so every icon states its own.
    pub fn view(self, color: Hsla) -> Svg {
        svg().path(self.path()).flex_none().text_color(color)
    }
}

/// Serves the icons to GPUI's `svg()` element.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ALL
            .iter()
            .find(|(_, file, _)| *file == path)
            .map(|(_, _, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(ALL.iter().map(|(_, file, _)| (*file).into()).collect())
    }
}
