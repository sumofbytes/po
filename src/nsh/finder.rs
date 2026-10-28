//! Ctrl+P: a query field over the project's files. [`Snapshot`] is the files that match, which
//! [`crate::Reader`] works out; the screen keeps the query and which file is picked.
use std::sync::{LazyLock, Mutex};

use nsh_core::prelude::*;
use tokio::sync::watch;

use super::bus::{Cmd, send};
use super::overlay::{self, Height};
use super::{Colors, field_with, rems, text_base};

/// The height the screen needs, which the overlay around it takes.
pub static HEIGHT: Height = overlay::height();

/// The files that match the query, in the order to show them.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    pub matches: Vec<String>,
    /// The walk stopped at this many files, so the list may be missing some.
    pub capped: Option<usize>,
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

/// What the field holds as the finder opens: the last query, which is selected.
static QUERY: Mutex<String> = Mutex::new(String::new());

/// Sets what the next finder starts from.
pub fn start(query: &str) {
    *QUERY.lock().unwrap_or_else(|e| e.into_inner()) = query.to_string();
}

/// What the list below the field has room for.
fn list_height() -> Points {
    Points(380.)
}

/// A file's name, and the folder it is in with its trailing slash.
fn split(path: &str) -> (&str, &str) {
    let name = path.rsplit('/').next().unwrap_or(path);
    (name, path.strip_suffix(name).unwrap_or(""))
}

#[component]
pub fn Finder() -> impl IntoElement {
    let query = signal(QUERY.lock().unwrap_or_else(|e| e.into_inner()).clone());
    let matches = signal(SNAPSHOT.borrow().clone());
    let selected = signal(0_usize);
    spawn(async move {
        let mut snapshot = SNAPSHOT.subscribe();
        // What was published before this ran would otherwise never arrive.
        snapshot.mark_changed();
        while snapshot.changed().await.is_ok() {
            let now = snapshot.borrow_and_update().clone();
            // A refreshed walk keeps the pick where it was, as far as there are files.
            selected.update(|at| *at = (*at).min(now.matches.len().saturating_sub(1)));
            matches.set(now);
        }
    });
    let list = element_ref();
    let input = element_ref();
    let count = move || matches.with(|m| m.matches.len());

    // The field takes the keyboard with the last query selected, so typing replaces it.
    let typed = query.get_untracked().encode_utf16().count() as u32;
    spawn(async move {
        input.focus();
        input.set_selection(0, typed);
    });

    let step = move |by: isize| {
        let len = count() as isize;
        if len == 0 {
            return;
        }
        let at = (selected.get_untracked() as isize + by).rem_euclid(len) as usize;
        selected.set(at);
        list.scroll_to_index(at as u32, percentage(0.5), px(0.0), false);
    };
    let pick = move |at: usize| {
        if let Some(path) = matches.with_untracked(|m| m.matches.get(at).cloned()) {
            send(Cmd::FinderPick(path));
        }
    };
    let always = KeyOptions { when: nsh_core::keymap::When::Always, repeat: true };
    on_key_with("up", always, move |_| step(-1));
    on_key_with("down", always, move |_| step(1));
    on_key_with("escape", KeyOptions { repeat: false, ..always }, |_| send(Cmd::CloseFinder));

    let parts: [Signal<f32>; 3] = [signal(0.), signal(0.), signal(0.)];
    effect(move || {
        let height: f32 = parts.iter().map(|part| part.get()).sum();
        HEIGHT.send_if_modified(|now| std::mem::replace(now, height) != height);
    });
    let measure = move |part: usize| move |bounds: &Bounds<Points>| parts[part].set(bounds.size.height.0);

    let row = move |at: Position| {
        let path = move || matches.with(|m| m.matches.get(at.index()).cloned().unwrap_or_default());
        view()
            .flex_row()
            .items_baseline()
            .gap(rems(0.5))
            .px(rems(0.75))
            .py(rems(0.375))
            .cursor_pointer()
            .hover(|s| s.bg(Colors::hover()))
            .style_when(move || selected.get() == at.index(), |s| s.bg(Colors::selected()))
            .on_press(move || pick(at.index()))
            .child(text(move || split(&path()).0.to_string()).number_of_lines(1).text_size(text_base()).text_color(Colors::foreground()))
            .child(text(move || split(&path()).1.to_string()).number_of_lines(1).flex_shrink(1.).text_size(text_base()).text_color(Colors::muted()))
    };

    // Every keystroke refilters, from the top.
    effect(move || {
        let query = query.get();
        selected.set(0);
        send(Cmd::FinderQuery(query));
    });

    view()
        .flex_1()
        .bg(Colors::popover())
        .child(
            view().on_layout(measure(0)).child(field_with(
                "finder-query",
                query,
                "Search files…",
                move || pick(selected.get_untracked()),
                move |field| field.element_ref(&input),
            )),
        )
        .child(
            view()
                .h(list_height())
                .flex_none()
                .border_t_1()
                .border_color(Colors::border())
                .on_layout(measure(1))
                .child(
                    virtual_list(count, row)
                        .element_ref(&list)
                        .estimated_item_size(rems(2.))
                        .flex_1(),
                )
                .child(when(move || count() == 0, move || {
                    view()
                        .absolute()
                        .inset_0()
                        .items_center()
                        .pt(rems(0.75))
                        .child(text("No matching files").text_color(Colors::muted()))
                })),
        )
        .child(
            view()
                .flex_row()
                .justify_between()
                .px(rems(0.75))
                .py(rems(0.25))
                .border_t_1()
                .border_color(Colors::border())
                .on_layout(measure(2))
                .child(text(move || match matches.with(|m| m.capped) {
                    Some(files) => format!("first {files} files only"),
                    None => "↑↓ navigate".to_string(),
                }).text_color(Colors::muted()))
                .child(text("↵ open · esc close").text_color(Colors::muted())),
        )
}

#[cfg(test)]
mod tests {
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;

    #[test]
    fn the_finder_lists_matches_picks_with_keys_and_reports_the_query() {
        let mut guard = super::super::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let files = ["src/main.rs", "src/nsh/finder.rs", "README.md"];
        start("");
        publish(Snapshot { matches: files.iter().map(|f| f.to_string()).collect(), capped: None });
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Finder>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(560.), Points(600.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app.settle();
        for shown in ["main.rs", "src/", "finder.rs", "src/nsh/", "README.md", "↑↓ navigate", "↵ open · esc close"] {
            assert!(app.shows(shown), "{shown} is on screen: {:?}", app.screen().texts());
        }
        let mut next = |app: &mut Headless| {
            app.settle();
            std::iter::from_fn(|| commands.try_recv().ok()).collect::<Vec<_>>()
        };
        // The query the screen starts with is reported once, as it is.
        assert_eq!(next(&mut app), [Cmd::FinderQuery(String::new())]);

        // Down twice, then Enter, picks the third file; Up wraps from the first to the last.
        app.press_key("down").unwrap();
        app.press_key("down").unwrap();
        app.submit("id:finder-query").unwrap();
        assert_eq!(next(&mut app), [Cmd::FinderPick("README.md".into())]);
        app.press_key("up").unwrap();
        app.press_key("up").unwrap();
        app.press_key("up").unwrap();
        app.submit("id:finder-query").unwrap();
        assert_eq!(next(&mut app), [Cmd::FinderPick("README.md".into())]);

        // A press on a row picks it; typing reports the query; Esc closes.
        app.press("text:finder.rs").unwrap();
        assert_eq!(next(&mut app), [Cmd::FinderPick("src/nsh/finder.rs".into())]);
        app.type_text("id:finder-query", "fi").unwrap();
        assert_eq!(next(&mut app).last(), Some(&Cmd::FinderQuery("fi".into())));
        app.press_key("escape").unwrap();
        assert_eq!(next(&mut app), [Cmd::CloseFinder]);

        // New matches replace the list; none shows the empty note, and a capped walk says so.
        publish(Snapshot { matches: vec![], capped: Some(5000) });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        assert!(app.shows("No matching files") && app.shows("first 5000 files only"), "{:?}", app.screen().texts());
    }
}
