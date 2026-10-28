//! The epics: a list of them with how far along each is, and one epic's page with its
//! milestones and issues. Drawn with [`crate::nsh::issues`]'s pieces and state.
use nsh_core::prelude::*;

use super::issues::{State, Page, boxed, caption, chip, delete_button, due_hint, issue_row, line_input, text_area};
use super::{Colors, icon, icon_button, rems, text_sm, text_xs};
use crate::issues::shared;
use crate::issues::{EpicStatus, Progress};
use crate::date::{Date, due_label, parse_due, today};
use crate::ui::icon::IconName;

const DUE_HINT: &str = "Due: fri, nov 2, 2026-12-01…";

impl State {
    /// Opens an epic's page, with its fields holding what it has.
    pub fn open_epic(self, id: &str) {
        let from = self.page.get_untracked();
        if !matches!(from, Page::Epic(_)) {
            self.back_to.set(from);
        }
        let (name, description, target) = self.read(|data| data.tracker.epic(id).map_or_else(Default::default, |e| (e.name.clone(), e.description.clone(), e.target.map(Date::iso).unwrap_or_default())));
        self.epic_name.set(name);
        self.epic_description.set(description);
        self.epic_target.set(target);
        self.milestone_name.set(String::new());
        self.milestone_due.set(String::new());
        self.arm_delete.set(false);
        self.page.set(Page::Epic(id.to_string()));
    }

    // As for an issue's page, each field is saved alone as it changes.

    fn save_epic_name(self, id: &str) {
        let name = self.epic_name.get_untracked().trim().to_string();
        if !name.is_empty() {
            shared::change(|tracker, now| tracker.update_epic(id, now, |epic| epic.name = name));
        }
    }

    fn save_epic_description(self, id: &str) {
        let description = self.epic_description.get_untracked();
        shared::change(|tracker, now| tracker.update_epic(id, now, |epic| epic.description = description));
    }

    fn save_epic_target(self, id: &str) {
        if let Some(target) = parse_due(&self.epic_target.get_untracked(), today()) {
            shared::change(|tracker, now| tracker.update_epic(id, now, |epic| epic.target = target));
        }
    }

    /// Deletes an epic on the second press, as an issue is.
    fn delete_epic(self, id: &str) {
        if !self.arm_delete.get_untracked() {
            return self.arm_delete.set(true);
        }
        self.arm_delete.set(false);
        self.back_to.set(Page::Epics);
        self.back();
        shared::change(|tracker, now| tracker.remove_epic(id, now));
    }

    fn add_epic(self) {
        let name = self.epic_name.get_untracked().trim().to_string();
        if name.is_empty() {
            return;
        }
        if let Some(id) = shared::change(|tracker, now| tracker.create_epic(&name, now)) {
            self.epic_name.set(String::new());
            self.open_epic(&id);
        }
    }
}

/// A bar of how much is done, started, and left.
pub fn bar(progress: Progress) -> View {
    let part = |amount: usize, tint: fn() -> AdaptiveColor| view().flex_1().flex_grow(amount as f32).h_full().bg(tint);
    let left = progress.all - progress.done - progress.started;
    view()
        .flex_1()
        .h(rems(0.375))
        .flex_row()
        .rounded(Colors::radius())
        .overflow_hidden()
        .bg(Colors::border())
        .child(part(progress.done, || Colors::added()()))
        .child(part(progress.started, || Colors::warning()()))
        .child(part(left, || Colors::border()()).opacity(0.))
}

fn count(progress: Progress) -> String {
    if progress.all == 0 { "no issues".into() } else { format!("{}/{}", progress.done, progress.all) }
}

pub fn epics_page(state: State) -> View {
    let has_project = move || state.data.with(|data| data.root.is_some());
    view().flex_1().child(when(has_project, move || {
        view()
            .flex_1()
            .child(view().px(rems(1.)).pt(rems(0.75)).pb(rems(0.5)).child(boxed(IconName::Plus, line_input("epic-new-name", state.epic_name, "New epic", move || state.add_epic()).auto_focus(true), None)))
            .child(scroll().flex_shrink(1.).child(dynamic(move || {
                let rows: Vec<View> = state.data.with(|data| {
                    data.tracker
                        .epics()
                        .iter()
                        .map(|epic| {
                            let (id, progress) = (epic.id.clone(), data.tracker.epic_progress(&epic.id));
                            view()
                                .test_id(&format!("epic-{}", epic.id))
                                .h(rems(2.25))
                                .flex_row()
                                .items_center()
                                .gap(rems(0.75))
                                .px(rems(0.625))
                                .rounded(Colors::radius())
                                .cursor_pointer()
                                .hover(|s| s.bg(Colors::hover()))
                                .on_press(move || state.open_epic(&id))
                                .child(icon(IconName::Layers, rems(0.875), Colors::muted()))
                                .child(text(epic.name.clone()).flex_1().number_of_lines(1).text_color(Colors::foreground()))
                                .child(text(epic.status.name()).flex_none().text_size(text_xs()).text_color(Colors::muted()))
                                .child(view().w(rems(8.)).flex_none().flex_row().child(bar(progress)))
                                .child(text(count(progress)).flex_none().w(rems(4.)).text_size(text_xs()).text_color(Colors::muted()))
                                .child(text(epic.target.map(|t| due_label(t, today())).unwrap_or_default()).flex_none().w(rems(4.5)).text_size(text_xs()).text_color(Colors::muted()))
                        })
                        .collect()
                });
                let empty = rows.is_empty();
                view()
                    .gap(rems(0.125))
                    .px(rems(0.75))
                    .pb(rems(0.75))
                    .child(rows)
                    .child(empty.then(|| view().py(rems(1.5)).items_center().child(text("No epics yet. An epic groups issues toward a goal.").text_size(text_sm()).text_color(Colors::muted()))))
            })))
    }).otherwise(|| view().py(rems(1.5)).items_center().child(text("Open a project to see its epics.").text_size(text_sm()).text_color(Colors::muted()))))
}

pub fn epic_page(state: State, id: String) -> View {
    let exists = {
        let id = id.clone();
        move || state.data.with(|data| data.tracker.epic(&id).is_some())
    };
    let shown = id.clone();
    view().flex_1().child(when(exists, move || epic_body(state, shown.clone())).otherwise(move || {
        view().p(rems(1.5)).gap(rems(0.75)).items_center().child(text("This epic is gone.").text_color(Colors::muted())).child(super::button("Back", false, move || state.back()))
    }))
}

fn epic_body(state: State, id: String) -> AnyElement {
    let (statuses, milestones, issues, overall) = {
        let id = id.clone();
        (
            {
                let id = id.clone();
                move || {
                    let now = state.data.with(|data| data.tracker.epic(&id).map(|e| e.status));
                    let chips: Vec<View> = EpicStatus::ALL
                        .iter()
                        .map(|s| {
                            let (s, id) = (*s, id.clone());
                            chip(s.name(), move || now == Some(s), move || { shared::change(|t, n| t.update_epic(&id, n, |e| e.status = s)); })
                        })
                        .collect();
                    view().flex_row().flex_wrap().gap(rems(0.25)).child(chips)
                }
            },
            {
                let id = id.clone();
                move || {
                    let rows: Vec<View> = state.data.with(|data| {
                        data.tracker
                            .epic(&id)
                            .map(|epic| {
                                epic.milestones
                                    .iter()
                                    .map(|m| {
                                        let (epic_id, mid) = (id.clone(), m.id.clone());
                                        let progress = data.tracker.milestone_progress(&id, &m.id);
                                        view()
                                            .test_id(&format!("milestone-{}", m.id))
                                            .group()
                                            .h(rems(2.))
                                            .flex_row()
                                            .items_center()
                                            .gap(rems(0.75))
                                            .px(rems(0.5))
                                            .child(icon(IconName::Flag, rems(0.75), Colors::muted()))
                                            .child(text(m.name.clone()).flex_1().number_of_lines(1).text_color(Colors::foreground()))
                                            .child(text(m.target.map(|t| due_label(t, today())).unwrap_or_default()).flex_none().text_size(text_xs()).text_color(Colors::muted()))
                                            .child(view().w(rems(7.)).flex_none().flex_row().child(bar(progress)))
                                            .child(text(count(progress)).flex_none().w(rems(4.)).text_size(text_xs()).text_color(Colors::muted()))
                                            .child(super::on_hover(icon_button(IconName::Close, rems(0.75), "Remove milestone", move || {
                                                shared::change(|t, n| t.update_epic(&epic_id, n, |e| e.milestones.retain(|m| m.id != mid)));
                                            })))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    });
                    view().gap(rems(0.125)).child(rows)
                }
            },
            {
                let id = id.clone();
                move || {
                    let rows: Vec<View> = state.data.with(|data| {
                        let filter = crate::issues::query::Filter { scope: crate::issues::query::Scope::All, epic: Some(id.clone()), ..Default::default() };
                        crate::issues::query::sections(&data.tracker, &filter, crate::issues::query::Group::None, today()).into_iter().flat_map(|s| s.issues).map(|n| issue_row(state, n)).collect()
                    });
                    view().gap(rems(0.125)).child(rows)
                }
            },
            {
                let id = id.clone();
                move || view().flex_row().items_center().gap(rems(0.75)).child(bar(state.data.with(|data| data.tracker.epic_progress(&id)))).child(text(count(state.data.with(|data| data.tracker.epic_progress(&id)))).text_size(text_xs()).text_color(Colors::muted()))
            },
        )
    };
    let (delete_id, add_id, ms_id) = (id.clone(), id.clone(), id.clone());
    let add_milestone = move || {
        let name = state.milestone_name.get_untracked().trim().to_string();
        let Some(due) = parse_due(&state.milestone_due.get_untracked(), today()) else { return };
        if name.is_empty() {
            return;
        }
        shared::change(|t, n| t.update_epic(&ms_id, n, |e| { e.add_milestone(&name, due); }));
        state.milestone_name.set(String::new());
        state.milestone_due.set(String::new());
    };
    let add_milestone_due = add_milestone.clone();
    scroll().flex_shrink(1.).child(
        view()
            .gap(rems(0.75))
            .px(rems(1.))
            .py(rems(0.75))
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .gap(rems(0.5))
                    .child(icon_button(IconName::ChevronLeft, rems(0.875), "Back (Esc)", move || state.back()))
                    .child(text(id.clone()).flex_1().text_size(text_sm()).text_color(Colors::muted()))
                    .child(delete_button(state, "Delete epic", move || state.delete_epic(&delete_id))),
            )
            .child(boxed(IconName::Pencil, line_input("epic-name", state.epic_name, "Name", {
                let id = id.clone();
                move || state.save_epic_name(&id)
            }).on_change({
                let id = id.clone();
                move |text: &str| {
                    state.epic_name.set(text.to_string());
                    state.save_epic_name(&id);
                }
            }), None))
            .child(text_area("epic-description", state.epic_description, "Description (markdown)", {
                let id = id.clone();
                move |text| {
                    state.epic_description.set(text.to_string());
                    state.save_epic_description(&id);
                }
            }))
            .child(view().flex_row().items_center().gap(rems(0.75)).child(view().w(rems(5.5)).flex_none().child(caption("Status"))).child(dynamic(statuses)))
            .child(
                view().flex_row().items_center().gap(rems(0.75)).child(view().w(rems(5.5)).flex_none().child(caption("Target"))).child(
                    view().flex_1().child(boxed(IconName::Calendar, line_input("epic-target", state.epic_target, DUE_HINT, {
                        let id = id.clone();
                        move || state.save_epic_target(&id)
                    }).on_change({
                        let id = id.clone();
                        move |text: &str| {
                            state.epic_target.set(text.to_string());
                            state.save_epic_target(&id);
                        }
                    }), Some(due_hint(state.epic_target)))),
                ),
            )
            .child(view().flex_row().items_center().gap(rems(0.75)).child(view().w(rems(5.5)).flex_none().child(caption("Progress"))).child(view().flex_1().child(dynamic(overall))))
            .child(caption("Milestones"))
            .child(dynamic(milestones))
            .child(boxed(
                IconName::Flag,
                line_input("milestone-name", state.milestone_name, "New milestone", add_milestone),
                Some(view().flex_none().w(rems(11.)).flex_row().items_center().gap(rems(0.5)).child(line_input("milestone-due", state.milestone_due, DUE_HINT, add_milestone_due)).child(due_hint(state.milestone_due))),
            ))
            .child(caption("Issues"))
            .child(boxed(IconName::Plus, line_input("epic-issue-title", state.new_title, "New issue in this epic", move || state.add(Some(add_id.clone()))), None))
            .child(dynamic(issues)),
    )
    .into_element()
}

