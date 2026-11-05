//! The epics: a list of them with how far along each is, and one epic's page with its
//! milestones and issues. Drawn with [`crate::nsh::issues`]'s pieces and state.
use nsh_core::prelude::*;

use super::issues::{State, Page, Field, editable, markdown_box, stop_editing, Tools, add_box, toolbar, boxed, chip, delete_button, due_hint, issue_row, legend, line_input, project_pill, text_area};
use super::{Colors, caption, icon, icon_button, note, scale, Sizing, Smooth};
use crate::issues::shared;
use crate::issues::{EpicStatus, Progress};
use crate::date::{Date, due_label, parse_due, today};
use crate::ui::icon::IconName;

const DUE_HINT: &str = "Due: fri, nov 2, 2026-12-01…";

impl State {
    /// Opens an epic of the repo `repo` on its page; Back comes back to the list of epics.
    pub fn open_epic_in(self, repo: &std::path::Path, id: &str) {
        self.flush();
        shared::set_project(repo);
        self.follow(shared::watch().borrow().clone());
        self.open_epic(id);
    }

    /// Opens an epic's page, with its fields holding what it has.
    pub fn open_epic(self, id: &str) {
        self.flush();
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
        self.editing.set(None);
        self.page.set(Page::Epic(id.to_string()));
    }

    // As for an issue's page, each field is saved alone as it changes.

    pub fn save_epic_name(self, id: &str) {
        let name = self.epic_name.get_untracked().trim().to_string();
        if !name.is_empty() {
            self.save(|tracker, now| tracker.update_epic(id, now, |epic| epic.name = name));
        }
    }

    pub fn save_epic_description(self, id: &str) {
        let description = self.epic_description.get_untracked();
        self.save(|tracker, now| tracker.update_epic(id, now, |epic| epic.description = description));
    }

    pub fn save_epic_target(self, id: &str) {
        if let Some(target) = parse_due(&self.epic_target.get_untracked(), today()) {
            self.save(|tracker, now| tracker.update_epic(id, now, |epic| epic.target = target));
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
        self.save(|tracker, now| tracker.remove_epic(id, now));
    }

    fn add_epic(self) {
        let name = self.epic_name.get_untracked().trim().to_string();
        if name.is_empty() {
            return;
        }
        let Some(repo) = self.target() else { return };
        let id = shared::change_repo(&repo, |tracker, now| tracker.create_epic(&name, now));
        self.epic_name.set(String::new());
        self.open_epic_in(&repo, &id);
    }
}

/// A bar of how much is done, started, and left.
pub fn bar(progress: Progress) -> View {
    let part = |amount: usize, tint: fn() -> AdaptiveColor| view().flex_1().flex_grow(amount as f32).h_full().bg(tint);
    let left = progress.all - progress.done - progress.started;
    view()
        .flex_1()
        .h(scale::s1_5())
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
    let has_project = move || state.all.with(|all| !all.repos.is_empty());
    view().flex_1().child(when(has_project, move || {
        view()
            .flex_1()
            .child(view().px(scale::s4()).pt(scale::s3()).pb(scale::s2()).child(add_box(line_input("epic-new-name", state.epic_name, "New epic", move || state.add_epic()).auto_focus(true), move || project_pill(state))))
            .child(toolbar(state, Tools::PROJECT))
            .child(scroll().flex_shrink(1.).child(dynamic(move || {
                let several = state.all.with(|all| all.repos.len() > 1);
                let (rows, empty): (Vec<View>, bool) = state.visible(|repos| {
                    let mut rows = vec![];
                    for repo in repos.iter().filter(|repo| !repo.tracker.epics().is_empty()) {
                        if several {
                            rows.push(view().px(scale::s2_5()).pt(scale::s3()).pb(scale::s1()).child(legend_text(repo.name.clone())));
                        }
                        for epic in repo.tracker.epics() {
                            let (root, id, progress) = (repo.root.clone(), epic.id.clone(), repo.tracker.epic_progress(&epic.id));
                            rows.push(
                                view()
                                    .test_id(&format!("epic-{}-{}", repo.name, epic.id))
                                    .h(scale::s8())
                                    .flex_row()
                                    .items_center()
                                    .gap(scale::s2())
                                    .px(scale::s2_5())
                                    .rounded(Colors::radius())
                                    .cursor_pointer()
                                    .smooth()
                                    .hover(|s| s.bg(Colors::hover()))
                                    .on_press(move || state.open_epic_in(&root, &id))
                                    .child(icon(IconName::Layers, scale::s3(), Colors::muted()))
                                    .child(text(epic.name.clone()).flex_1().number_of_lines(1).text_color(Colors::foreground()))
                                    .child(caption(epic.status.name()).flex_none())
                                    .child(view().w(scale::bar_width()).flex_none().flex_row().child(bar(progress)))
                                    .child(caption(count(progress)).flex_none().w(scale::count_width()))
                                    .child(caption(epic.target.map(|t| due_label(t, today())).unwrap_or_default()).flex_none().w(scale::due_width())),
                            );
                        }
                    }
                    let empty = rows.is_empty();
                    (rows, empty)
                });
                view()
                    .gap(scale::s0_5())
                    .px(scale::s3())
                    .pb(scale::s3())
                    .child(rows)
                    .child(empty.then(|| view().py(scale::s6()).items_center().child(note("No epics yet. An epic groups issues toward a goal."))))
            })))
    }).otherwise(|| view().py(scale::s6()).items_center().child(note("Open a project to see its epics."))))
}

/// A project's name over its epics.
fn legend_text(name: String) -> Text {
    text(name).text_size(super::text_xs()).font_weight(FontWeight::SEMIBOLD).text_color(Colors::muted())
}

pub fn epic_page(state: State, id: String) -> View {
    let exists = {
        let id = id.clone();
        move || state.data.with(|data| data.tracker.epic(&id).is_some())
    };
    let shown = id.clone();
    view().flex_1().child(when(exists, move || epic_body(state, shown.clone())).otherwise(move || {
        view().p(scale::s6()).gap(scale::s3()).items_center().child(text("This epic is gone.").text_color(Colors::muted())).child(super::button("Back", false, move || state.back()))
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
                            chip(s.name(), move || now == Some(s), move || { state.save(|t, n| t.update_epic(&id, n, |e| e.status = s)); })
                        })
                        .collect();
                    view().flex_row().flex_wrap().gap(scale::s1()).child(chips)
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
                                            .h(scale::s8())
                                            .flex_row()
                                            .items_center()
                                            .gap(scale::s2())
                                            .px(scale::s2_5())
                                            .child(icon(IconName::Flag, scale::s3(), Colors::muted()))
                                            .child(text(m.name.clone()).flex_1().number_of_lines(1).text_color(Colors::foreground()))
                                            .child(view().w(scale::bar_width()).flex_none().flex_row().child(bar(progress)))
                                            .child(caption(count(progress)).flex_none().w(scale::count_width()))
                                            .child(caption(m.target.map(|t| due_label(t, today())).unwrap_or_default()).flex_none().w(scale::due_width()))
                                            .child(super::on_hover(icon_button(IconName::Close, scale::s3(), "Remove milestone", move || {
                                                state.save(|t, n| t.update_epic(&epic_id, n, |e| e.milestones.retain(|m| m.id != mid)));
                                            })))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    });
                    view().gap(scale::s0_5()).child(rows)
                }
            },
            {
                let id = id.clone();
                move || {
                    let rows: Vec<View> = state.data.with(|data| {
                        let filter = crate::issues::query::Filter { scope: crate::issues::query::Scope::All, epic: Some(id.clone()), ..Default::default() };
                        crate::issues::query::sections(&data.tracker, &filter, crate::issues::query::Group::None, today()).into_iter().flat_map(|s| s.issues).map(|n| issue_row(state, n)).collect()
                    });
                    view().gap(scale::s0_5()).child(rows)
                }
            },
            {
                let id = id.clone();
                move || view().flex_row().items_center().gap(scale::s3()).child(bar(state.data.with(|data| data.tracker.epic_progress(&id)))).child(caption(count(state.data.with(|data| data.tracker.epic_progress(&id)))))
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
        state.save(|t, n| t.update_epic(&ms_id, n, |e| { e.add_milestone(&name, due); }));
        state.milestone_name.set(String::new());
        state.milestone_due.set(String::new());
    };
    let add_milestone_due = add_milestone.clone();
    scroll().flex_shrink(1.).child(
        view()
            .gap(scale::s3())
            .px(scale::s4())
            .py(scale::s3())
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .gap(scale::s2())
                    .child(icon_button(IconName::ChevronLeft, scale::s3_5(), "Back (Esc)", move || state.back()))
                    .child(note(id.clone()).flex_1())
                    .child(delete_button(state, "Delete epic", move || state.delete_epic(&delete_id))),
            )
            .child(editable(
                state,
                Field::EpicName,
                move || {
                    let name = state.epic_name.get();
                    let empty = name.trim().is_empty();
                    view().min_h(scale::s10()).justify_center().child(
                        text(if empty { "Unnamed (double-click to edit)".to_string() } else { name })
                            .sized(super::font(1.25))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(move || if empty { Colors::muted()() } else { Colors::foreground()() }),
                    )
                },
                move || {
                    boxed(IconName::Pencil, line_input("epic-name", state.epic_name, "Name", move || stop_editing(state)).auto_focus(true).on_blur(move || stop_editing(state)).on_change(move |text: &str| {
                        state.epic_name.set(text.to_string());
                        state.touch(|edited| edited.epic_name = true);
                    }), None)
                    .into_element()
                },
            ))
            .child(editable(
                state,
                Field::EpicDescription,
                move || markdown_box(state.epic_description, "No description. Double-click to write one (markdown)."),
                move || {
                    text_area("epic-description", state.epic_description, "Description (markdown)", move |text| {
                        state.epic_description.set(text.to_string());
                        state.touch(|edited| edited.epic_description = true);
                    }, move || stop_editing(state))
                    .into_element()
                },
            ))
            .child(view().flex_row().items_center().gap(scale::s3()).child(view().w(scale::legend_width()).flex_none().child(legend("Status"))).child(dynamic(statuses)))
            .child(
                view().flex_row().items_center().gap(scale::s3()).child(view().w(scale::legend_width()).flex_none().child(legend("Target"))).child(
                    view().flex_1().child(boxed(IconName::Calendar, line_input("epic-target", state.epic_target, DUE_HINT, move || state.flush()).on_change(move |text: &str| {
                        state.epic_target.set(text.to_string());
                        state.touch(|edited| edited.epic_target = true);
                    }), Some(due_hint(state.epic_target)))),
                ),
            )
            .child(view().flex_row().items_center().gap(scale::s3()).child(view().w(scale::legend_width()).flex_none().child(legend("Progress"))).child(view().flex_1().child(dynamic(overall))))
            .child(legend("Milestones"))
            .child(dynamic(milestones))
            .child(boxed(
                IconName::Flag,
                line_input("milestone-name", state.milestone_name, "New milestone", add_milestone),
                Some(view().flex_none().w(scale::due_entry_width()).flex_row().items_center().gap(scale::s2()).child(line_input("milestone-due", state.milestone_due, DUE_HINT, add_milestone_due)).child(due_hint(state.milestone_due))),
            ))
            .child(legend("Issues"))
            .child(boxed(IconName::Plus, line_input("epic-issue-title", state.new_title, "New issue in this epic", move || state.add(Some(add_id.clone()))), None))
            .child(dynamic(issues)),
    )
    .into_element()
}

