//! The inspector: a thread's side panel of tabs. Each tab shows one piece of content (a web page
//! today; files and diffs fit the same slot) and stays alive while other tabs or threads are
//! shown, so its state is exactly as left when you come back.

use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{App, KeyBinding, Subscription, actions};

use crate::browser::Browser;
use crate::icons::Icon;

actions!(toma, [ToggleInspector]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-shift-b", ToggleInspector, None)]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabId(u64);

impl TabId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// What a tab displays. New kinds (file, diff) add a variant plus a toolbar and body in the shell.
pub enum TabContent {
    Browser(Browser),
}

pub struct Tab {
    pub id: TabId,
    pub content: TabContent,
    /// Listeners owned by the tab's content, dropped with it.
    pub _subscriptions: Vec<Subscription>,
}

impl Tab {
    pub fn title(&self) -> String {
        match &self.content {
            TabContent::Browser(browser) => page_label(&browser.title, &browser.url),
        }
    }

    pub fn icon(&self) -> Icon {
        match &self.content {
            TabContent::Browser(_) => Icon::Globe,
        }
    }

    pub fn browser(&self) -> Option<&Browser> {
        match &self.content {
            TabContent::Browser(browser) => Some(browser),
        }
    }

    pub fn browser_mut(&mut self) -> Option<&mut Browser> {
        match &mut self.content {
            TabContent::Browser(browser) => Some(browser),
        }
    }

    /// Native content floats above GPUI, so it is hidden whenever its tab is not on screen.
    fn set_shown(&mut self, shown: bool) {
        match &mut self.content {
            TabContent::Browser(browser) => browser.set_shown(shown),
        }
    }
}

#[derive(Default)]
pub struct Inspector {
    pub tabs: Vec<Tab>,
    pub active: Option<TabId>,
    /// Closing the panel keeps the tabs; reopening shows them as they were.
    pub open: bool,
}

impl Inspector {
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|tab| Some(tab.id) == self.active)
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    /// Adds a tab right after the active one and shows it.
    pub fn add(&mut self, tab: Tab) {
        let at = self
            .active_index()
            .map_or(self.tabs.len(), |index| index + 1);
        self.active = Some(tab.id);
        self.tabs.insert(at, tab);
        self.open = true;
    }

    pub fn select(&mut self, id: TabId) {
        if self.tabs.iter().any(|tab| tab.id == id) {
            self.active = Some(id);
            self.open = true;
        }
    }

    /// Closing the active tab activates its right neighbor, else its left; the last one closes
    /// the panel.
    pub fn close(&mut self, id: TabId) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let mut tab = self.tabs.remove(index);
        tab.set_shown(false);
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.last())
                .map(|tab| tab.id);
        }
        if self.tabs.is_empty() {
            self.open = false;
        }
    }

    /// The tab already showing `url`, so clicking the same link twice doesn't duplicate it.
    pub fn find_page(&self, url: &str) -> Option<TabId> {
        let url = url.trim_end_matches('/');
        self.tabs
            .iter()
            .find(|tab| {
                tab.browser()
                    .is_some_and(|b| b.url.trim_end_matches('/') == url)
            })
            .map(|tab| tab.id)
    }

    /// Shows only the active tab, and only while the panel is on screen.
    pub fn sync_visibility(&mut self, on_screen: bool) {
        let active = self.active;
        for tab in &mut self.tabs {
            tab.set_shown(on_screen && self.open && Some(tab.id) == active);
        }
    }

    fn active_index(&self) -> Option<usize> {
        self.tabs.iter().position(|tab| Some(tab.id) == self.active)
    }
}

/// A page tab's label: its title once loaded, else its host, else "New Tab".
fn page_label(title: &str, url: &str) -> String {
    if !title.trim().is_empty() {
        return title.trim().to_owned();
    }
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if host.is_empty() || url.starts_with("about:") {
        return "New Tab".to_owned();
    }
    host.trim_start_matches("www.").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_labels_prefer_title_then_host() {
        assert_eq!(
            page_label(" Rust Blog ", "https://blog.rust-lang.org/"),
            "Rust Blog"
        );
        assert_eq!(page_label("", "https://www.github.com/a/b"), "github.com");
        assert_eq!(page_label("", "about:blank"), "New Tab");
    }
}
