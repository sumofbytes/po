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
    /// The user named this command themselves, on the command line, so it needs no approval.
    pub vouched: bool,
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
            Some(Agent { id, name, command, daemon, vouched: false })
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

/// Whether Po may run `agent` without asking: it is one of the bundled agents as shipped, the user
/// named it on the command line, or they approved this exact command before. A command that came
/// from `config.json` or changed there is asked about, so editing the config (or something editing
/// it) can't make Po run a program the user never saw.
pub fn trusted(agent: &Agent) -> bool {
    agent.vouched || shipped(agent) || approved_this_run(agent) || approved_in(&approvals_path(), agent)
}

/// Commands approved since Po started, kept apart from the file so that a yes holds even if the
/// file can't be written.
static THIS_RUN: std::sync::Mutex<Vec<Value>> = std::sync::Mutex::new(Vec::new());

fn approved_this_run(agent: &Agent) -> bool {
    THIS_RUN.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).contains(&entry(agent))
}

fn approve_this_run(agent: &Agent) {
    let mut approved = THIS_RUN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !approved.contains(&entry(agent)) {
        approved.push(entry(agent));
    }
}

fn shipped(agent: &Agent) -> bool {
    let bundled = serde_json::from_str(DEFAULT_AGENTS).map(|list: Value| parse(&list)).unwrap_or_default();
    bundled.iter().any(|known| known.command == agent.command && known.daemon == agent.daemon)
}

/// Records that the user approved running `agent`'s command: for this run always, and for the
/// next ones if the approvals file can be written.
pub fn approve(agent: &Agent) {
    approve_this_run(agent);
    let path = approvals_path();
    if let Err(error) = approve_in(&path, agent) {
        log::error!("{}: {error}; the approval holds until Po is closed", path.display());
    }
}

/// In Po's own data directory rather than the config's, which is what an agent is asked to edit.
fn approvals_path() -> PathBuf {
    data_dir().join("approved-agents.json")
}

fn entry(agent: &Agent) -> Value {
    serde_json::json!({ "command": agent.command, "daemon": agent.daemon })
}

fn approved_in(path: &Path, agent: &Agent) -> bool {
    let wanted = entry(agent);
    read_json(path).and_then(|list| list.as_array().map(|list| list.contains(&wanted))).unwrap_or(false)
}

/// Adds `agent` to the approvals file, writing a new file and renaming it over the old so that a
/// crash can't leave half of one. A file that isn't a list is left alone rather than replaced,
/// since it may hold approvals that a person can still recover.
fn approve_in(path: &Path, agent: &Agent) -> std::io::Result<()> {
    let invalid = |why: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, why.to_string());
    let mut list = match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Array(list)) => list,
            _ => return Err(invalid("not a list of approved commands; fix or delete it")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let wanted = entry(agent);
    if list.contains(&wanted) {
        return Ok(());
    }
    list.push(wanted);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_string_pretty(&list).expect("a list of objects"))?;
    std::fs::rename(&temp, path)
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
    use super::*;

    fn agent(command: &str) -> Agent {
        Agent { id: "x".into(), name: "X".into(), command: command.into(), daemon: None, vouched: false }
    }

    #[test]
    fn the_agents_that_ship_need_no_approval_but_others_do() {
        assert!(shipped(&agent("claude-agent-acp")));
        // The same program with other arguments is a change.
        assert!(!shipped(&agent("claude-agent-acp --other")));
        assert!(!shipped(&agent("./my-agent")));
        assert!(trusted(&Agent { vouched: true, ..agent("./mine") }));
    }

    #[test]
    fn an_approval_holds_for_the_run_even_if_nothing_can_be_written() {
        let agent = agent("./approved-only-in-memory");
        assert!(!approved_this_run(&agent));
        approve_this_run(&agent);
        approve_this_run(&agent);
        assert!(approved_this_run(&agent));
        assert!(trusted(&agent));
        assert!(!approved_this_run(&Agent { command: "./another".into(), ..agent }));
    }

    #[test]
    fn a_file_that_is_not_a_list_is_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("po-approvals-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("approved.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(approve_in(&path, &agent("./a")).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_approval_is_for_that_exact_command() {
        let path = std::env::temp_dir().join(format!("po-approvals-{}/approved.json", std::process::id()));
        assert!(!approved_in(&path, &agent("./a")));
        approve_in(&path, &agent("./a")).unwrap();
        approve_in(&path, &agent("./a")).unwrap();
        assert!(approved_in(&path, &agent("./a")));
        assert!(!approved_in(&path, &agent("./a --more")));
        assert!(!approved_in(&path, &Agent { daemon: Some("http://x".into()), ..agent("./a") }));
        assert_eq!(read_json(&path).unwrap().as_array().unwrap().len(), 1);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
