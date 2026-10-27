//! A form: labeled fields the user fills in, and a submit button. The same inputs and buttons
//! the rest of the app is drawn with, so what an agent puts on screen looks like po itself.
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Window, div, rems};
use serde_json::{Map, Value};

use crate::theme::Palette;

use super::button::Button;
use super::input::{Input, InputState};

/// One field of a form: a line of text, or a choice among options. What the agent named it is
/// what its answer comes back keyed by.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub label: String,
    /// The options to pick among; empty is a field to type into.
    pub options: Vec<String>,
    /// The text it starts with, or the choice picked to begin with.
    pub value: Option<String>,
}

pub enum FormEvent {
    /// The form changed, but nothing was answered yet.
    Changed,
    /// The user submitted: the answers by field name, empty ones left out.
    Submit(Value),
}

/// The state of one form: an input for each text field, the choice picked for each choice
/// field, parallel to the fields, and whether it has been answered.
pub struct FormState {
    fields: Vec<Field>,
    /// The label on the submit button.
    submit: String,
    inputs: Vec<Option<Entity<InputState>>>,
    picked: Vec<Option<String>>,
    done: bool,
}

impl FormState {
    pub fn new(fields: Vec<Field>, submit: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let inputs = fields
            .iter()
            .map(|field| match field.options.is_empty() {
                true => {
                    let (value, label) = (field.value.clone(), field.label.clone());
                    let input = cx.new(|cx| InputState::new(window, cx).placeholder(label));
                    if let Some(value) = value {
                        input.update(cx, |input, cx| input.set_value(value, window, cx));
                    }
                    Some(input)
                }
                false => None,
            })
            .collect();
        let picked = fields.iter().map(|field| field.value.clone()).collect();
        Self { fields, submit, inputs, picked, done: false }
    }

    /// The answers by field name, empty ones left out.
    fn values(&self, cx: &App) -> Value {
        let mut values = Map::new();
        for (at, field) in self.fields.iter().enumerate() {
            let value = match field.options.is_empty() {
                true => self.inputs[at].as_ref().map(|input| input.read(cx).value().trim().to_string()).unwrap_or_default(),
                false => self.picked[at].clone().unwrap_or_default(),
            };
            if !value.is_empty() {
                values.insert(field.name.clone(), Value::String(value));
            }
        }
        Value::Object(values)
    }

    fn pick(&mut self, at: usize, option: &str, cx: &mut Context<Self>) {
        self.picked[at] = Some(option.to_string());
        cx.emit(FormEvent::Changed);
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.done {
            return;
        }
        self.done = true;
        cx.emit(FormEvent::Submit(self.values(cx)));
        cx.notify();
    }
}

impl EventEmitter<FormEvent> for FormState {}

impl Render for FormState {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.global::<Palette>();
        let (muted, border) = (colors.muted_foreground, colors.border);
        let fields = self
            .fields
            .iter()
            .enumerate()
            .map(|(at, field)| {
                let label = div().flex_none().w(rems(9.)).truncate().text_sm().text_color(muted).child(field.label.clone());
                let widget = if field.options.is_empty() {
                    let input = self.inputs[at].clone().expect("a text field has an input");
                    Input::new(&input).into_any_element()
                } else {
                    let picked = self.picked[at].clone();
                    let name = field.name.clone();
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .children(field.options.iter().map(|option| {
                            let option = option.clone();
                            Button::new(SharedString::from(format!("form-{name}-{option}")))
                                .label(option.clone())
                                .ghost()
                                .small()
                                .selected(picked.as_deref() == Some(option.as_str()))
                                .on_click(cx.listener(move |form, _, _, cx| form.pick(at, &option, cx)))
                        }))
                        .into_any_element()
                };
                div().flex().items_center().gap_2().child(label).child(widget)
            })
            .collect::<Vec<_>>();
        let submit = Button::new("form-submit")
            .label(if self.done { "Answered".into() } else { self.submit.clone() })
            .primary()
            .small()
            .disabled(self.done)
            .on_click(cx.listener(|form, _, _, cx| form.submit(cx)));
        div().flex().flex_col().gap_3().children(fields).child(div().flex().justify_end().pt_2().border_t_1().border_color(border).child(submit))
    }
}
