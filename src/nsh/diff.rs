//! The viewer's diff pane: one changed file, old against new, side by side or inline. nsh's
//! `DiffView` compares the two versions, highlights the words that changed and folds the lines
//! between changes; [`Snapshot`] is what [`crate::Reader`] publishes.
use std::sync::LazyLock;

use nsh_base::{DiffLayout, diff_view};
use nsh_core::prelude::*;
use tokio::sync::watch;

use super::bus::{Cmd, send};
use super::header::{Control, pane_header};
use super::source::Shared;
use super::{Colors, rems, text_sm, text_xs};

#[derive(Clone, PartialEq, Debug)]
pub enum Body {
    /// No file is picked: what to do about it.
    Hint(String),
    /// The diff is being worked out.
    Loading,
    /// The file has no textual changes.
    NoChanges,
    /// The file's two versions, `path` naming the language.
    Versions { path: String, old: Shared, new: Shared },
}

#[derive(Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub glass: bool,
    pub title: String,
    pub lead: Vec<Control>,
    pub trail: Vec<Control>,
    /// Side by side (true) or inline.
    pub split: bool,
    /// The lines added and removed.
    pub counts: (usize, usize),
    pub body: Body,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self { glass: false, title: String::new(), lead: vec![], trail: vec![], split: true, counts: (0, 0), body: Body::Loading }
    }
}

static SNAPSHOT: LazyLock<watch::Sender<Snapshot>> = LazyLock::new(|| watch::Sender::new(Snapshot::default()));

/// Shows `snapshot`; nothing is redrawn when it is what the screen already has.
pub fn publish(snapshot: Snapshot) {
    SNAPSHOT.send_if_modified(|now| {
        let changed = *now != snapshot;
        if changed {
            *now = snapshot;
        }
        changed
    });
}

fn centered(message: String) -> View {
    view().flex_1().items_center().justify_center().px(rems(1.)).child(text(message).text_size(text_sm()).text_color(Colors::muted()))
}

/// One of the two layouts, as a button of the pair.
fn layout_button(label: &'static str, split: bool, current: Signal<Snapshot>) -> View {
    let selected = move || current.with(|s| s.split) == split;
    view()
        .role(AccessibilityRole::Button)
        .label(label)
        .px(rems(0.5))
        .py(rems(0.125))
        .rounded(Colors::radius())
        .cursor_pointer()
        .tooltip(super::tooltip("Toggle inline/split (Ctrl+\\)"))
        .hover(|s| s.bg(Colors::hover()))
        .style_when(selected, |s| s.bg(Colors::selected()))
        .on_press(move || send(Cmd::SetSplit(split)))
        .child(text(label).text_size(text_xs()).text_color(Colors::foreground()))
}

#[component]
pub fn Diff() -> impl IntoElement {
    let state = signal(SNAPSHOT.borrow().clone());
    spawn(async move {
        let mut snapshot = SNAPSHOT.subscribe();
        while snapshot.changed().await.is_ok() {
            let now = snapshot.borrow_and_update().clone();
            state.set(now);
        }
    });

    let right = move || {
        let (added, removed) = state.with(|s| s.counts);
        view()
            .flex_row()
            .items_center()
            .gap(rems(0.75))
            .child(
                view()
                    .flex_row()
                    .gap(rems(0.5))
                    .child(text(format!("+{added}")).text_size(text_xs()).text_color(Colors::added()))
                    .child(text(format!("-{removed}")).text_size(text_xs()).text_color(Colors::danger())),
            )
            .child(view().flex_row().child(layout_button("Inline", false, state)).child(layout_button("Split", true, state)))
    };

    let body = move || match state.with(|s| s.body.clone()) {
        Body::Hint(message) => centered(message).into_element(),
        Body::NoChanges => centered("No textual changes".into()).into_element(),
        Body::Loading => view().flex_1().into_element(),
        Body::Versions { path, old, new } => diff_view()
            .old_value(old.0.to_string())
            .new_value(new.0.to_string())
            .language(path)
            .layout(move || if state.with(|s| s.split) { DiffLayout::Split } else { DiffLayout::Unified })
            .font_size(super::mono_size())
            .line_height(super::mono_size() * 1.5)
            .flex_1()
            .into_element(),
    };

    view()
        .flex_1()
        .bg(Colors::background())
        .child(dynamic(move || {
            let (title, lead, trail, glass) = state.with(|s| (s.title.clone(), s.lead.clone(), s.trail.clone(), s.glass));
            pane_header(title, right(), &lead, &trail, glass)
        }))
        .child(dynamic(body))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;

    #[test]
    fn the_diff_pane_compares_two_versions_and_reports_its_layout_buttons() {
        let mut guard = super::super::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        publish(Snapshot {
            title: "src/lib.rs".into(),
            counts: (1, 1),
            body: Body::Versions {
                path: "src/lib.rs".into(),
                old: Shared(Arc::from("fn total() -> u32 {\n    1\n}\n")),
                new: Shared(Arc::from("fn total() -> u64 {\n    1\n}\n")),
            },
            ..Snapshot::default()
        });
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Diff>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(700.), Points(500.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app.settle();
        for shown in ["src/lib.rs", "+1", "-1", "Inline", "Split"] {
            assert!(app.shows(shown), "{shown} is on screen: {:?}", app.screen().texts());
        }
        app.press("label:Inline").unwrap();
        app.settle();
        assert_eq!(commands.try_recv().ok(), Some(Cmd::SetSplit(false)));

        // No file picked, loading, and no textual changes each say so.
        publish(Snapshot { title: "Changes".into(), body: Body::Hint("Pick a changed file in the tree".into()), ..Snapshot::default() });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        assert!(app.shows("Pick a changed file in the tree"));
        publish(Snapshot { title: "a.bin".into(), body: Body::NoChanges, ..Snapshot::default() });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        assert!(app.shows("No textual changes"));
    }
}
