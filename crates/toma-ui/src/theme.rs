use gpui::{App, Global, Hsla, WindowAppearance, rgba};

/// macOS system colors for the current appearance. Read with `Theme::get(cx)`.
#[derive(Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub accent: Hsla,
    pub content_bg: Hsla,
    /// Translucent so the blurred window background shows through, like a native sidebar.
    pub sidebar_bg: Hsla,
    pub control_bg: Hsla,
    pub code_bg: Hsla,
    pub separator: Hsla,
    pub selection: Hsla,
    pub hover: Hsla,
    pub green: Hsla,
    pub orange: Hsla,
    pub red: Hsla,
    pub gray: Hsla,
}

impl Global for Theme {}

impl Theme {
    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }

    pub fn get(cx: &App) -> Self {
        *cx.global::<Self>()
    }

    fn light() -> Self {
        Self {
            dark: false,
            text: rgba(0x1d1d1fff).into(),
            text_secondary: rgba(0x6e6e73ff).into(),
            text_tertiary: rgba(0xa1a1a6ff).into(),
            accent: rgba(0x007affff).into(),
            content_bg: rgba(0xffffffff).into(),
            sidebar_bg: rgba(0xececefe0).into(),
            control_bg: rgba(0xffffffff).into(),
            code_bg: rgba(0x0000000d).into(),
            separator: rgba(0x0000001a).into(),
            selection: rgba(0x00000014).into(),
            hover: rgba(0x0000000a).into(),
            green: rgba(0x34c759ff).into(),
            orange: rgba(0xff9500ff).into(),
            red: rgba(0xff3b30ff).into(),
            gray: rgba(0x8e8e93ff).into(),
        }
    }

    fn dark() -> Self {
        Self {
            dark: true,
            text: rgba(0xf5f5f7ff).into(),
            text_secondary: rgba(0x98989dff).into(),
            text_tertiary: rgba(0x6e6e73ff).into(),
            accent: rgba(0x0a84ffff).into(),
            content_bg: rgba(0x1e1e1eff).into(),
            sidebar_bg: rgba(0x262628d9).into(),
            control_bg: rgba(0x2c2c2eff).into(),
            code_bg: rgba(0xffffff14).into(),
            separator: rgba(0xffffff1a).into(),
            selection: rgba(0xffffff1a).into(),
            hover: rgba(0xffffff0d).into(),
            green: rgba(0x30d158ff).into(),
            orange: rgba(0xff9f0aff).into(),
            red: rgba(0xff453aff).into(),
            gray: rgba(0x98989dff).into(),
        }
    }
}
