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

/// Changes the user's `config.json` and writes it back, keeping every key `change` leaves alone.
/// The file is replaced whole, so a crash can't leave half of one; one that exists but doesn't
/// parse is left as it is, since overwriting it would throw away what a person wrote.
pub fn update(change: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Result<(), String> {
    let dir = dir().ok_or("no config folder")?;
    // Written through a symlink, so a config kept in a dotfiles repo stays a link to it.
    let path = dir.join("config.json");
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let mut config = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => map,
            _ => return Err(format!("{} isn't a JSON object; fix it first", path.display())),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    change(&mut config);
    std::fs::create_dir_all(path.parent().unwrap_or(&dir)).map_err(|error| format!("{}: {error}", dir.display()))?;
    let temp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&Value::Object(config)).map_err(|error| error.to_string())?;
    std::fs::write(&temp, text + "\n").and_then(|()| std::fs::rename(&temp, &path)).map_err(|error| format!("{}: {error}", path.display()))
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

/// A stdio MCP server from the config's `mcp_servers`, which an agent is asked to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl McpServer {
    /// What the user is asked to approve: the command line, which is what will run.
    pub fn line(&self) -> String {
        std::iter::once(self.command.as_str()).chain(self.args.iter().map(String::as_str)).collect::<Vec<_>>().join(" ")
    }

    /// The server as ACP's `mcpServers` entry.
    fn acp(&self) -> Value {
        let env: Vec<Value> = self.env.iter().map(|(name, value)| serde_json::json!({ "name": name, "value": value })).collect();
        serde_json::json!({ "name": self.name, "command": self.command, "args": self.args, "env": env })
    }

    /// What an approval is for: everything that decides what runs, not the name.
    fn approval(&self) -> Value {
        serde_json::json!({ "mcp": { "command": self.command, "args": self.args, "env": self.env } })
    }
}

/// The servers of the config's `mcp_servers`. An entry that doesn't parse is logged and skipped,
/// so one typo doesn't take the rest away; so is a second entry with a name already used.
pub fn mcp_entries(config: &Value) -> Vec<McpServer> {
    let mut servers: Vec<McpServer> = Vec::new();
    for entry in config["mcp_servers"].as_array().into_iter().flatten() {
        let (Some(name), Some(command)) = (entry["name"].as_str(), entry["command"].as_str()) else {
            log::warn!("mcp_servers: an entry needs a name and a command: {entry}");
            continue;
        };
        if servers.iter().any(|known| known.name == name) {
            log::warn!("mcp_servers: `{name}` is listed twice; only the first is used");
            continue;
        }
        let mut args = Vec::new();
        match &entry["args"] {
            Value::Null => {}
            Value::Array(list) => {
                for item in list {
                    match item.as_str() {
                        Some(arg) => args.push(arg.to_string()),
                        None => log::warn!("mcp_servers: `{name}`: an argument is not a string and is skipped: {item}"),
                    }
                }
            }
            other => log::warn!("mcp_servers: `{name}`: `args` is not a list and is ignored: {other}"),
        }
        let mut env = Vec::new();
        match &entry["env"] {
            Value::Null => {}
            Value::Object(map) => {
                for (key, value) in map {
                    match value.as_str() {
                        Some(value) => env.push((key.clone(), value.to_string())),
                        None => log::warn!("mcp_servers: `{name}`: `env` value for {key} is not a string and is skipped: {value}"),
                    }
                }
            }
            other => log::warn!("mcp_servers: `{name}`: `env` is not an object and is ignored: {other}"),
        }
        servers.push(McpServer { name: name.to_string(), command: command.to_string(), args, env });
    }
    servers
}

/// Servers the user said no to for this run, so that Po doesn't ask again until it is restarted.
static DECLINED: std::sync::Mutex<Vec<Value>> = std::sync::Mutex::new(Vec::new());

fn declined(server: &McpServer) -> bool {
    DECLINED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).contains(&server.approval())
}

fn approved_mcp(path: &Path, server: &McpServer) -> bool {
    let wanted = server.approval();
    approved_this_run_value(&wanted) || read_json(path).and_then(|list| list.as_array().map(|list| list.contains(&wanted))).unwrap_or(false)
}

/// The config's servers that Po has not been told it may hand to an agent, and the user has not
/// declined this run. A server is code the agent runs as the user, like an agent's own command.
pub fn unapproved_mcp(config: &Value) -> Vec<McpServer> {
    let path = approvals_path();
    mcp_entries(config).into_iter().filter(|server| !approved_mcp(&path, server) && !declined(server)).collect()
}

/// The config's `mcp_servers` that are approved, as ACP's `mcpServers` list for a new or resumed
/// session. The others are left out and say so in the log.
pub fn mcp_servers(config: &Value) -> Value {
    let path = approvals_path();
    let servers = mcp_entries(config).into_iter().filter(|server| {
        let approved = approved_mcp(&path, server);
        if !approved {
            log::info!("mcp_servers: `{}` is not approved, so it is not given to the agent", server.name);
        }
        approved
    });
    Value::Array(servers.map(|server| server.acp()).collect())
}

/// Records that the user approved handing `servers` to agents, for this run and, if the file can be
/// written, the next ones.
pub fn approve_mcp(servers: &[McpServer]) {
    let path = approvals_path();
    for server in servers {
        approve_value_this_run(server.approval());
        if let Err(error) = approve_value_in(&path, server.approval()) {
            log::error!("{}: {error}; the approval holds until Po is closed", path.display());
        }
    }
}

/// Leaves `servers` out for the rest of this run.
pub fn decline_mcp(servers: &[McpServer]) {
    let mut declined = DECLINED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for server in servers {
        if !declined.contains(&server.approval()) {
            declined.push(server.approval());
        }
    }
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
    approved_this_run_value(&entry(agent))
}

fn approved_this_run_value(wanted: &Value) -> bool {
    THIS_RUN.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).contains(wanted)
}

fn approve_this_run(agent: &Agent) {
    approve_value_this_run(entry(agent));
}

fn approve_value_this_run(value: Value) {
    let mut approved = THIS_RUN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !approved.contains(&value) {
        approved.push(value);
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
    approve_value_in(path, entry(agent))
}

fn approve_value_in(path: &Path, wanted: Value) -> std::io::Result<()> {
    let invalid = |why: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, why.to_string());
    let mut list = match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Array(list)) => list,
            _ => return Err(invalid("not a list of approved commands; fix or delete it")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
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

    fn server(name: &str, command: &str) -> McpServer {
        McpServer { name: name.into(), command: command.into(), args: vec![], env: vec![] }
    }

    #[test]
    fn mcp_entries_become_acp_stdio_servers() {
        let config = serde_json::json!({ "mcp_servers": [
            { "name": "fs", "command": "npx", "args": ["-y", "server-fs", "."], "env": { "TOKEN": "x" } },
            { "name": "bare", "command": "tool" },
        ] });
        let servers = mcp_entries(&config);
        assert_eq!(servers.len(), 2);
        assert_eq!(
            servers[0].acp(),
            serde_json::json!({ "name": "fs", "command": "npx", "args": ["-y", "server-fs", "."], "env": [{ "name": "TOKEN", "value": "x" }] })
        );
        assert_eq!(servers[1].acp(), serde_json::json!({ "name": "bare", "command": "tool", "args": [], "env": [] }));
        assert_eq!(servers[0].line(), "npx -y server-fs .");
    }

    #[test]
    fn mcp_entries_skip_what_does_not_parse_and_repeated_names() {
        assert!(mcp_entries(&Value::Null).is_empty());
        let config = serde_json::json!({ "mcp_servers": [
            { "name": "no-command" },
            { "command": "no-name" },
            "text",
            { "name": "ok", "command": "c", "args": ["a", 1], "env": { "N": 1, "S": "s" } },
            { "name": "ok", "command": "other" },
            { "name": "odd", "command": "c", "args": "not a list", "env": ["not", "an", "object"] },
        ] });
        let servers = mcp_entries(&config);
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].args, vec!["a".to_string()]);
        assert_eq!(servers[0].env, vec![("S".to_string(), "s".to_string())]);
        assert_eq!(servers[0].command, "c");
        assert!(servers[1].args.is_empty() && servers[1].env.is_empty());
    }

    #[test]
    fn a_server_is_approved_for_what_it_runs_not_its_name() {
        let path = std::env::temp_dir().join(format!("po-mcp-approvals-{}/approved.json", std::process::id()));
        let fs = server("fs", "npx");
        assert!(!approved_mcp(&path, &fs));
        approve_value_in(&path, fs.approval()).unwrap();
        assert!(approved_mcp(&path, &fs));
        assert!(approved_mcp(&path, &server("renamed", "npx")));
        assert!(!approved_mcp(&path, &server("fs", "other")));
        assert!(!approved_mcp(&path, &McpServer { args: vec!["--more".into()], ..fs.clone() }));
        assert!(!approved_mcp(&path, &McpServer { env: vec![("K".into(), "v".into())], ..fs }));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_declined_server_is_not_asked_about_again_this_run() {
        let config = serde_json::json!({ "mcp_servers": [{ "name": "d", "command": "po-test-declined-server" }] });
        assert_eq!(unapproved_mcp(&config).len(), 1);
        decline_mcp(&mcp_entries(&config));
        assert!(unapproved_mcp(&config).is_empty());
        // Declined is not approved: it stays out of what the agent gets.
        assert_eq!(mcp_servers(&config), serde_json::json!([]));
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
