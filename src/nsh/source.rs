//! The viewer's source pane: the open file in a read-only editor, or a markdown file rendered, with
//! a find bar. [`Snapshot`] is what [`crate::Reader`] publishes; the screen draws it.
use std::sync::{Arc, LazyLock};

use nsh_base::{EditorApi, EditorSearch, EditorSearchResult, editor, text_view};
use nsh_core::prelude::*;
use tokio::sync::watch;

use super::bus::{Cmd, send};
use super::header::{Control, header_button, pane_header, status, toggle_button};
use super::{Colors, icon_button, note, scale};
use crate::ui::icon::IconName;

/// A file's text, compared by identity so a snapshot that holds the same text costs nothing to
/// compare.
#[derive(Clone, Debug)]
pub struct Shared(pub Arc<str>);

impl PartialEq for Shared {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Body {
    /// Nothing is open: what to do about it.
    Hint(String),
    /// A file that can't be shown, and why.
    Notice(String),
    Code { path: String, text: Shared },
    /// A markdown file, rendered.
    Markdown(Shared),
}

#[derive(Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub glass: bool,
    pub title: String,
    pub lead: Vec<Control>,
    pub trail: Vec<Control>,
    /// The file's language, said in the header.
    pub language: String,
    /// A markdown file has a toggle: `Some(rendered)`.
    pub preview: Option<bool>,
    pub body: Body,
    /// A line to scroll to (from 0), and a number that changes each time one is asked for.
    pub reveal: Option<(u32, u64)>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self { glass: false, title: String::new(), lead: vec![], trail: vec![], language: String::new(), preview: None, body: Body::Hint(String::new()), reveal: None }
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
    view().flex_1().items_center().justify_center().px(scale::s4()).child(note(message))
}

#[component]
pub fn Source() -> impl IntoElement {
    let state = signal(SNAPSHOT.borrow().clone());
    spawn(async move {
        let mut snapshot = SNAPSHOT.subscribe();
        // What was published before this ran would otherwise never arrive.
        snapshot.mark_changed();
        while snapshot.changed().await.is_ok() {
            let now = snapshot.borrow_and_update().clone();
            state.set(now);
        }
    });

    // Find: Ctrl+F opens the bar over the code, Esc closes it.
    let finding = signal(false);
    let query = signal(String::new());
    let case = signal(false);
    let result = signal(EditorSearchResult::default());
    let code = component_ref::<nsh_base::Editor>();
    let find_field = element_ref();
    on_key_with("mod+f", KeyOptions { when: nsh_core::keymap::When::Always, repeat: false }, move |_| {
        finding.set(true);
        find_field.focus();
    });
    on_key_with("escape", KeyOptions { when: nsh_core::keymap::When::Always, repeat: false }, move |_| finding.set(false));
    let run = move |command: nsh_base::EditorCommand| {
        if let Some(api) = code.api::<EditorApi>() {
            api.run(command);
        }
    };

    // A line asked for scrolls there, once per request.
    let revealed = stored(0_u64);
    effect(move || {
        if let Some((line, serial)) = state.with(|s| s.reveal)
            && revealed.replace(serial) != serial
            && let Some(api) = code.api::<EditorApi>()
        {
            api.go_to_line(line);
        }
    });

    let find_bar = move || {
        finding.get().then(|| {
            view()
                .flex_row()
                .items_center()
                .gap(scale::s2())
                .px(scale::s3())
                .py(scale::s1_5())
                .border_b_1()
                .border_color(Colors::border())
                .child(super::field_with("source-find", query, "Find", move || run(nsh_base::EditorCommand::FindNext), move |input| input.element_ref(&find_field).auto_focus(true)).flex_1())
                .child(toggle_button(IconName::CaseSensitive, "Match case", case.get(), move || case.update(|on| *on = !*on)))
                .child(
                    text(move || {
                        let found = result.get();
                        match (found.error, query.get().is_empty()) {
                            (Some(_), _) => "Invalid".to_string(),
                            (None, true) => String::new(),
                            (None, false) if found.count == 0 => "No results".to_string(),
                            (None, false) => format!("{} of {}", found.current.map_or(0, |c| c + 1), found.count),
                        }
                    })
                    .text_size(super::text_xs())
                    .text_color(Colors::muted()),
                )
                .child(icon_button(IconName::ChevronLeft, scale::s3(), "Previous match", move || run(nsh_base::EditorCommand::FindPrevious)))
                .child(icon_button(IconName::ChevronRight, scale::s3(), "Next match", move || run(nsh_base::EditorCommand::FindNext)))
                .child(icon_button(IconName::Close, scale::s3(), "Close (Esc)", move || finding.set(false)))
        })
    };

    let body = move || match state.with(|s| s.body.clone()) {
        Body::Hint(message) | Body::Notice(message) => centered(message).into_element(),
        Body::Markdown(text) => scroll()
            .flex_1()
            .child(
                text_view()
                    .markdown(text.0.to_string())
                    .on_link_press(|press: &nsh_base::TextViewLinkPressEvent| send(Cmd::OpenUrl(press.url.to_string())))
                    .p(scale::s4())
                    .text_size(super::text_base())
                    .text_color(Colors::foreground()),
            )
            .into_element(),
        Body::Code { path, text } => view()
            .flex_1()
            .child(dynamic(find_bar))
            .child(
                editor()
                    .component_ref(&code)
                    .value(move || Some(text.0.to_string().into()))
                    .language(path)
                    .read_only(true)
                    .search(move || (finding.get() && !query.get().is_empty()).then(|| {
                        let mut search = EditorSearch::default();
                        search.query = query.get().into();
                        search.case_sensitive = case.get();
                        search
                    }))
                    .on_search_change(move |found: &EditorSearchResult| result.set(found.clone()))
                    .font_size(super::mono_size())
                    .line_height(super::mono_size() * 1.5)
                    .flex_1(),
            )
            .into_element(),
    };

    let right = move || {
        view()
            .flex_row()
            .items_center()
            .gap(scale::s2())
            .child(status(state.with(|s| s.language.clone())))
            .child(state.with(|s| s.preview).map(|rendered| {
                header_button(
                    if rendered { "Source" } else { "Preview" },
                    if rendered { "Show the markdown source (Ctrl+Shift+M)" } else { "Show the rendered markdown (Ctrl+Shift+M)" },
                    || send(Cmd::ToggleRendered),
                )
            }))
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
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;

    fn code(path: &str, text: &str) -> Body {
        Body::Code { path: path.into(), text: Shared(Arc::from(text)) }
    }

    #[test]
    fn the_source_pane_shows_files_notes_and_markdown_and_reports_its_buttons() {
        let mut guard = super::super::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let base = Snapshot {
            title: "src/lib.rs".into(),
            language: "Rust".into(),
            lead: vec![Control::Sidebar(true)],
            trail: vec![Control::Focus(false), Control::Viewer(true)],
            ..Snapshot::default()
        };
        publish(Snapshot { body: code("src/lib.rs", "fn main() {}\n"), ..base.clone() });
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Source>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(500.), Points(500.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app.settle();
        for shown in ["src/lib.rs", "Rust", "fn main() {}"] {
            assert!(app.shows(shown), "{shown} is on screen: {:?}", app.screen().texts());
        }
        let mut next = |app: &mut Headless| {
            app.settle();
            std::iter::from_fn(|| commands.try_recv().ok()).collect::<Vec<_>>()
        };

        // The window's controls in the header report themselves.
        app.press("label:Sidebar (Ctrl+B)").unwrap();
        app.press("label:Viewer (Ctrl+Shift+B)").unwrap();
        app.press("label:Focus the viewer (Ctrl+Shift+F)").unwrap();
        assert_eq!(next(&mut app), [Cmd::ToggleLeft, Cmd::ToggleRight, Cmd::ToggleFocus]);

        // Nothing open, then a file that can't be shown, say so.
        publish(Snapshot { title: "No file open".into(), body: Body::Hint("Pick a file in the explorer.".into()), ..Snapshot::default() });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        assert!(app.shows("Pick a file in the explorer.") && app.shows("No file open"), "{:?}", app.screen().texts());
        publish(Snapshot { title: "big.bin".into(), body: Body::Notice("big.bin is too large to show".into()), ..Snapshot::default() });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        assert!(app.shows("big.bin is too large to show"));

        // A markdown file has its toggle, and renders.
        publish(Snapshot {
            title: "README.md".into(),
            language: "Markdown".into(),
            preview: Some(true),
            body: Body::Markdown(Shared(Arc::from("# Hello\n\nSome *words*.\n"))),
            ..Snapshot::default()
        });
        app.wait(std::time::Duration::from_millis(150)).unwrap();
        assert!(app.shows("Hello"), "{:?}", app.screen().texts());
        app.press("label:Show the markdown source (Ctrl+Shift+M)").unwrap();
        assert_eq!(next(&mut app), [Cmd::ToggleRendered]);
    }
}
