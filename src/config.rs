use std::path::{Path, PathBuf};

use serde_json::Value;

/// Agents shipped in the binary; the user's config adds to or overrides them by `id`.
const DEFAULT_AGENTS: &str = include_str!("../agents.json");

#[derive(Clone)]
pub struct Agent {
    pub id: String,
    pub name: String,
    /// Either an ACP agent's command, or, with `daemon`, the command that starts it.
    pub command: String,
    /// URL of a dex daemon to talk to instead of ACP.
    pub daemon: Option<String>,
}

/// `$XDG_CONFIG_HOME/po`, or `~/.config/po`.
pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("po"))
}

pub fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(error) => {
            log::error!("{}: {error}", path.display());
            None
        }
    }
}

/// The user's `config.json`, or null when there is none.
pub fn load() -> Value {
    dir().and_then(|dir| read_json(&dir.join("config.json"))).unwrap_or(Value::Null)
}

fn parse(list: &Value) -> Vec<Agent> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = entry["id"].as_str()?.to_string();
            let command = entry["command"].as_str()?.to_string();
            let name = entry["name"].as_str().unwrap_or(&id).to_string();
            let daemon = entry["daemon"].as_str().map(String::from);
            Some(Agent { id, name, command, daemon })
        })
        .collect()
}

/// The bundled agents with the config's `agents` list merged in.
pub fn agents(config: &Value) -> Vec<Agent> {
    let mut agents = serde_json::from_str(DEFAULT_AGENTS).map(|list: Value| parse(&list)).unwrap_or_default();
    for agent in parse(&config["agents"]) {
        match agents.iter_mut().find(|known| known.id == agent.id) {
            Some(known) => *known = agent,
            None => agents.push(agent),
        }
    }
    agents
}
