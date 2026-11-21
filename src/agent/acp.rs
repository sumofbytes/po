//! A minimal Agent Client Protocol client: JSON-RPC over the agent's stdio, one session.
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result};
use futures::channel::mpsc::unbounded;
use serde_json::{Value, json};

use crate::agent::terminals;
use crate::dev::profile::{UnboundedReceiver, UnboundedSender};
use crate::system::terminal::Launch;

#[derive(Clone, Debug)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    pub kind: String,
}

/// A session an agent can resume.
#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    /// ISO 8601, or empty when the agent doesn't say.
    pub updated: String,
}

/// A choice of a session setting, like its model: one of the agent's `select` config options.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    /// `model`, `mode`, `thought_level`, or empty; a hint for ordering and labels only.
    pub category: String,
    pub current: String,
    /// Each choice's value and name.
    pub choices: Vec<(String, String)>,
}

/// The `select` options in an agent's `configOptions`, in the agent's order. Choices may be
/// flat or grouped; groups are flattened.
fn parse_options(list: &Value) -> Vec<ConfigOption> {
    fn choices(list: &Value, into: &mut Vec<(String, String)>) {
        for entry in list.as_array().into_iter().flatten() {
            if let Some(value) = entry["value"].as_str() {
                into.push((
                    value.to_string(),
                    entry["name"].as_str().unwrap_or(value).to_string(),
                ));
            } else {
                choices(&entry["options"], into);
            }
        }
    }
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["type"].as_str().is_none_or(|kind| kind == "select"))
        .filter_map(|entry| {
            let id = entry["id"]
                .as_str()
                .or(entry["configId"].as_str())?
                .to_string();
            let current = entry["currentValue"].as_str()?.to_string();
            let mut all = Vec::new();
            choices(&entry["options"], &mut all);
            (!all.is_empty()).then(|| ConfigOption {
                name: entry["name"].as_str().unwrap_or(&id).to_string(),
                category: entry["category"].as_str().unwrap_or_default().to_string(),
                id,
                current,
                choices: all,
            })
        })
        .collect()
}

pub enum Event {
    /// The session's settings, when it opens and whenever they change.
    Options(Vec<ConfigOption>),
    /// The agent accepted a change to a session setting.
    OptionSet {
        id: String,
        value: String,
    },
    /// The agent refused a change to session setting `id`.
    Refused {
        id: String,
        text: String,
    },
    /// A session is open: a new one, or a loaded one once its history has been replayed. `fresh`
    /// is false for a resumed one, whose settings the agent restored.
    Ready {
        agent: String,
        fresh: bool,
    },
    /// The agent's resumable sessions for this project, newest first.
    Sessions(Vec<SessionInfo>),
    /// A `session/update` payload.
    Update(Value),
    Permission {
        request: Value,
        title: String,
        options: Vec<PermissionOption>,
        /// The shell command the request is to run, which the user may run themselves first.
        command: Option<String>,
    },
    Stopped(String),
    /// The agent took a steering or follow-up message into the turn (dex daemon only).
    Accepted {
        content: String,
        queue: crate::agent::dex::Queue,
    },
    /// The daemon had no turn to add a steering or follow-up message to (dex daemon only).
    Rejected {
        content: String,
        queue: crate::agent::dex::Queue,
    },
    Failed(String),
    Exited(String),
}

#[derive(Clone)]
enum Pending {
    Init,
    Session,
    Prompt,
    /// A page of `session/list`, counting from 0.
    List(u8),
    Load,
    SetOption {
        id: String,
        value: String,
    },
}

/// `session/list` pages followed before giving up on the rest.
const MAX_PAGES: u8 = 5;

/// Sessions offered for resuming; the picker isn't virtualized, and agents like dex keep hundreds.
const MAX_SESSIONS: usize = 100;

struct Shared {
    /// Taken by every request and reply; followed by hotpath in a profiled build.
    stdin: hotpath::wrap::std::sync::Mutex<ChildStdin>,
    pending: hotpath::wrap::std::sync::Mutex<HashMap<u64, Pending>>,
    next: AtomicU64,
    events: UnboundedSender<Event>,
    cwd: PathBuf,
    session: Mutex<Option<String>>,
    agent: Mutex<String>,
    /// Start by listing sessions to resume instead of opening a new one.
    resume: bool,
    /// With `resume`, the session to load straight away, skipping the listing: the agent scans
    /// every session of the project to list them, which is slow when the one wanted is known.
    wanted: Option<String>,
    listed: Mutex<Vec<SessionInfo>>,
    /// Po's own MCP server for this thread, if the window has a socket to give it.
    po_mcp: Option<Value>,
    /// Whether the agent said, at the handshake, that it takes images in a prompt.
    images: AtomicBool,
    /// The terminals this agent made, released with it.
    terminals: Mutex<Vec<String>>,
}

impl Shared {
    /// What a new or resumed session is handed: Po's own server, which is Po and needs no
    /// approval, then the approved ones of the config.
    fn mcp_servers(&self) -> Value {
        let mut servers = vec![];
        servers.extend(self.po_mcp.clone());
        if let Value::Array(config) =
            crate::config::mcp_servers(&crate::config::plugin::effective_config())
        {
            // A config server named `po` would hide Po's own from the agent.
            servers.extend(
                config
                    .into_iter()
                    .filter(|server| self.po_mcp.is_none() || server["name"] != "po"),
            );
        }
        Value::Array(servers)
    }

    fn write(&self, message: &Value) {
        log::debug!("acp -> {message}");
        let mut stdin = self.stdin.lock().expect("stdin lock");
        if let Err(error) = writeln!(stdin, "{message}").and_then(|_| stdin.flush()) {
            log::error!("acp: write failed: {error}");
        }
    }

    fn request(&self, method: &str, params: Value, kind: Pending) {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.pending.lock().expect("pending lock").insert(id, kind);
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    fn respond(&self, id: Value, result: Value) {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn respond_error(&self, id: Value, code: i64, message: &str) {
        self.write(
            &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        );
    }

    fn list_sessions(&self, cursor: Option<&str>, page: u8) {
        let mut params = json!({ "cwd": self.cwd.to_string_lossy() });
        if let Some(cursor) = cursor {
            params["cursor"] = json!(cursor);
        }
        self.request("session/list", params, Pending::List(page));
    }

    /// Announces the settings a `session/new` or `session/load` result carries, if any.
    fn emit_options(&self, result: &Value) {
        self.emit_listed(&result["configOptions"]);
    }

    /// Announces a list of settings, unless it is missing or empty, which would hide them all.
    fn emit_listed(&self, list: &Value) {
        let options = parse_options(list);
        if !options.is_empty() {
            self.emit(Event::Options(options));
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.unbounded_send(event);
    }

    #[hotpath::measure(impl_type = "Shared")]
    fn handle(self: &Arc<Self>, message: Value) {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(String::from);
        let id = message.get("id").cloned();
        match (method, id) {
            (Some(method), Some(id)) => self.on_request(&method, id, &message["params"]),
            (Some(method), None) => {
                if method == "session/update" {
                    let update = &message["params"]["update"];
                    if update["sessionUpdate"] == "config_option_update" {
                        self.emit_listed(&update["configOptions"]);
                    } else {
                        self.emit(Event::Update(update.clone()));
                    }
                }
            }
            (None, Some(id)) => self.on_response(id, &message),
            _ => {}
        }
    }

    fn on_response(&self, id: Value, message: &Value) {
        let Some(kind) = id
            .as_u64()
            .and_then(|id| self.pending.lock().expect("pending lock").remove(&id))
        else {
            return;
        };
        if let Some(error) = message.get("error") {
            if let Pending::SetOption { id, .. } = kind {
                log::warn!("acp: setting refused: {error}");
                let text = error["message"].as_str().unwrap_or("the agent refused");
                self.emit(Event::Refused {
                    id,
                    text: format!("setting not changed: {text}"),
                });
                return;
            }
            // -32000 is ACP's `authRequired`.
            if error["code"].as_i64() == Some(-32000) {
                let agent = self.agent.lock().expect("agent lock").clone();
                self.emit(Event::Failed(format!(
                    "{agent} needs you to sign in. Run its login here, then start a new thread.\n\n```bash\n{}\n```",
                    if agent.to_lowercase().contains("claude") { "claude /login" } else { "# run the agent's login command" }
                )));
                return;
            }
            let text = error["message"].as_str().unwrap_or("agent error");
            let detail = error["data"]
                .as_str()
                .or(error["data"]["details"].as_str())
                .unwrap_or("");
            self.emit(Event::Failed(format!("{text} {detail}").trim().to_string()));
            return;
        }
        let result = &message["result"];
        match kind {
            Pending::Init => {
                if let Some(name) = result["agentInfo"]["name"].as_str() {
                    *self.agent.lock().expect("agent lock") = name.to_string();
                }
                self.images.store(
                    result["agentCapabilities"]["promptCapabilities"]["image"].as_bool()
                        == Some(true),
                    Ordering::Relaxed,
                );
                if self.resume {
                    let caps = &result["agentCapabilities"];
                    if caps["loadSession"].as_bool() != Some(true)
                        || !caps["sessionCapabilities"]["list"].is_object()
                    {
                        let agent = self.agent.lock().expect("agent lock").clone();
                        self.emit(Event::Failed(format!(
                            "{agent} can't list and resume sessions."
                        )));
                        return;
                    }
                    if let Some(id) = &self.wanted {
                        *self.session.lock().expect("session lock") = Some(id.clone());
                        let cwd = self.cwd.to_string_lossy().to_string();
                        self.request("session/load", json!({ "sessionId": id, "cwd": cwd, "mcpServers": self.mcp_servers() }), Pending::Load);
                        return;
                    }
                    self.list_sessions(None, 0);
                    return;
                }
                let cwd = self.cwd.to_string_lossy().to_string();
                self.request(
                    "session/new",
                    json!({ "cwd": cwd, "mcpServers": self.mcp_servers() }),
                    Pending::Session,
                );
            }
            Pending::List(page) => {
                let mut listed = self.listed.lock().expect("listed lock");
                for entry in result["sessions"].as_array().into_iter().flatten() {
                    let Some(id) = entry["sessionId"].as_str() else {
                        continue;
                    };
                    listed.push(SessionInfo {
                        id: id.to_string(),
                        title: entry["title"]
                            .as_str()
                            .unwrap_or_default()
                            .trim()
                            .to_string(),
                        updated: entry["updatedAt"].as_str().unwrap_or_default().to_string(),
                    });
                }
                match result["nextCursor"]
                    .as_str()
                    .filter(|cursor| !cursor.is_empty())
                {
                    Some(cursor) if page + 1 < MAX_PAGES => {
                        drop(listed);
                        self.list_sessions(Some(cursor), page + 1);
                    }
                    _ => {
                        let mut all = std::mem::take(&mut *listed);
                        drop(listed);
                        all.sort_by(|a, b| b.updated.cmp(&a.updated));
                        all.truncate(MAX_SESSIONS);
                        self.emit(Event::Sessions(all));
                    }
                }
            }
            Pending::SetOption { id, value } => {
                self.emit_options(result);
                self.emit(Event::OptionSet { id, value });
            }
            Pending::Load => {
                self.emit_options(result);
                let agent = self.agent.lock().expect("agent lock").clone();
                self.emit(Event::Ready {
                    agent,
                    fresh: false,
                });
            }
            Pending::Session => {
                *self.session.lock().expect("session lock") =
                    result["sessionId"].as_str().map(String::from);
                self.emit_options(result);
                let agent = self.agent.lock().expect("agent lock").clone();
                self.emit(Event::Ready { agent, fresh: true });
            }
            Pending::Prompt => {
                self.emit(Event::Stopped(
                    result["stopReason"]
                        .as_str()
                        .unwrap_or("end_turn")
                        .to_string(),
                ));
            }
        }
    }

    fn on_request(self: &Arc<Self>, method: &str, id: Value, params: &Value) {
        match method {
            "session/request_permission" => {
                let options = params["options"]
                    .as_array()
                    .map(|all| {
                        all.iter()
                            .filter_map(|o| {
                                Some(PermissionOption {
                                    id: o["optionId"].as_str()?.to_string(),
                                    name: o["name"].as_str()?.to_string(),
                                    kind: o["kind"].as_str().unwrap_or_default().to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let title = params["toolCall"]["title"]
                    .as_str()
                    .unwrap_or("Run a tool")
                    .to_string();
                let command = params["toolCall"]["rawInput"]["command"]
                    .as_str()
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .map(String::from);
                self.emit(Event::Permission {
                    request: id,
                    title,
                    options,
                    command,
                });
            }
            "fs/read_text_file" => match self.read_file(params) {
                Ok(content) => self.respond(id, json!({ "content": content })),
                Err(error) => self.respond_error(id, -32602, &format!("{error:#}")),
            },
            "terminal/create" => match self.create_terminal(params) {
                Ok(terminal) => self.respond(id, json!({ "terminalId": terminal })),
                Err(error) => self.respond_error(id, -32602, &error),
            },
            "terminal/output" => match terminals::output(self.terminal_id(params)) {
                Some((text, truncated, status)) => {
                    let mut result = json!({ "output": plain(&text), "truncated": truncated });
                    if let Some(code) = status {
                        result["exitStatus"] = json!({ "exitCode": code, "signal": null });
                    }
                    self.respond(id, result);
                }
                None => self.respond_error(id, -32602, "no such terminal"),
            },
            "terminal/wait_for_exit" => {
                // On a thread of its own: the reader must keep taking the agent's messages.
                let terminal = self.terminal_id(params).to_string();
                let shared = self.clone();
                let wait = move || {
                    loop {
                        match terminals::output(&terminal) {
                            None => return shared.respond_error(id, -32602, "no such terminal"),
                            Some((_, _, Some(code))) => {
                                return shared
                                    .respond(id, json!({ "exitCode": code, "signal": null }));
                            }
                            Some(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
                        }
                    }
                };
                if std::thread::Builder::new()
                    .name("acp-wait".into())
                    .spawn(wait)
                    .is_err()
                {
                    log::warn!("acp: could not wait for a terminal");
                }
            }
            "terminal/kill" => match terminals::kill(self.terminal_id(params)) {
                true => self.respond(id, json!({})),
                false => self.respond_error(id, -32602, "no such terminal"),
            },
            "terminal/release" => {
                let terminal = self.terminal_id(params);
                if let Ok(mut mine) = self.terminals.lock() {
                    mine.retain(|t| t != terminal);
                }
                match terminals::release(terminal) {
                    true => self.respond(id, json!({})),
                    false => self.respond_error(id, -32602, "no such terminal"),
                }
            }
            _ => self.respond_error(id, -32601, "method not supported"),
        }
    }

    fn terminal_id<'a>(&self, params: &'a Value) -> &'a str {
        params["terminalId"].as_str().unwrap_or_default()
    }

    /// `terminal/create`: runs the command, in the project or a folder inside it.
    fn create_terminal(&self, params: &Value) -> Result<String, String> {
        let program = params["command"].as_str().filter(|c| !c.is_empty());
        let program = program.ok_or("no command")?;
        let root = self.cwd.canonicalize().map_err(|_| "bad project root")?;
        let cwd = match params["cwd"].as_str() {
            Some(cwd) => Path::new(cwd)
                .canonicalize()
                .map_err(|_| "no such folder")?,
            None => root.clone(),
        };
        if !cwd.starts_with(&root) {
            return Err("outside the project".into());
        }
        let text = |v: &Value| v.as_str().map(String::from);
        let launch = Launch {
            program: program.to_string(),
            args: params["args"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(text)
                .collect(),
            env: params["env"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| Some((text(&e["name"])?, text(&e["value"])?)))
                .collect(),
        };
        let limit = params["outputByteLimit"]
            .as_u64()
            .map_or(terminals::DEFAULT_LIMIT, |l| l as usize);
        let id = terminals::create(&launch, &cwd, limit)?;
        if let Ok(mut mine) = self.terminals.lock() {
            mine.push(id.clone());
        }
        Ok(id)
    }

    fn read_file(&self, params: &Value) -> Result<String> {
        let path = params["path"].as_str().context("no path")?;
        let path = Path::new(path).canonicalize().context("no such file")?;
        let root = self.cwd.canonicalize().context("bad project root")?;
        anyhow::ensure!(path.starts_with(&root), "outside the project");
        let text = std::fs::read_to_string(&path).context("not a text file")?;
        let start = params["line"]
            .as_u64()
            .map_or(0, |l| l.saturating_sub(1) as usize);
        let limit = params["limit"].as_u64().map_or(usize::MAX, |l| l as usize);
        if start == 0 && limit == usize::MAX {
            return Ok(text);
        }
        Ok(text
            .lines()
            .skip(start)
            .take(limit)
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

pub struct Client {
    shared: Arc<Shared>,
    child: Arc<Mutex<Child>>,
}

impl Client {
    /// Starts `command` (program and arguments) in `cwd` and begins the handshake.
    /// With `resume`, lists the agent's sessions rather than opening a new one; `load` picks one.
    /// The child's pid is known once it is started, and `control` is then asked for the
    /// environment Po's MCP server for this thread gets; the agent itself does not.
    /// With `resume` and `wanted`, loads that session without listing the others.
    pub fn spawn(
        command: &str,
        cwd: &Path,
        resume: bool,
        wanted: Option<&str>,
        control: impl FnOnce(u32) -> Vec<(String, String)>,
    ) -> Result<(Self, UnboundedReceiver<Event>)> {
        let mut parts = command.split_whitespace();
        let program = parts.next().context("empty agent command")?;
        let mut child = Command::new(program)
            .args(parts)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("could not start `{command}`"))?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let (events, receiver) = hotpath::channel!(unbounded(), label = "acp events");
        let control = control(child.id());
        let shared = Arc::new(Shared {
            stdin: hotpath::mutex!(Mutex::new(stdin), label = "acp stdin"),
            pending: hotpath::mutex!(Mutex::new(HashMap::new()), label = "acp pending"),
            next: AtomicU64::new(1),
            events,
            cwd: cwd.to_path_buf(),
            session: Mutex::new(None),
            agent: Mutex::new(program.to_string()),
            resume,
            wanted: wanted.filter(|_| resume).map(String::from),
            listed: Mutex::new(Vec::new()),
            images: AtomicBool::new(false),
            terminals: Mutex::new(Vec::new()),
            po_mcp: if control.is_empty() {
                None
            } else {
                crate::ipc::mcp::server(&control)
            },
        });
        let child = Arc::new(Mutex::new(child));
        let (reader, reaper) = (shared.clone(), child.clone());
        std::thread::Builder::new()
            .name("acp-read".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    match hotpath::measure_block!("acp parse", serde_json::from_str::<Value>(&line))
                    {
                        Ok(message) => {
                            log::debug!("acp <- {line}");
                            reader.handle(message)
                        }
                        Err(error) => log::debug!("acp: skipped non-JSON line: {error}"),
                    }
                }
                let status = reaper.lock().expect("child lock").wait();
                reader.emit(Event::Exited(
                    status.map_or_else(|e| e.to_string(), |s| s.to_string()),
                ));
            })
            .expect("failed to spawn thread");
        shared.request(
            "initialize",
            json!({
                "protocolVersion": 1,
                // `terminal` runs an agent's commands in Po; the `_meta` keys show the output of
                // the commands an agent runs itself (claude-agent-acp's display terminals).
                "clientCapabilities": {
                    "fs": { "readTextFile": true, "writeTextFile": false },
                    "terminal": true,
                    "_meta": { "terminal_output": true, "terminal_output_delta": true },
                },
                "clientInfo": { "name": "po", "version": "0.1.0" }
            }),
            Pending::Init,
        );
        Ok((Self { shared, child }, receiver))
    }

    /// Whether pictures can go in a prompt as image blocks.
    pub fn takes_images(&self) -> bool {
        self.shared.images.load(Ordering::Relaxed)
    }

    /// Sends `text`, then `images` as image blocks.
    pub fn prompt(&self, text: &str, images: &[Value]) -> bool {
        let Some(session) = self.shared.session.lock().expect("session lock").clone() else {
            return false;
        };
        // An empty text block is refused by the API, so a picture sent alone goes without one.
        let mut blocks = if text.is_empty() {
            vec![]
        } else {
            vec![json!({ "type": "text", "text": text })]
        };
        blocks.extend_from_slice(images);
        self.shared.request(
            "session/prompt",
            json!({ "sessionId": session, "prompt": blocks }),
            Pending::Prompt,
        );
        true
    }

    /// Changes a session setting; the agent answers with all the settings, as [`Event::Options`].
    pub fn set_option(&self, id: &str, value: &str) -> bool {
        let Some(session) = self.shared.session.lock().expect("session lock").clone() else {
            return false;
        };
        self.shared.request(
            "session/set_config_option",
            json!({ "sessionId": session, "configId": id, "value": value }),
            Pending::SetOption {
                id: id.to_string(),
                value: value.to_string(),
            },
        );
        true
    }

    /// Resumes a listed session; the agent replays its history as updates, then answers.
    pub fn load(&self, id: &str) -> bool {
        *self.shared.session.lock().expect("session lock") = Some(id.to_string());
        let cwd = self.shared.cwd.to_string_lossy().to_string();
        self.shared.request(
            "session/load",
            json!({ "sessionId": id, "cwd": cwd, "mcpServers": self.shared.mcp_servers() }),
            Pending::Load,
        );
        true
    }

    /// The session this client has open, once it has one.
    pub fn session(&self) -> Option<String> {
        self.shared.session.lock().expect("session lock").clone()
    }

    pub fn cancel(&self) {
        if let Some(session) = self.shared.session.lock().expect("session lock").clone() {
            self.shared.write(&json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": session } }));
        }
    }

    /// Answers a permission request with an option's id, or cancels it.
    pub fn answer(&self, request: Value, option: Option<&str>) {
        let outcome = match option {
            Some(id) => json!({ "outcome": "selected", "optionId": id }),
            None => json!({ "outcome": "cancelled" }),
        };
        self.shared.respond(request, json!({ "outcome": outcome }));
    }
}

/// What a program wrote, as an agent reads it: without the escapes that colour and move the cursor,
/// and with lines ending in a newline alone.
pub(crate) fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // A control sequence ends at a byte of 0x40..=0x7e after its parameters.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
                    }
                }
                // An operating system command ends at BEL or ESC \.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' if chars.peek() == Some(&'\n') => {}
            c => out.push(c),
        }
    }
    out
}

impl Drop for Client {
    fn drop(&mut self) {
        for id in self
            .shared
            .terminals
            .lock()
            .map(|t| t.clone())
            .unwrap_or_default()
        {
            terminals::release(&id);
        }
        let _ = self.child.lock().expect("child lock").kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_drops_escapes_and_carriage_returns() {
        assert_eq!(plain("\x1b[1;31mred\x1b[0m\r\nnext"), "red\nnext");
        assert_eq!(plain("a\x1b]0;title\x07b\x1b]8;;x\x1b\\c"), "abc");
    }

    #[test]
    fn parses_flat_and_grouped_select_options() {
        let list = json!([
            { "id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "b",
              "options": [{ "value": "a", "name": "A" }, { "value": "b", "name": "B" }] },
            { "configId": "effort", "name": "Effort", "type": "select", "currentValue": "x",
              "options": [{ "group": "g", "name": "G", "options": [{ "value": "x", "name": "X" }] }] },
            { "id": "flag", "name": "Flag", "type": "boolean", "currentValue": true }
        ]);
        let options = parse_options(&list);
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].current, "b");
        assert_eq!(options[1].id, "effort");
        assert_eq!(options[1].choices, vec![("x".to_string(), "X".to_string())]);
    }
}
