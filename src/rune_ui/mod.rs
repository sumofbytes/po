//! Po's screens on Rune: what they share. This is the Rune port's counterpart of `src/nsh/mod.rs`.
pub mod agent;
pub mod chat;
pub mod diff;
pub mod dropdown;
pub mod explorer;
pub mod finder;
pub mod header;
pub mod home;
pub mod issues;
pub mod pictures;
pub mod rename;
pub mod settings;
pub mod shell;
pub mod sidebar;
pub mod source;
pub mod tabs;
pub mod terminal;
pub mod theme;
pub mod theme_pick;
pub mod work;

/// Markdown set to read like a page of a book: a text serif, a little larger than the UI, with
/// open leading. Code keeps its monospace, which the style sets apart from these.
pub fn book_style(k: &rune_kit::Kit) -> rune_kit::markdown::MarkdownStyle {
    use rune::tok::{Family, TextStyle};
    let body = k.ty.body.size + 1.0;
    let serif = |size: f32, weight: f32, leading: f32| {
        TextStyle::new(size, weight)
            .family(Family::Named(crate::config::fonts::BUNDLED_BOOK_FAMILY))
            .lh((size * leading).round())
    };
    rune_kit::markdown::MarkdownStyle::from_kit(k)
        .body_font(serif(body, 400.0, 1.6))
        .heading_font(1, serif(body * 1.45, 600.0, 1.3))
        .heading_font(2, serif(body * 1.2, 600.0, 1.3))
        .heading_font(3, serif(body * 1.05, 600.0, 1.4))
}
