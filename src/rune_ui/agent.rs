//! What the agent put on screen, on Rune: a markdown document it wrote, or a form the user
//! answers through [`Cmd::SubmitForm`]. Counterpart of `src/nsh/agent.rs`.
use rune::tok::TextStyle;
use rune::{El, ScrollSpec, TextEdit, col, row, text};
use rune_kit::markdown::Markdown;
use rune_kit::{Button, ButtonVariant, Kit, SegmentedControl, TextInput};
use serde_json::{Map, Value};

use crate::model::agent::{Body, Field, Snapshot};
use crate::model::bus::{Cmd, send};

fn label_style() -> TextStyle {
    crate::config::fonts::ui(12.0, 500.0)
}
fn name_style() -> TextStyle {
    crate::config::fonts::ui(13.0, 400.0)
}

/// What the user has put in the form on screen. Seeded anew when the form's id or fields change,
/// which is what a replaced form means; kept as the pane redraws.
#[derive(Default)]
pub struct State {
    seeded: Option<(String, Vec<Field>)>,
    /// A field to type into.
    edits: Vec<TextEdit>,
    /// A field to pick among options: the index chosen.
    picks: Vec<Option<usize>>,
}

pub type At<S> = fn(&mut S) -> &mut State;

impl State {
    pub fn follow(&mut self, snapshot: &Snapshot) {
        let Body::Form { id, fields, .. } = &snapshot.body else {
            return;
        };
        if self
            .seeded
            .as_ref()
            .is_some_and(|(old_id, old)| old_id == id && old == fields)
        {
            return;
        }
        self.seeded = Some((id.clone(), fields.clone()));
        self.edits = fields
            .iter()
            .map(|f| {
                TextEdit::new(if f.options.is_empty() {
                    f.value.clone().unwrap_or_default()
                } else {
                    String::new()
                })
            })
            .collect();
        self.picks = fields
            .iter()
            .map(|f| {
                f.value
                    .as_ref()
                    .and_then(|v| f.options.iter().position(|o| o == v))
            })
            .collect();
    }

    /// The answers by field name, empty ones left out.
    fn answers(&self, fields: &[Field]) -> Value {
        let mut values = Map::new();
        for (i, field) in fields.iter().enumerate() {
            let value = match field
                .options
                .get(self.picks.get(i).copied().flatten().unwrap_or(usize::MAX))
            {
                Some(option) => option.clone(),
                None => self
                    .edits
                    .get(i)
                    .map(|e| e.text.trim().to_string())
                    .unwrap_or_default(),
            };
            if !value.is_empty() {
                values.insert(field.name.clone(), Value::String(value));
            }
        }
        Value::Object(values)
    }
}

fn page(k: &Kit, content: El) -> El {
    col().w_full().grow(1.0).min_h(0.0).child(
        k.scroll_bar(
            col()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .items_center()
                .child(content),
        ),
    )
}

fn field<S: 'static>(k: &Kit, i: usize, field: &Field, state: &State, at: At<S>) -> El {
    let control = if field.options.is_empty() {
        TextInput::new(&state.edits[i])
            .label(field.label.clone())
            .width(None)
            .on_edit(move |s: &mut S| &mut at(s).edits[i])
            .build(k)
    } else {
        let names: Vec<&str> = field.options.iter().map(String::as_str).collect();
        SegmentedControl::new(&names, state.picks[i])
            .label(field.label.clone())
            .on_pick(move |s: &mut S, n: usize, _: &mut rune::Tx| at(s).picks[i] = Some(n))
            .build(k)
    };
    col()
        .w_full()
        .gap(4.0)
        .child(
            text(field.label.clone())
                .font(label_style())
                .color(k.c.text_muted),
        )
        .child(control)
}

/// The pane's body, below the header the shell draws. The terminal (`Bare`) is the shell's.
pub fn view<S: 'static>(snapshot: &Snapshot, state: &State, k: &Kit, at: At<S>) -> El {
    let title = super::header::ruled(
        k,
        row()
            .w_full()
            .h(super::header::BAND)
            .items_center()
            .gap(8.0)
            .px(12.0)
            .child(super::header::controls::<S>(k, &snapshot.lead))
            .child(
                row().grow(1.0).shrink().child(
                    text(snapshot.title.clone())
                        .font(name_style())
                        .color(k.c.text_base)
                        .truncate(),
                ),
            )
            .child(super::header::controls::<S>(k, &snapshot.trail)),
    );
    let body = match &snapshot.body {
        // Only the header: what is below it is the shell's.
        Body::Bare => return title,
        Body::Markdown(source) => page(
            k,
            col().w_full().max_w(820.0).padding(16.0).child(
                Markdown::new(source.0.to_string())
                    .style(super::book_style(k))
                    .selectable(true)
                    .build(k),
            ),
        ),
        Body::Form {
            id,
            fields,
            submit,
            answered,
            doc,
        } => {
            let mut form = col().w_full().max_w(640.0).gap(12.0).padding(16.0);
            if let Some(doc) = doc {
                form = form.child(
                    Markdown::new(doc.0.to_string())
                        .style(super::book_style(k))
                        .build(k),
                );
            }
            // Until `follow` has seeded the state for this form there is nothing to bind to.
            let seeded = state
                .seeded
                .as_ref()
                .is_some_and(|(seeded, _)| seeded == id)
                && state.edits.len() == fields.len();
            if seeded {
                for (i, f) in fields.iter().enumerate() {
                    form = form.child(field::<S>(k, i, f, state, at));
                }
            }
            let (id, fields) = (id.clone(), fields.clone());
            let send_it = Button::new(if *answered {
                "Answered".to_string()
            } else {
                submit.clone()
            })
            .variant(ButtonVariant::Submit)
            .disabled(*answered)
            .on_press(move |s: &mut S| {
                let answers = at(s).answers(&fields);
                send(Cmd::SubmitForm(id.clone(), answers));
            })
            .build(k);
            page(k, form.child(row().w_full().justify_end().child(send_it)))
        }
    };
    col().w_full().h_full().min_h(0.0).child(title).child(body)
}
