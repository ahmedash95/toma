//! The ⌘K jump palette: finds channels and threads by name, like Slack's quick switcher.

use gpui::{App, KeyBinding, actions};
use toma_domain::*;

use crate::view_model::ShellViewModel;

/// Results shown at once; the list scrolls past this.
const LIMIT: usize = 50;

actions!(toma, [TogglePalette]);

/// ⌘K, plus ⌘T as Slack also accepts, from anywhere in the window.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", TogglePalette, None),
        KeyBinding::new("cmd-t", TogglePalette, None),
    ]);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Jump {
    Channel(ChannelId),
    Thread(ThreadId),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaletteItem {
    pub jump: Jump,
    pub title: String,
    /// The channel a thread lives in; empty for channels.
    pub subtitle: String,
    pub folder: bool,
    pub status: Option<WorkStatus>,
}

/// With no query, threads by recent activity followed by channels in sidebar order.
/// Otherwise everything whose title matches, best match first.
pub fn search(model: &ShellViewModel, query: &str) -> Vec<PaletteItem> {
    let snapshot = &model.snapshot;
    let channel_name = |id: ChannelId| {
        snapshot
            .channels
            .iter()
            .find(|channel| channel.id == id)
            .map_or(String::new(), |channel| channel.name.clone())
    };
    let threads = model
        .recent_threads()
        .into_iter()
        .map(|thread| PaletteItem {
            jump: Jump::Thread(thread.id),
            title: thread.title.clone(),
            subtitle: channel_name(thread.channel_id),
            folder: false,
            status: Some(thread.status),
        });
    let channels = snapshot.channels.iter().map(|channel| PaletteItem {
        jump: Jump::Channel(channel.id),
        title: channel.name.clone(),
        subtitle: String::new(),
        folder: channel.repository_path.is_some(),
        status: None,
    });
    let items = threads.chain(channels);

    let query = query.trim();
    if query.is_empty() {
        return items.take(LIMIT).collect();
    }
    let mut scored: Vec<_> = items
        .filter_map(|item| Some((score(&item.title, query)?, item)))
        .collect();
    // Stable, so equal scores keep the recency / sidebar order.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored
        .into_iter()
        .take(LIMIT)
        .map(|(_, item)| item)
        .collect()
}

/// How well `query` matches `text`, case-insensitively: a prefix beats the start of a word,
/// which beats any substring, which beats the letters appearing in order. `None` when the
/// letters are not all there.
fn score(text: &str, query: &str) -> Option<u32> {
    let text = text.to_lowercase();
    let query = query.to_lowercase();
    if let Some(at) = text.find(&query) {
        let word_start = at == 0 || !text[..at].ends_with(char::is_alphanumeric);
        return Some(match (at, word_start) {
            (0, _) => 400,
            (_, true) => 300,
            _ => 200,
        });
    }
    let mut letters = text.chars();
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|wanted| letters.any(|c| c == wanted))
        .then_some(100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toma_storage::WorkspaceSnapshot;

    fn model() -> (ShellViewModel, ChannelId, ThreadId, ThreadId) {
        let workspace_id = WorkspaceId::new();
        let general = ChannelId::new();
        let design = ChannelId::new();
        let old = ThreadId::new();
        let recent = ThreadId::new();
        let channel = |id, name: &str, position| Channel {
            id,
            workspace_id,
            name: name.into(),
            position,
            repository_path: None,
        };
        let thread = |id, title: &str, updated_at| TaskThread {
            id,
            channel_id: general,
            root_message_id: MessageId::new(),
            title: title.into(),
            status: WorkStatus::Working,
            created_at: 0,
            updated_at,
            permission_mode: PermissionMode::Ask,
        };
        let snapshot = WorkspaceSnapshot {
            channels: vec![
                channel(general, "general", 0),
                channel(design, "design-review", 1),
            ],
            threads: vec![
                thread(old, "Fix login redirect", 1),
                thread(recent, "Command palette", 2),
            ],
            ..Default::default()
        };
        (ShellViewModel::new(snapshot), design, old, recent)
    }

    fn jumps(items: Vec<PaletteItem>) -> Vec<Jump> {
        items.into_iter().map(|item| item.jump).collect()
    }

    #[test]
    fn empty_query_lists_recent_threads_then_channels() {
        let (model, design, old, recent) = model();
        let items = search(&model, " ");
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].jump, Jump::Thread(recent));
        assert_eq!(items[0].subtitle, "general");
        assert_eq!(items[1].jump, Jump::Thread(old));
        assert_eq!(items[3].jump, Jump::Channel(design));
    }

    #[test]
    fn prefix_and_word_matches_rank_above_scattered_letters() {
        let (model, design, old, recent) = model();
        assert_eq!(jumps(search(&model, "REVIEW")), [Jump::Channel(design)]);
        assert_eq!(jumps(search(&model, "log")), [Jump::Thread(old)]);
        // "cp" is scattered in "Command palette" only; "de" prefixes "design-review".
        assert_eq!(jumps(search(&model, "cmd pal")), [Jump::Thread(recent)]);
        assert_eq!(search(&model, "de")[0].jump, Jump::Channel(design));
        assert!(search(&model, "zzz").is_empty());
    }

    #[test]
    fn scores_order_match_kinds() {
        assert_eq!(score("design-review", "des"), Some(400));
        assert_eq!(score("design-review", "rev"), Some(300));
        assert_eq!(score("design-review", "sig"), Some(200));
        assert_eq!(score("design-review", "dr"), Some(100));
        assert_eq!(score("design-review", "x"), None);
    }
}
