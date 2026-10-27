//! The settings page (Ctrl/Cmd+,): the options of `config.json` as fields, written back to it on
//! Save. Theme, fonts and glass apply as soon as the file is written, since po watches it. The
//! fields are NSH's ([`crate::nsh::settings`]); this is what they hold and how it is written.
use serde_json::{Map, Value};

use crate::config;
use crate::fonts;

/// One entry of the config's `agents`. Keys the page has no field for, like `daemon`, are kept.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentForm {
    pub id: String,
    pub name: String,
    pub command: String,
    pub extra: Map<String, Value>,
}

/// What the page's fields hold, as text, so a half-typed number is still what was typed.
#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    pub theme: String,
    pub font_family: String,
    pub font_size: String,
    pub mono_family: String,
    pub mono_size: String,
    pub idle_minutes: String,
    pub max_live: String,
    pub notifications: bool,
    pub glass: bool,
    pub viewer_left: bool,
    pub agents: Vec<AgentForm>,
}

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

impl Form {
    /// The fields as `config.json` has them now. `viewer_left` is where the viewer is now, which is
    /// what the page starts from.
    pub fn load(viewer_left: bool) -> Self {
        let config = config::load();
        let text = |value: &Value| match value {
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        };
        let font = &config["font"];
        let agents = config["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .map(|entry| {
                let get = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                let extra = entry.iter().filter(|(key, _)| !["id", "name", "command"].contains(&key.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
                AgentForm { id: get("id"), name: get("name"), command: get("command"), extra }
            })
            .collect();
        let glass = match &config["glass"] {
            Value::Bool(on) => *on,
            Value::Number(_) => true,
            _ => false,
        };
        Self {
            theme: config["theme"].as_str().unwrap_or("").to_string(),
            font_family: names_line(&font["family"]),
            font_size: text(&font["size"]),
            mono_family: names_line(&font["mono_family"]),
            mono_size: text(&font["mono_size"]),
            idle_minutes: text(&config["idle_minutes"]),
            max_live: text(&config["max_live_agents"]),
            notifications: config["notifications"].as_bool().unwrap_or(true),
            glass,
            viewer_left,
            agents,
        }
    }

    /// Writes the fields to `config.json`, or says what is wrong with them.
    pub fn save(&self) -> Result<(), String> {
        self.collect().and_then(config::update)
    }

    /// The edit to make to `config.json`, or what is wrong with the fields.
    fn collect(&self) -> Result<impl FnOnce(&mut Map<String, Value>) + use<>, String> {
        let read = |text: &str| text.trim().to_string();
        let sizes = f64::from(*fonts::SIZES.start())..=f64::from(*fonts::SIZES.end());
        let font_size = number("Font size", &read(&self.font_size), sizes.clone(), false)?;
        let mono_size = number("Mono font size", &read(&self.mono_size), sizes, false)?;
        let idle = number("Idle minutes", &read(&self.idle_minutes), 0.0..=1_000_000., true)?;
        let max_live = number("Max live agents", &read(&self.max_live), 1.0..=1000., true)?;
        let (theme, font_family, mono_family) = (read(&self.theme), names_value(&read(&self.font_family)), names_value(&read(&self.mono_family)));
        let (notifications, glass, viewer_left) = (self.notifications, self.glass, self.viewer_left);
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
            put(config, "viewer_position", Some((if viewer_left { "left" } else { "right" }).into()));
            put(config, "agents", (!agents.is_empty()).then(|| Value::Array(agents)));
        })
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
