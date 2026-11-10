//! What Home, the issues page and the epics share: how an issue's status and priority look.
use rune::{Color, El, col, text};
use rune::tok::TextStyle;
use rune_kit::Kit;

use crate::issues::{Priority, Status};

pub fn body_style() -> TextStyle { crate::fonts::ui(13.0, 400.0) }
pub fn small_style() -> TextStyle { crate::fonts::ui(12.0, 400.0) }
pub fn title_style() -> TextStyle { crate::fonts::ui(22.0, 600.0) }
pub fn heading_style() -> TextStyle { crate::fonts::ui(14.0, 600.0) }

pub fn status_tint(k: &Kit, status: Status) -> Color {
    match status {
        Status::Backlog | Status::Canceled => k.c.text_muted,
        Status::Todo => k.c.text_base,
        Status::InProgress => k.c.warning.fg,
        Status::InReview => k.c.info.fg,
        Status::Done => k.c.success.fg,
    }
}

pub fn status_glyph(status: Status) -> &'static str {
    match status {
        Status::Backlog => "◌",
        Status::Todo => "○",
        Status::InProgress => "◐",
        Status::InReview => "◉",
        Status::Done => "✓",
        Status::Canceled => "×",
    }
}

pub fn priority_tint(k: &Kit, priority: Priority) -> Color {
    match priority {
        Priority::Urgent => k.c.danger.fg,
        Priority::High => k.c.warning.fg,
        Priority::Medium => k.c.text_base,
        Priority::Low | Priority::None => k.c.text_muted,
    }
}

/// A bordered card with a title, to put rows in.
pub fn card(k: &Kit, title: &str) -> El {
    col().w_full().gap(8.0).padding(16.0).radius(10.0).border(1.0, k.c.border_base).child(text(title.to_string()).font(heading_style()).color(k.c.text_base))
}

pub fn muted(k: &Kit, s: impl Into<String>) -> El {
    text(s.into()).font(small_style()).color(k.c.text_muted)
}

/// A number with what it counts, in a box.
pub fn metric(k: &Kit, label: &str, value: usize, tint: Color) -> El {
    col().grow(1.0).min_w(0.0).px(16.0).py(12.0).radius(10.0).border(1.0, k.c.border_base).child(text(value.to_string()).font(crate::fonts::ui(24.0, 600.0)).color(tint)).child(muted(k, label))
}

