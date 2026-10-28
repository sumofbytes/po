//! The viewer showing what the agent put on screen: a markdown document it wrote, or a form the
//! user answers. [`Snapshot`] is what [`crate::Reader`] publishes; the screen draws it and
//! answers a form back through [`Cmd::SubmitForm`].
use std::sync::{Arc, LazyLock};

use nsh_base::text_view;
use nsh_core::prelude::*;
use tokio::sync::watch;

use super::bus::{Cmd, send};
use super::header::{Control, pane_header, status};
use super::source::Shared;
use super::{Colors, body, field_height, note, rems, text_base, text_sm};
use serde_json::{Map, Value};

/// One field of the form on screen.
#[derive(Clone, PartialEq, Debug)]
pub struct Field {
    pub name: String,
    pub label: String,
    /// The options to pick among; empty is a field to type into.
    pub options: Vec<String>,
    /// The text it starts with, or the choice picked to begin with.
    pub value: Option<String>,
}

/// A markdown document the agent wrote, or a form of its the user answers. What a field of the
/// form holds is the screen's own, and it is seeded anew when the `id` or the `fields` of the
/// form change, which is what a replaced form means.
#[derive(Clone, PartialEq, Debug)]
pub enum Body {
    Markdown(Shared),
    Form { id: String, fields: Vec<Field>, submit: String, answered: bool, doc: Option<Shared> },
}

impl Default for Body {
    fn default() -> Self {
        Self::Markdown(Shared(Arc::from("")))
    }
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    pub glass: bool,
    pub title: String,
    pub lead: Vec<Control>,
    pub trail: Vec<Control>,
    pub body: Body,
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

fn markdown_text(shared: Shared) -> impl IntoElement + use<> {
    text_view()
        .markdown(shared.0.to_string())
        .on_link_press(|press: &nsh_base::TextViewLinkPressEvent| send(Cmd::OpenUrl(press.url.to_string())))
        .text_size(text_base())
        .text_color(Colors::foreground())
}

/// A text button: filled with the primary color. The label is the form's own wording, so a
/// [`String`] rather than [`super::button`]'s `&'static str`.
fn submit_button(label: impl IntoReactive<SharedString> + Clone + 'static, on_press: impl Fn() + 'static) -> View {
    view()
        .role(AccessibilityRole::Button)
        .label(label.clone())
        .flex_none()
        .px(rems(0.75))
        .py(rems(0.375))
        .rounded(Colors::radius())
        .cursor_pointer()
        .on_press(on_press)
        .bg(Colors::primary())
        .child(text(label).text_size(text_sm()).text_color(Colors::primary_foreground()))
}

/// A field of the form: its label, then a field to type into, or its options to pick among.
fn field_row(at: usize, field: &Field, drafts: Signal<Vec<String>>) -> View {
    let name = field.name.as_str();
    let widget = if field.options.is_empty() {
        view()
            .w(Points(0.))
            .flex_1()
            .child(
                nsh_core::element::input()
                    .test_id(&format!("form-{name}"))
                    .value(move || drafts.with(|values| values.get(at).cloned().unwrap_or_default()))
                    .placeholder(field.label.as_str())
                    .placeholder_color(Colors::one(Colors::muted()))
                    .selection_color(Colors::one(Colors::caret()))
                    .text_color(Colors::foreground())
                    .auto_correct(false)
                    .on_change(move |typed: &str| drafts.update(|values| values[at] = typed.to_string()))
                    .h(field_height())
                    .px(rems(0.625))
                    .rounded(Colors::radius())
                    .border_1()
                    .border_color(Colors::input_border())
                    .bg(Colors::input_background()),
            )
            .into_element()
    } else {
        view()
            .flex_1()
            .flex_wrap()
            .gap(rems(0.25))
            .children(field.options.iter().map(|option| {
                let option = option.clone();
                let alternative = option.clone();
                let selected = option.clone();
                let picked = move || drafts.with(|values| values.get(at).is_some_and(|value| *value == alternative));
                view()
                    .role(AccessibilityRole::Button)
                    .label(option.clone())
                    .px(rems(0.75))
                    .py(rems(0.25))
                    .rounded(Colors::radius())
                    .cursor_pointer()
                    .hover(|s| s.bg(Colors::hover()))
                    .style_when(picked, |s| s.bg(Colors::selected()))
                    .on_press(move || drafts.update(|values| values[at] = selected.clone()))
                    .child(body(option.clone()))
            }))
            .into_element()
    };
    view()
        .flex_row()
        .items_center()
        .gap(rems(0.5))
        .child(
            view()
                .w(rems(9.))
                .flex_none()
                .child(note(field.label.clone()).number_of_lines(1)),
        )
        .child(widget)
}

#[component]
pub fn Agent() -> impl IntoElement {
    let state = signal(SNAPSHOT.borrow().clone());
    // What the form's fields hold, parallel to them: what a text field holds, or the choice
    // picked. Kept while the form stays the same (see the loop below), so the redraws of the
    // pane around it cost nothing to the user's typing.
    let drafts = signal(Vec::<String>::new());
    spawn(async move {
        let mut snapshot = SNAPSHOT.subscribe();
        // What was published before this ran would otherwise never arrive.
        snapshot.mark_changed();
        // The form the drafts were seeded from last, by id and fields.
        let mut seeded: Option<(String, Vec<Field>)> = None;
        while snapshot.changed().await.is_ok() {
            let now = snapshot.borrow_and_update().clone();
            if let Body::Form { id, fields, .. } = &now.body {
                let same = seeded.as_ref().is_some_and(|(old_id, old_fields)| old_id == id && old_fields == fields);
                if !same {
                    seeded = Some((id.clone(), fields.clone()));
                    drafts.set(fields.iter().map(|field| field.value.clone().unwrap_or_default()).collect());
                }
            }
            state.set(now);
        }
    });

    // The answers by field name, empty ones left out. An answered form sends nothing more.
    let answer = move || {
        let answering = state.with(|s| match &s.body {
            Body::Form { id, fields, answered, .. } if !*answered => Some((id.clone(), fields.clone())),
            _ => None,
        });
        let Some((id, fields)) = answering else { return };
        let draft = drafts.get_untracked();
        let mut values = Map::new();
        for (field, value) in fields.iter().zip(draft.iter()) {
            let value = value.trim();
            if !value.is_empty() {
                values.insert(field.name.clone(), Value::String(value.to_string()));
            }
        }
        send(Cmd::SubmitForm(id, Value::Object(values)));
    };

    let header = move || {
        let (title, lead, trail, glass, form) = state.with(|s| (s.title.clone(), s.lead.clone(), s.trail.clone(), s.glass, matches!(s.body, Body::Form { .. })));
        let note = if form { "a form from the agent" } else { "from the agent" };
        pane_header(title, status(note), &lead, &trail, glass)
    };

    let body = move || match state.with(|s| s.body.clone()) {
        Body::Markdown(shared) => scroll()
            .flex_1()
            .child(
                view()
                    .flex()
                    .justify_center()
                    .child(
                        view()
                            .w_full()
                            .max_w(px(820.))
                            .px(rems(1.))
                            .pt(rems(1.))
                            .pb(rems(1.25))
                            .child(markdown_text(shared.clone())),
                    )
                    .into_element(),
            )
            .into_element(),
        Body::Form { fields, submit, answered, doc, .. } => {
            let column = view()
                .w_full()
                .max_w(px(640.))
                .flex_col()
                .gap(rems(0.75))
                .px(rems(1.))
                .children(doc.map(|doc| markdown_text(doc)))
                .children(fields.iter().enumerate().map(move |(index, field)| {
                    field_row(index, field, drafts)
                }))
                .child(
                    view()
                        .flex_row()
                        .justify_end()
                        .pt(rems(0.5))
                        .border_t_1()
                        .border_color(Colors::border())
                        .child(if answered {
                            // The agent has the answers; the button could be pressed again, but
                            // answers nothing.
                            submit_button("Answered", move || answer()).flex_none().opacity(crate::theme::tokens::DISABLED).into_element()
                        } else {
                            submit_button(submit.clone(), move || answer()).into_element()
                        }),
                );
            scroll()
                .flex_1()
                .child(view().flex_col().items_center().py(rems(1.)).child(column))
                .into_element()
        }
    };

    view()
        .flex_1()
        .bg(Colors::background())
        .child(dynamic(header))
        .child(dynamic(body))
}

#[cfg(test)]
mod tests {
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;

    #[test]
    fn the_agents_view_renders_markdown_and_holds_a_form_and_reports_its_answer() {
        let mut guard = super::super::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let markdown = Shared(Arc::from("# Plan\n\nSome *words* [a link](https://po)."));
        publish(Snapshot {
            title: "Plan".into(),
            body: Body::Markdown(markdown),
            ..Snapshot::default()
        });
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Agent>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(700.), Points(600.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app.settle();
        for shown in ["Plan", "from the agent"] {
            assert!(app.shows(shown), "{shown} is on screen: {:?}", app.screen().texts());
        }

        // A form: what it is for above the fields, choices to pick, answers to type.
        publish(Snapshot {
            title: "What next?".into(),
            body: Body::Form {
                id: "f1".into(),
                fields: vec![
                    Field { name: "topic".into(), label: "What to work on".into(), options: vec![], value: Some("the nsh migration".into()) },
                    Field { name: "size".into(), label: "How big a step".into(), options: vec!["Small".into(), "Whole".into()], value: None },
                ],
                submit: "Go".into(),
                answered: false,
                doc: Some(Shared(Arc::from("A *form* from me."))),
            },
            ..Snapshot::default()
        });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        for shown in ["What next?", "a form from the agent", "A form from me.", "What to work on", "Small", "Whole", "Go"] {
            assert!(app.shows(shown), "{shown} is on screen: {:?}", app.screen().texts());
        }

        // The typing is kept as the pane around it redraws, and the answers trimmed as they are.
        app.press("label:Whole").unwrap();
        app.wait(std::time::Duration::from_millis(50)).unwrap();
        publish(Snapshot {
            title: "What next?".into(),
            body: Body::Form {
                id: "f1".into(),
                fields: vec![
                    Field { name: "topic".into(), label: "What to work on".into(), options: vec![], value: Some("the nsh migration".into()) },
                    Field { name: "size".into(), label: "How big a step".into(), options: vec!["Small".into(), "Whole".into()], value: None },
                ],
                submit: "Go".into(),
                answered: false,
                doc: Some(Shared(Arc::from("A *form* from me."))),
            },
            ..Snapshot::default()
        });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        assert_eq!(app.press("label:Go"), Ok(()));
        app.settle();
        assert_eq!(commands.try_recv().ok(), Some(Cmd::SubmitForm("f1".into(), serde_json::json!({ "topic": "the nsh migration", "size": "Whole" }))));
    }
}
