//! The settings page (Ctrl/Cmd+,): the options of `config.json` as fields, written back to it on
//! Save. Theme, fonts and glass apply as soon as the file is written, since po watches it.
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use serde_json::{Map, Value};

use crate::config;
use crate::fonts;
use crate::schemes;
use crate::theme::Palette;
use crate::ui::button::Button;
use crate::ui::input::{Input, InputEvent, InputState};
use crate::ui::list_item::ListItem;

pub enum SettingsEvent {
    Close,
    Saved,
}

/// One entry of the config's `agents`. Keys the page has no field for, like `daemon`, are kept.
struct AgentRow {
    id: Entity<InputState>,
    name: Entity<InputState>,
    command: Entity<InputState>,
    extra: Map<String, Value>,
}

pub struct SettingsView {
    theme: Entity<InputState>,
    font_family: Entity<InputState>,
    font_size: Entity<InputState>,
    mono_family: Entity<InputState>,
    mono_size: Entity<InputState>,
    idle_minutes: Entity<InputState>,
    max_live: Entity<InputState>,
    notifications: bool,
    glass: bool,
    agents: Vec<AgentRow>,
    error: Option<String>,
    _events: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

/// A name or list of names as one comma separated line, the way a field shows it.
fn names_line(value: &Value) -> String {
    match value {
        Value::String(name) => name.clone(),
        Value::Array(list) => list.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

/// The inverse of `names_line`: one name stays a string, several become a list.
fn names_value(line: &str) -> Option<Value> {
    let mut names: Vec<&str> = line.split(',').map(str::trim).filter(|name| !name.is_empty()).collect();
    match names.len() {
        0 => None,
        1 => Some(Value::String(names.remove(0).to_string())),
        _ => Some(Value::Array(names.into_iter().map(|name| Value::String(name.to_string())).collect())),
    }
}

/// A field's number: nothing for an empty field (the default applies), an error for text, for a
/// fraction where `whole` is asked, or for a number the option would ignore.
fn number(label: &str, line: &str, range: std::ops::RangeInclusive<f64>, whole: bool) -> Result<Option<f64>, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let n = line.parse::<f64>().ok().filter(|n| n.is_finite()).ok_or_else(|| format!("{label} needs a number"))?;
    if whole && n.fract() != 0. {
        return Err(format!("{label} needs a whole number"));
    }
    if !range.contains(&n) {
        return Err(format!("{label} must be from {} to {}", range.start(), range.end()));
    }
    Ok(Some(n))
}

fn json_number(n: f64) -> Value {
    if n.fract() == 0. { Value::from(n as u64) } else { Value::from(n) }
}

/// Sets `key`, or removes it for `None` so the option goes back to its default.
fn put(map: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    match value {
        Some(value) => {
            map.insert(key.to_string(), value);
        }
        None => {
            map.remove(key);
        }
    }
}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config = config::load();
        let field = |value: String, placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_string()));
            input.update(cx, |input, cx| input.set_value(value, window, cx));
            input
        };
        let text = |value: &Value| match value {
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        };
        let theme = field(config["theme"].as_str().unwrap_or("").to_string(), "Rose Pine", window, cx);
        let font = &config["font"];
        let font_family = field(names_line(&font["family"]), "Adwaita Sans", window, cx);
        let font_size = field(text(&font["size"]), "14", window, cx);
        let mono_family = field(names_line(&font["mono_family"]), "system monospace", window, cx);
        let mono_size = field(text(&font["mono_size"]), "13", window, cx);
        let idle_minutes = field(text(&config["idle_minutes"]), "15", window, cx);
        let max_live = field(text(&config["max_live_agents"]), "8", window, cx);
        let agents = config["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .map(|entry| {
                let get = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                let extra = entry.iter().filter(|(key, _)| !["id", "name", "command"].contains(&key.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
                AgentRow {
                    id: field(get("id"), "id", window, cx),
                    name: field(get("name"), "Name", window, cx),
                    command: field(get("command"), "command --acp", window, cx),
                    extra,
                }
            })
            .collect();
        // The theme's matches follow what is typed.
        let events = vec![cx.subscribe(&theme, |_, _, _: &InputEvent, cx| cx.notify())];
        let glass = match &config["glass"] {
            Value::Bool(on) => *on,
            Value::Number(_) => true,
            _ => false,
        };
        Self {
            theme,
            font_family,
            font_size,
            mono_family,
            mono_size,
            idle_minutes,
            max_live,
            notifications: config["notifications"].as_bool().unwrap_or(true),
            glass,
            agents,
            error: None,
            _events: events,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.theme.update(cx, |input, cx| input.focus(window, cx));
    }

    fn add_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut field = |placeholder: &str, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_string()));
        let row = AgentRow { id: field("id", cx), name: field("Name", cx), command: field("command --acp", cx), extra: Map::new() };
        row.id.update(cx, |input, cx| input.focus(window, cx));
        self.agents.push(row);
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        match self.collect(cx).and_then(|change| config::update(change)) {
            Ok(()) => {
                self.error = None;
                cx.emit(SettingsEvent::Saved);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    /// The edit to make to `config.json`, or what is wrong with the fields.
    fn collect(&self, cx: &Context<Self>) -> Result<impl FnOnce(&mut Map<String, Value>) + use<>, String> {
        let read = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
        let sizes = f64::from(*fonts::SIZES.start())..=f64::from(*fonts::SIZES.end());
        let font_size = number("Font size", &read(&self.font_size), sizes.clone(), false)?;
        let mono_size = number("Mono font size", &read(&self.mono_size), sizes, false)?;
        let idle = number("Idle minutes", &read(&self.idle_minutes), 0.0..=1_000_000., true)?;
        let max_live = number("Max live agents", &read(&self.max_live), 1.0..=1000., true)?;
        let (theme, font_family, mono_family) = (read(&self.theme), names_value(&read(&self.font_family)), names_value(&read(&self.mono_family)));
        let (notifications, glass) = (self.notifications, self.glass);
        let mut agents = Vec::new();
        for row in &self.agents {
            let (id, name, command) = (read(&row.id), read(&row.name), read(&row.command));
            if id.is_empty() && name.is_empty() && command.is_empty() {
                continue;
            }
            if id.is_empty() || command.is_empty() {
                return Err("An agent needs an id and a command".into());
            }
            let mut entry = row.extra.clone();
            entry.insert("id".into(), id.into());
            entry.insert("command".into(), command.into());
            put(&mut entry, "name", (!name.is_empty()).then(|| name.into()));
            agents.push(Value::Object(entry));
        }
        Ok(move |config: &mut Map<String, Value>| {
            put(config, "theme", (!theme.is_empty()).then(|| theme.into()));
            let mut font = config.get("font").and_then(Value::as_object).cloned().unwrap_or_default();
            put(&mut font, "family", font_family);
            put(&mut font, "size", font_size.map(json_number));
            put(&mut font, "mono_family", mono_family);
            put(&mut font, "mono_size", mono_size.map(json_number));
            put(config, "font", (!font.is_empty()).then(|| Value::Object(font)));
            put(config, "idle_minutes", idle.map(json_number));
            put(config, "max_live_agents", max_live.map(json_number));
            // Notifications default on, so only turning them off is written.
            put(config, "notifications", (!notifications).then_some(Value::Bool(false)));
            // A glass opacity set by hand stays while glass is on.
            let kept = config.get("glass").filter(|value| value.is_number()).cloned();
            put(config, "glass", if glass { Some(kept.unwrap_or(Value::Bool(true))) } else { config.contains_key("glass").then_some(Value::Bool(false)) });
            put(config, "agents", (!agents.is_empty()).then(|| Value::Array(agents)));
        })
    }

    fn pick_theme(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.theme.update(cx, |input, cx| input.set_value(name.to_string(), window, cx));
        cx.notify();
    }
}

fn section(title: &str, muted: gpui::Hsla) -> impl IntoElement {
    div().pt_3().pb_1().text_xs().text_color(muted).child(title.to_uppercase())
}

/// A label on the left and its control on the right.
fn row(label: &str, hint: &str, muted: gpui::Hsla, control: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_3()
        .py_1()
        .child(div().w(px(200.)).flex_none().child(div().text_sm().child(label.to_string())).child(div().text_xs().text_color(muted).child(hint.to_string())))
        .child(div().flex_1().min_w_0().flex().justify_end().child(control))
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.global::<Palette>();
        let (popover, border, muted, radius, danger) = (palette.popover, palette.border, palette.muted_foreground, palette.radius, palette.danger);
        let this = cx.entity();
        let typed = self.theme.read(cx).value().trim().to_string();
        let matches: Vec<&'static str> = if typed.is_empty() || schemes::names().any(|name| name.eq_ignore_ascii_case(&typed)) {
            vec![]
        } else {
            let needle = typed.to_lowercase();
            schemes::names().filter(|name| name.to_lowercase().contains(&needle)).take(6).collect()
        };
        let field = |input: &Entity<InputState>| div().w_full().child(Input::new(input).field());
        let toggle = |id: &'static str, on: bool, cx: &mut Context<Self>| {
            Button::new(id).outline().label(if on { "On" } else { "Off" }).selected(on).on_click(cx.listener(move |view, _, _, cx| {
                match id {
                    "notifications" => view.notifications = !view.notifications,
                    _ => view.glass = !view.glass,
                }
                cx.notify();
            }))
        };
        let agents: Vec<_> = self
            .agents
            .iter()
            .enumerate()
            .map(|(ix, agent)| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py_1()
                    .child(div().w(px(90.)).flex_none().child(Input::new(&agent.id).field()))
                    .child(div().w(px(120.)).flex_none().child(Input::new(&agent.name).field()))
                    .child(div().flex_1().min_w_0().child(Input::new(&agent.command).field()))
                    .child(Button::new(("remove-agent", ix)).ghost().label("✕").tooltip("Remove").on_click(cx.listener(move |view, _, _, cx| {
                        view.agents.remove(ix);
                        cx.notify();
                    })))
            })
            .collect();
        div()
            .absolute()
            .inset_0()
            .id("settings-backdrop")
            .occlude()
            .on_click(cx.listener(|_, _: &gpui::ClickEvent, _, cx| cx.emit(SettingsEvent::Close)))
            .flex()
            .justify_center()
            .items_start()
            .py_6()
            .child(
                div()
                    .id("settings")
                    .occlude()
                    .key_context("Settings")
                    .w(px(680.))
                    .max_h_full()
                    .flex()
                    .flex_col()
                    .rounded(radius)
                    .border_1()
                    .border_color(border)
                    .bg(popover)
                    .overflow_hidden()
                    .child(div().px_4().py_2().border_b_1().border_color(border).font_weight(gpui::FontWeight::SEMIBOLD).child("Settings"))
                    .child(
                        div()
                            .id("settings-body")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px_4()
                            .pb_3()
                            .child(section("Appearance", muted))
                            .child(row("Theme", "A colour scheme name or theme file", muted, field(&self.theme)))
                            .children((!matches.is_empty()).then(|| {
                                div().pb_1().flex().flex_col().children(matches.into_iter().enumerate().map(|(ix, name)| {
                                    let this = this.clone();
                                    ListItem::new(("scheme", ix))
                                        .on_click(move |_, window, cx| this.update(cx, |view, cx| view.pick_theme(name, window, cx)))
                                        .child(name)
                                }))
                            }))
                            .child(row("Glass", "Translucent panes over the desktop", muted, toggle("glass", self.glass, cx)))
                            .child(row("UI font", "Names tried in order, comma separated", muted, field(&self.font_family)))
                            .child(row("UI font size", "Also scales spacing", muted, field(&self.font_size)))
                            .child(row("Monospace font", "Names tried in order, comma separated", muted, field(&self.mono_family)))
                            .child(row("Monospace font size", "Code and diffs", muted, field(&self.mono_size)))
                            .child(section("Behavior", muted))
                            .child(row("Notifications", "When a thread needs you in the background", muted, toggle("notifications", self.notifications, cx)))
                            .child(row("Idle minutes", "Stop an idle agent after this long; 0 never", muted, field(&self.idle_minutes)))
                            .child(row("Max live agents", "Longest idle agents stop beyond this many", muted, field(&self.max_live)))
                            .child(section("Agents", muted))
                            .child(div().text_xs().text_color(muted).pb_1().child("Added to the bundled agents; the same id replaces one. A new command asks before it first runs."))
                            .children(agents)
                            .child(div().pt_1().child(Button::new("add-agent").outline().label("Add agent").on_click(cx.listener(|view, _, window, cx| view.add_agent(window, cx))))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_4()
                            .py_2()
                            .border_t_1()
                            .border_color(border)
                            .child(div().flex_1().text_xs().text_color(if self.error.is_some() { danger } else { muted }).child(
                                self.error.clone().unwrap_or_else(|| "Saved to config.json; other keys in it are kept. MCP servers are edited there.".into()),
                            ))
                            .child(Button::new("settings-cancel").outline().label("Close").on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Close))))
                            .child(Button::new("settings-save").primary().label("Save").on_click(cx.listener(|view, _, _, cx| view.save(cx)))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn font_names_round_trip_as_a_string_or_a_list() {
        assert_eq!(names_value("Fira Code"), Some(json!("Fira Code")));
        assert_eq!(names_value(" A , B ,"), Some(json!(["A", "B"])));
        assert_eq!(names_value("  "), None);
        assert_eq!(names_line(&json!(["A", "B"])), "A, B");
        assert_eq!(names_line(&json!("A")), "A");
    }

    #[test]
    fn numbers_may_be_empty_but_must_be_usable() {
        let any = 0.0..=100.;
        assert_eq!(number("n", "", any.clone(), false), Ok(None));
        assert_eq!(number("n", " 14 ", any.clone(), true), Ok(Some(14.)));
        assert!(number("n", "big", any.clone(), false).is_err());
        assert!(number("n", "-1", any.clone(), false).is_err());
        assert!(number("n", "1e20", any.clone(), true).is_err());
        assert!(number("n", "1.5", any.clone(), true).is_err());
        assert_eq!(number("n", "1.5", any, false), Ok(Some(1.5)));
        assert_eq!(json_number(14.), json!(14));
        assert_eq!(json_number(13.5), json!(13.5));
    }
}
