//! Cmd+= / Cmd+- / Cmd+0 zoom, like other Mac apps. Every UI size goes through [`px`], so
//! layout, spacing and text scale together; the level is remembered across launches.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{App, KeyBinding, Menu, MenuItem, Pixels, actions};

actions!(toma, [ZoomIn, ZoomOut, ResetZoom, Quit]);

const STEP: f32 = 0.1;
const RANGE: (f32, f32) = (0.6, 2.0);

/// The zoom factor, stored as `f32` bits.
static ZOOM: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0

pub fn zoom() -> f32 {
    f32::from_bits(ZOOM.load(Ordering::Relaxed))
}

/// Zoom-aware replacement for `gpui::px`, for every size the UI draws.
pub fn px(value: f32) -> Pixels {
    gpui::px(value * zoom())
}

pub fn init(cx: &mut App) {
    if let Some(saved) = saved_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| text.trim().parse::<f32>().ok())
    {
        ZOOM.store(clamp(saved).to_bits(), Ordering::Relaxed);
    }
    cx.bind_keys([
        KeyBinding::new("cmd-=", ZoomIn, None),
        KeyBinding::new("cmd-+", ZoomIn, None),
        KeyBinding::new("cmd--", ZoomOut, None),
        KeyBinding::new("cmd-0", ResetZoom, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
    cx.on_action(|_: &ZoomIn, cx| set(zoom() + STEP, cx));
    cx.on_action(|_: &ZoomOut, cx| set(zoom() - STEP, cx));
    cx.on_action(|_: &ResetZoom, cx| set(1.0, cx));
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.set_menus(vec![
        Menu {
            name: "Toma".into(),
            items: vec![MenuItem::action("Quit Toma", Quit)],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Actual Size", ResetZoom),
                MenuItem::action("Zoom In", ZoomIn),
                MenuItem::action("Zoom Out", ZoomOut),
            ],
        },
    ]);
}

fn set(value: f32, cx: &mut App) {
    let value = clamp(value);
    ZOOM.store(value.to_bits(), Ordering::Relaxed);
    if let Some(path) = saved_path() {
        let _ = std::fs::create_dir_all(path.parent().expect("has parent"));
        let _ = std::fs::write(path, value.to_string());
    }
    cx.refresh_windows();
}

/// Snaps to 10% steps so repeated presses land on round numbers.
fn clamp(value: f32) -> f32 {
    ((value / STEP).round() * STEP).clamp(RANGE.0, RANGE.1)
}

fn saved_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/Toma/zoom"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_snaps_to_steps_within_range() {
        assert_eq!(clamp(1.0 + STEP), 1.1);
        assert_eq!(clamp(0.1), RANGE.0);
        assert_eq!(clamp(9.0), RANGE.1);
    }
}
