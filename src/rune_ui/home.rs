//! The Home page, where Po opens: what is due today, what is late and what is ready to pick up
//! across every open project, and the threads to pick up. Counterpart of `src/nsh/home.rs`; the data is
//! [`crate::model::home`]'s.
use rune::{El, ScrollSpec, col, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use super::work::{
    body_style, card, metric, muted, priority_tint, small_style, status_glyph, status_tint,
    title_style,
};
use crate::issues::date::{Date, due_label};
use crate::model::bus::{Cmd, send};
use crate::model::home::{Model, Pickup, TASKS, Task, complete, greeting};
use crate::model::sidebar::Mark;

/// Marking an issue done can't be undone, so it waits for a second press: this is the one asked.
#[derive(Default)]
pub struct State {
    armed: Option<(std::path::PathBuf, u64)>,
}

pub type At<S> = fn(&mut S) -> &mut State;

fn task_row<S: 'static>(k: &Kit, task: &Task, today: Date, armed: bool, at: At<S>) -> El {
    let (repo, number) = (task.repo.clone(), task.number);
    let late = task.late(today);
    let due = task
        .due
        .map(|due| due_label(due, today))
        .unwrap_or_else(|| task.status.slug().replace('-', " "));
    let (done, open, work) = (repo.clone(), repo.clone(), repo);
    let mark = if armed {
        "Confirm"
    } else {
        status_glyph(task.status)
    };
    let tint = if armed {
        k.c.warning.fg
    } else {
        status_tint(k, task.status)
    };
    row()
        .w_full()
        .items_center()
        .gap(10.0)
        .px(8.0)
        .h(36.0)
        .radius(6.0)
        .child(
            k.interactive(
                row()
                    .key(format!("done-{}", task.key))
                    .child(text(mark).font(small_style()).color(tint))
                    .on_press(move |s: &mut S| {
                        let state = at(s);
                        if state.armed.as_ref() == Some(&(done.clone(), number)) {
                            complete(&done, number);
                            state.armed = None;
                        } else {
                            state.armed = Some((done.clone(), number));
                        }
                    }),
            ),
        )
        .child(
            row()
                .w(3.0)
                .h(18.0)
                .radius(2.0)
                .fill(priority_tint(k, task.priority)),
        )
        .child(
            k.interactive(
                row()
                    .key(format!("task-{}", task.key))
                    .grow(1.0)
                    .min_w(0.0)
                    .child(
                        text(task.title.clone())
                            .font(body_style())
                            .color(k.c.text_base)
                            .truncate(),
                    )
                    .on_press(move |_: &mut S| send(Cmd::OpenIssue(open.clone(), number))),
            ),
        )
        .child(
            Button::new("Work on this")
                .variant(ButtonVariant::Ghost)
                .on_press(move |_: &mut S| send(Cmd::WorkOnIssue(work.clone(), number)))
                .build(k),
        )
        .child(muted(k, task.project.clone()))
        .child(
            row()
                .w(64.0)
                .justify_end()
                .child(text(due).font(small_style()).color(if late {
                    k.c.danger.fg
                } else {
                    k.c.text_muted
                })),
        )
}

fn mark_note(k: &Kit, mark: Mark) -> (&'static str, rune::Color) {
    match mark {
        Mark::Asking => ("Needs you", k.c.warning.fg),
        Mark::Failed => ("Failed", k.c.danger.fg),
        Mark::Unread => ("New reply", k.c.success.fg),
        Mark::Running => ("Working", k.c.text_muted),
        Mark::Idle => ("", k.c.text_muted),
    }
}

fn thread_row<S: 'static>(k: &Kit, thread: &Pickup) -> El {
    let (project, id) = (thread.project, thread.id);
    let (status, tint) = mark_note(k, thread.mark);
    let meta = [
        Some(thread.owner.clone()),
        (!status.is_empty()).then(|| status.to_string()),
        thread.ago.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    row()
        .w_full()
        .items_center()
        .gap(8.0)
        .py(6.0)
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .child(
                    text(thread.label.clone())
                        .font(body_style())
                        .color(k.c.text_base)
                        .truncate(),
                )
                .child(text(meta).font(small_style()).color(tint).truncate()),
        )
        .child(
            row().w(72.0).justify_end().items_center().child(
                Button::new("Resume")
                    .variant(ButtonVariant::Outline)
                    .control_size(ControlSize::Small)
                    .on_press(move |_: &mut S| send(Cmd::OpenThread(project, id)))
                    .build(k),
            ),
        )
}

pub fn view<S: 'static>(model: &Model, state: &State, k: &Kit, at: At<S>) -> El {
    let today = crate::issues::date::today();
    let mut tasks = card(k, "Today");
    if model.tasks.is_empty() {
        tasks = tasks.child(muted(
            k,
            "Nothing is due today. Pick something from the issues, or start a thread.",
        ));
    }
    for task in model.tasks.iter().take(TASKS) {
        let armed = state.armed.as_ref() == Some(&(task.repo.clone(), task.number));
        tasks = tasks.child(task_row::<S>(k, task, today, armed, at));
    }
    let more = model.tasks.len().saturating_sub(TASKS);
    if more > 0 {
        tasks = tasks.child(
            k.interactive(
                row()
                    .key("more-issues")
                    .px(8.0)
                    .py(4.0)
                    .child(muted(k, format!("{more} more in Issues")))
                    .on_press(|_: &mut S| send(Cmd::ToggleIssues)),
            ),
        );
    }
    let mut threads = card(k, "Threads");
    if model.threads.is_empty() {
        threads = threads.child(muted(k, "No threads yet."));
    }
    for thread in &model.threads {
        threads = threads.child(thread_row::<S>(k, thread));
    }
    let side = col()
        .w(340.0)
        .gap(16.0)
        .child(
            card(k, "Start").child(
                row()
                    .gap(8.0)
                    .child(
                        Button::new("New thread")
                            .variant(ButtonVariant::Submit)
                            .on_press(|_: &mut S| send(Cmd::NewThread))
                            .build(k),
                    )
                    .child(
                        Button::new("Open issues")
                            .variant(ButtonVariant::Ghost)
                            .on_press(|_: &mut S| send(Cmd::ToggleIssues))
                            .build(k),
                    ),
            ),
        )
        .child(threads);
    let metrics = row()
        .w_full()
        .gap(12.0)
        .child(metric(k, "Due today", model.due_today, k.c.text_base))
        .child(metric(
            k,
            "Late",
            model.late,
            if model.late > 0 {
                k.c.danger.fg
            } else {
                k.c.text_muted
            },
        ))
        .child(metric(k, "In progress", model.in_progress, k.c.warning.fg))
        .child(metric(
            k,
            "Agent threads",
            model.active_threads,
            k.c.info.fg,
        ));
    let page = col()
        .w_full()
        .max_w(1000.0)
        .gap(20.0)
        .padding(28.0)
        .child(
            col()
                .gap(4.0)
                .child(muted(k, today.long()))
                .child(
                    text(greeting(crate::issues::date::hour()))
                        .font(title_style())
                        .color(k.c.text_base),
                )
                .child(muted(k, "A clear view of what matters today.")),
        )
        .child(metrics)
        .child(
            row()
                .w_full()
                .gap(16.0)
                .items_start()
                .child(col().grow(1.0).min_w(0.0).child(tasks))
                .child(side),
        );
    col().w_full().h_full().min_h(0.0).child(
        k.scroll_bar(
            col()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .items_center()
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(page),
        ),
    )
}
