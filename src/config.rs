use std::collections::BTreeMap;
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

/// The user's home directory: `$HOME`, or `%USERPROFILE%` on Windows.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// `$XDG_CONFIG_HOME/po`, or `~/.config/po` (`%APPDATA%\po` on Windows).
pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| {
        if cfg!(windows) {
            std::env::var_os("APPDATA").map(PathBuf::from)
        } else {
            home().map(|home| home.join(".config"))
        }
    })?;
    Some(base.join("po"))
}

/// `$XDG_DATA_HOME/po`, or `~/.local/share/po` (`%LOCALAPPDATA%\po` on Windows): where po keeps what it makes, like worktrees.
pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
            } else {
                home().map(|home| home.join(".local/share"))
            }
        })
        .unwrap_or_else(std::env::temp_dir)
        .join("po")
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

/// The `mcp_servers` of the config as ACP's `mcpServers` list, handed to every new or resumed
/// session. Only stdio servers: `command` is required, `args` and `env` (a name-to-value object)
/// are optional. An entry that doesn't parse is logged and skipped, so one typo doesn't take the
/// rest away.
pub fn mcp_servers(config: &Value) -> Value {
    let servers = config["mcp_servers"].as_array().into_iter().flatten().filter_map(|entry| {
        let (Some(name), Some(command)) = (entry["name"].as_str(), entry["command"].as_str()) else {
            log::warn!("mcp_servers: an entry needs a name and a command: {entry}");
            return None;
        };
        let args: Vec<&str> = entry["args"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let env: Vec<Value> = entry["env"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| Some(serde_json::json!({ "name": key, "value": value.as_str()? })))
            .collect();
        Some(serde_json::json!({ "name": name, "command": command, "args": args, "env": env }))
    });
    Value::Array(servers.collect())
}

/// The settings chosen for each agent (by agent id, then option id), reapplied to its new threads.
pub type Choices = BTreeMap<String, BTreeMap<String, String>>;

fn choices_path() -> PathBuf {
    data_dir().join("choices.json")
}

pub fn load_choices() -> Choices {
    read_json(&choices_path()).and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default()
}

/// Remembers one choice. The file is read again first, so other windows' choices are kept, and
/// replaced whole, so a crash can't leave it half written.
pub fn save_choice(agent: &str, id: &str, value: &str) {
    // Saves run on background tasks and in other windows' processes; one at a time keeps a
    // pick from overwriting another.
    static SAVING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _saving = SAVING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = choices_path();
    let _ = path.parent().map(std::fs::create_dir_all);
    // Held until the end of the save; released when the file closes.
    let lock = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(path.with_extension("lock"));
    if let Ok(lock) = &lock
        && let Err(error) = lock.lock()
    {
        log::warn!("{}: {error}", path.display());
    }
    let mut choices = load_choices();
    choices.entry(agent.to_string()).or_default().insert(id.to_string(), value.to_string());
    let temp = path.with_extension("json.tmp");
    let saved = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&temp, serde_json::to_string_pretty(&choices).unwrap_or_default()))
        .and_then(|()| std::fs::rename(&temp, &path));
    if let Err(error) = saved {
        log::error!("{}: {error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn mcp_servers_become_acp_stdio_entries() {
        let config = json!({ "mcp_servers": [
            { "name": "fs", "command": "npx", "args": ["-y", "server-fs", "."], "env": { "TOKEN": "x" } },
            { "name": "bare", "command": "tool" },
        ] });
        assert_eq!(
            mcp_servers(&config),
            json!([
                { "name": "fs", "command": "npx", "args": ["-y", "server-fs", "."], "env": [{ "name": "TOKEN", "value": "x" }] },
                { "name": "bare", "command": "tool", "args": [], "env": [] },
            ])
        );
    }

    #[test]
    fn mcp_servers_skip_bad_entries_and_default_to_none() {
        assert_eq!(mcp_servers(&Value::Null), json!([]));
        let config = json!({ "mcp_servers": [{ "name": "no-command" }, { "command": "no-name" }, "text", { "name": "ok", "command": "c", "env": { "N": 1 } }] });
        assert_eq!(mcp_servers(&config), json!([{ "name": "ok", "command": "c", "args": [], "env": [] }]));
    }
}
