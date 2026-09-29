//! Browser tabs for the inspector. Each tab owns one native WebView (WKWebView on macOS) that
//! lives until the tab closes, so switching tabs or threads only hides and shows views: pages,
//! history, scroll and form state survive going back and forth.

use std::rc::Rc;

use futures::channel::mpsc::UnboundedSender;
use gpui::{
    App, AppContext, Bounds, ContentMask, Element, ElementId, Entity, GlobalElementId, Hitbox,
    HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseDownEvent, Pixels, Size, Style,
    Window,
};
use wry::{
    NewWindowResponse, PageLoadEvent, Rect, WebView, WebViewBuilder,
    dpi::{LogicalPosition, LogicalSize},
};

use crate::composer::Composer;
use crate::inspector::TabId;

/// Page changes reported by a WebView, delivered to the shell on the UI thread.
pub enum BrowserEvent {
    Loading {
        tab: TabId,
        url: String,
        loading: bool,
    },
    Title {
        tab: TabId,
        title: String,
    },
    /// `target=_blank` and `window.open` open a new tab instead of a new window.
    NewTab {
        tab: TabId,
        url: String,
    },
}

pub struct Browser {
    webview: Rc<WebView>,
    pub address: Entity<Composer>,
    pub url: String,
    pub title: String,
    pub loading: bool,
    shown: bool,
}

impl Browser {
    pub fn new(
        tab: TabId,
        url: &str,
        events: UnboundedSender<BrowserEvent>,
        window: &Window,
        cx: &mut App,
    ) -> wry::Result<Self> {
        let (load, title, open) = (events.clone(), events.clone(), events);
        let webview = WebViewBuilder::new()
            .with_url(url)
            .with_visible(false)
            .with_bounds(Rect::default())
            .with_devtools(true)
            .with_on_page_load_handler(move |event, url| {
                let loading = matches!(event, PageLoadEvent::Started);
                let _ = load.unbounded_send(BrowserEvent::Loading { tab, url, loading });
            })
            .with_document_title_changed_handler(move |value| {
                let _ = title.unbounded_send(BrowserEvent::Title { tab, title: value });
            })
            .with_new_window_req_handler(move |url, _| {
                let _ = open.unbounded_send(BrowserEvent::NewTab { tab, url });
                NewWindowResponse::Deny
            })
            .build_as_child(window)?;
        let address = cx.new(|cx| {
            Composer::new(
                shown_address(url),
                Vec::new(),
                "Search or enter address",
                cx,
            )
            .compact()
        });
        Ok(Self {
            webview: Rc::new(webview),
            address,
            url: url.to_owned(),
            title: String::new(),
            loading: true,
            shown: false,
        })
    }

    pub fn load(&mut self, url: &str, cx: &mut App) {
        let _ = self.webview.load_url(url);
        self.set_url(url, cx);
    }

    pub fn set_url(&mut self, url: &str, cx: &mut App) {
        self.url = url.to_owned();
        self.address
            .update(cx, |address, cx| address.set_text(shown_address(url), cx));
    }

    pub fn back(&self) {
        let _ = self.webview.go_back();
    }

    pub fn forward(&self) {
        let _ = self.webview.go_forward();
    }

    pub fn reload(&self) {
        let _ = self.webview.reload();
    }

    /// The native view floats above GPUI content, so it must be hidden whenever its slot is.
    pub fn set_shown(&mut self, shown: bool) {
        if self.shown != shown {
            if !shown {
                let _ = self.webview.focus_parent();
            }
            let _ = self.webview.set_visible(shown);
            self.shown = shown;
        }
    }

    /// Takes the element's layout slot and keeps the native view's frame on top of it.
    pub fn view(&self) -> BrowserElement {
        BrowserElement(self.webview.clone())
    }
}

/// A blank page shows an empty address bar, ready for typing.
fn shown_address(url: &str) -> &str {
    if url == "about:blank" { "" } else { url }
}

/// Only web pages open in-app; `mailto:`, `file:` and friends go to the system.
pub fn opens_in_app(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Turns address bar input into a URL: explicit URLs load as typed, bare hosts get a scheme,
/// anything else becomes a web search.
pub fn normalize_address(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if input.contains("://") || input.starts_with("about:") {
        return Some(input.to_owned());
    }
    let host = input.split(['/', '?', '#']).next().unwrap_or_default();
    let host_name = host.split(':').next().unwrap_or_default();
    let local = host_name == "localhost"
        || host_name.split('.').count() == 4
            && host_name.split('.').all(|p| p.parse::<u8>().is_ok());
    if local {
        return Some(format!("http://{input}"));
    }
    if !input.contains(char::is_whitespace) && host_name.contains('.') {
        return Some(format!("https://{input}"));
    }
    let query: String = input
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_owned(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    Some(format!("https://www.google.com/search?q={query}"))
}

pub struct BrowserElement(Rc<WebView>);

impl IntoElement for BrowserElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for BrowserElement {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            size: Size::full(),
            flex_grow: 1.,
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> Hitbox {
        let _ = self.0.set_bounds(Rect {
            position: LogicalPosition::new(f32::from(bounds.origin.x), f32::from(bounds.origin.y))
                .into(),
            size: LogicalSize::new(f32::from(bounds.size.width), f32::from(bounds.size.height))
                .into(),
        });
        window.insert_hitbox(bounds, HitboxBehavior::BlockMouse)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut Hitbox,
        window: &mut Window,
        _: &mut App,
    ) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let webview = self.0.clone();
            // Clicking back into GPUI returns keyboard focus from the page to the app.
            window.on_mouse_event(move |event: &MouseDownEvent, _, _, _| {
                if !bounds.contains(&event.position) {
                    let _ = webview.focus_parent();
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_become_urls_or_searches() {
        let n = |s| normalize_address(s);
        assert_eq!(n("  "), None);
        assert_eq!(n("https://x.dev/a").as_deref(), Some("https://x.dev/a"));
        assert_eq!(n("x.dev/a?b=1").as_deref(), Some("https://x.dev/a?b=1"));
        assert_eq!(
            n("localhost:3000/app").as_deref(),
            Some("http://localhost:3000/app")
        );
        assert_eq!(
            n("127.0.0.1:8080").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(
            n("rust gpui & wry").as_deref(),
            Some("https://www.google.com/search?q=rust+gpui+%26+wry")
        );
    }

    #[test]
    fn only_web_links_open_in_app() {
        assert!(opens_in_app("https://x.dev"));
        assert!(opens_in_app("HTTP://x.dev"));
        assert!(!opens_in_app("mailto:a@b.c"));
        assert!(!opens_in_app("file:///tmp/a"));
    }
}
