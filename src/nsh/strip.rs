//! The strips under the explorer's header: how a PR review is opened or closed, and how a
//! worktree's branch is finished. [`Snapshot`] is what [`crate::Reader`] publishes; the screen
//! draws it.
use std::sync::LazyLock;

use nsh_core::prelude::*;
use tokio::sync::watch;

use super::bus::{Cmd, send};
use super::{Colors, body, caption, field_with, rems, text_xs, tooltip};

/// The strip a PR-reviewed project shows, or the way to review one.
#[derive(Clone, PartialEq, Debug)]
pub enum Pr {
    /// The open PR's summary, with its way out.
    Opened { label: String },
    /// Nothing open: a field for the PR's number or URL, whether opening one is running, and
    /// how the last attempt went (`Some` while it runs, and failed once it stopped).
    Asking { status: Option<String>, opening: bool },
}

/// The strip a worktree project shows: its branch, and the ways to finish it.
#[derive(Clone, PartialEq, Debug)]
pub struct Worktree {
    pub branch: String,
    /// Discard was asked for once and waits for a second click, since it can't be undone.
    pub confirm_discard: bool,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    pub worktree: Option<Worktree>,
    pub pr: Option<Pr>,
    /// Bumped each time the PR field was emptied after a PR opened: the screen clears what
    /// its field holds when it sees a new value.
    pub pr_reset: u64,
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

/// A small text button: an outline, or a ghost one with only its hover behind it.
fn small_button(label: impl IntoReactive<SharedString> + Clone + 'static, outline: bool, tip: Option<&'static str>, on_press: impl Fn() + 'static) -> View {
    let b = view()
        .role(AccessibilityRole::Button)
        .label(label.clone())
        .flex_none()
        .h(rems(1.5))
        .px(rems(0.5))
        .rounded(Colors::radius())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
        .when_some(tip, |b, tip| b.tooltip(tooltip(tip)))
        .on_press(on_press);
    let b = if outline { b.border_1().border_color(Colors::input_border()) } else { b };
    b.child(
        view()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(body(label)),
    )
}

fn pr_strip(pr: Pr, spec: Signal<String>, on_open: impl Fn() + Clone + 'static) -> View {
    match pr {
        Pr::Opened { label } => view()
            .flex_row()
            .items_center()
            .gap(rems(0.5))
            .px(rems(0.75))
            .py(rems(0.5))
            .border_b_1()
            .border_color(Colors::border())
            .child(caption(label).flex_1().min_w_0().number_of_lines(1))
            .child(small_button("Close", false, Some("Return to this repository, forgetting the review"), move || send(Cmd::ClosePr))),
        Pr::Asking { status, opening } => {
            let submit = on_open.clone();
            view()
                .flex_col()
                .gap(rems(0.25))
                .px(rems(0.75))
                .py(rems(0.5))
                .border_b_1()
                .border_color(Colors::border())
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(rems(0.5))
                        .min_w_0()
                        .child(field_with("pr-spec", spec, "PR number or URL", submit, |input| input).flex_1().min_w_0())
                        .child(small_button(if opening { "…" } else { "Open" }, true, None, on_open)),
                )
                .children(status.map(|line| {
                    let failed = !opening;
                    text(line)
                        .text_size(text_xs())
                        .text_color(move || if failed { Colors::danger()() } else { Colors::muted()() })
                }))
        }
    }
}

fn worktree_strip(wt: Worktree) -> View {
    view()
        .flex_col()
        .gap(rems(0.25))
        .px(rems(0.75))
        .py(rems(0.5))
        .border_b_1()
        .border_color(Colors::border())
        .child(
            text(format!("⎇ {}", wt.branch))
                .flex_1()
                .min_w_0()
                .number_of_lines(1)
                .text_size(text_xs())
                .text_color(Colors::muted()),
        )
        .child(
            view()
                .flex_row()
                .gap(rems(0.5))
                .child(small_button("Merge", true, Some("Commit this branch's work and merge it into the checkout it came from"), || send(Cmd::MergeWorktree)))
                .child(small_button(
                    if wt.confirm_discard { "Discard everything?" } else { "Discard" },
                    false,
                    Some("Delete this branch and everything done in it"),
                    || send(Cmd::DiscardWorktree),
                )),
        )
}

#[component]
pub fn Strips() -> impl IntoElement {
    strips_view()
}

/// The strips themselves, for a screen that has them under its header.
pub fn strips_view() -> View {
    let state = signal(SNAPSHOT.borrow().clone());
    // What the PR field holds, the screen's own; emptied when Po says it emptied the PR field,
    // which a PR opening does.
    let spec = signal(String::new());
    spawn(async move {
        let mut snapshot = SNAPSHOT.subscribe();
        // What was published before this ran would otherwise never arrive.
        snapshot.mark_changed();
        // The value of `pr_reset` the field was seen with last.
        let mut emptied = 0_u64;
        while snapshot.changed().await.is_ok() {
            let now = snapshot.borrow_and_update().clone();
            if now.pr_reset != emptied {
                emptied = now.pr_reset;
                spec.set(String::new());
            }
            state.set(now);
        }
    });
    let open = move || {
        let typed = spec.get();
        let typed = typed.trim();
        if !typed.is_empty() {
            send(Cmd::OpenPr(typed.to_string()));
        }
    };

    view()
        .flex_col()
        .min_w_0()
        .child(dynamic(move || state.with(|s| s.worktree.clone()).map(worktree_strip)))
        .child(dynamic(move || {
            state
                .with(|s| s.pr.clone())
                .map(move |pr| pr_strip(pr, spec, open))
        }))
}

#[cfg(test)]
mod tests {
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;

    #[test]
    fn the_strips_open_speak_of_prs_and_worktrees_and_answer_their_buttons() {
        let mut guard = super::super::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let mut next = |app: &mut Headless| {
            app.settle();
            std::iter::from_fn(|| commands.try_recv().ok()).collect::<Vec<_>>()
        };

        // A worktree: its branch and its merge button.
        publish(Snapshot {
            worktree: Some(Worktree { branch: "po/nsh".into(), confirm_discard: false }),
            ..Snapshot::default()
        });
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Strips>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(400.), Points(200.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app.settle();
        assert!(app.shows("⎇ po/nsh"), "{:?}", app.screen().texts());
        app.press("label:Merge").unwrap();
        assert_eq!(next(&mut app), [Cmd::MergeWorktree]);
        app.press("label:Discard").unwrap();
        assert_eq!(next(&mut app), [Cmd::DiscardWorktree]);

        // Asking once is not enough for discard; the second click means it.
        publish(Snapshot {
            worktree: Some(Worktree { branch: "po/nsh".into(), confirm_discard: true }),
            ..Snapshot::default()
        });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        eprintln!("SCREEN={:?}", app.screen().texts());
        // Asking once replaces the label; the confirm press still answers discard.
        app.press("label:Discard everything?").unwrap();
        assert_eq!(next(&mut app), [Cmd::DiscardWorktree]);

        // A PR can be opened: the typed spec goes back trimmed, and the placeholder shows.
        publish(Snapshot {
            pr: Some(Pr::Asking { status: None, opening: false }),
            ..Snapshot::default()
        });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        app.type_text("id:pr-spec", " 42 ").unwrap();
        app.press("label:Open").unwrap();
        app.settle();
        assert_eq!(next(&mut app), [Cmd::OpenPr("42".into())]);

        // Opened: its summary, a way out, and the field Po emptied stays empty.
        publish(Snapshot {
            pr: Some(Pr::Opened { label: "PR #42 · 3 files".into() }),
            pr_reset: 1,
            ..Snapshot::default()
        });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        assert!(app.shows("PR #42 · 3 files"));
        app.press("label:Close").unwrap();
        app.settle();
        assert_eq!(next(&mut app), [Cmd::ClosePr]);
        publish(Snapshot {
            pr: Some(Pr::Asking { status: None, opening: false }),
            ..Snapshot::default()
        });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        app.type_text("id:pr-spec", "12").unwrap();
        app.press("label:Open").unwrap();
        app.settle();
        assert_eq!(next(&mut app), [Cmd::OpenPr("12".into())]);

        // Opening reports itself quietly; an error is the danger color, so at least it shows.
        publish(Snapshot {
            pr: Some(Pr::Asking { status: Some("Checking out #12…".into()), opening: true }),
            ..Snapshot::default()
        });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        for shown in ["…", "Checking out #12…"] {
            assert!(app.shows(shown), "{shown} is on screen: {:?}", app.screen().texts());
        }
        publish(Snapshot {
            pr: Some(Pr::Asking { status: Some("no such PR".into()), opening: false }),
            ..Snapshot::default()
        });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        assert!(app.shows("no such PR"));
    }
}
