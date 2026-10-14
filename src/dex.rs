//! A client for the dex daemon (`dex serve`): HTTP for commands, SSE for a turn's events.
//!
//! Unlike ACP, the daemon lets a client add to a running turn: a *steer* lands before the
//! agent's next model call, a *follow-up* runs once the turn is done, and either can be
//! recalled until the agent has taken it. The daemon's events are translated to the ACP
//! updates the thread already renders.
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::{Value, json};

use crate::acp::{ConfigOption, Event, PermissionOption, SessionInfo};

/// How long a freshly started daemon gets to answer `/health`.
const STARTUP: Duration = Duration::from_secs(15);

/// Sessions offered for resuming.
const MAX_SESSIONS: usize = 100;

/// The daemon's modes, from the most careful, with the permission each needs. It refuses a
/// mode more permissive than its own, and with none named runs at its own.
const MODES: [(&str, &str, &str); 3] = [("plan", "Plan", "read-only"), ("manual", "Manual", "ask"), ("auto", "Auto", "trusted")];

/// Where a steering or follow-up message ended up.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Queue {
    Steer,
    Followup,
}

impl Queue {
    fn route(self) -> &'static str {
        match self {
            Queue::Steer => "steer",
            Queue::Followup => "followup",
        }
    }
}

/// The questions of one `ask_user` batch, answered one at a time and sent together.
struct Batch {
    answers: Vec<Option<Value>>,
}

struct Shared {
    base: String,
    token: Option<String>,
    cwd: PathBuf,
    resume: bool,
    /// Short requests; a stuck daemon shouldn't hang a worker thread forever.
    quick: ureq::Agent,
    /// Event streams, which stay open for a whole turn.
    stream: ureq::Agent,
    session: Mutex<Option<String>>,
    events: UnboundedSender<Event>,
    batches: Mutex<HashMap<String, Batch>>,
    /// The session's settings. The daemon takes them with each message rather than keeping
    /// them, so they are kept here; each option's id is the field of the chat request it fills.
    options: Mutex<Vec<ConfigOption>>,
}

impl Shared {
    fn url(&self, tail: &str) -> String {
        format!("{}{tail}", self.base)
    }

    fn session_url(&self, tail: &str) -> Result<String> {
        let session = self.session.lock().expect("session lock").clone().context("no session yet")?;
        Ok(self.url(&format!("/api/sessions/{session}/{tail}")))
    }

    fn emit(&self, event: Event) {
        let _ = self.events.unbounded_send(event);
    }

    fn post(&self, agent: &ureq::Agent, url: &str, body: &Value) -> Result<ureq::http::Response<ureq::Body>> {
        let mut request = agent.post(url).header("x-dex-protocol", "1");
        if let Some(token) = &self.token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        Ok(request.send_json(body)?)
    }

    fn get(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>> {
        let mut request = self.quick.get(url).header("x-dex-protocol", "1");
        if let Some(token) = &self.token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        Ok(request.call()?)
    }

    fn healthy(&self) -> bool {
        self.get(&self.url("/health")).is_ok()
    }

    /// Opens a session, or lists the resumable ones.
    fn open(&self) -> Result<()> {
        if self.resume {
            let mut response = self.get(&self.url("/api/sessions"))?;
            let listed: Value = response.body_mut().read_json()?;
            let cwd = self.cwd.to_string_lossy();
            let mut sessions: Vec<SessionInfo> = listed
                .as_array()
                .into_iter()
                .flatten()
                .filter(|entry| entry["cwd"].as_str() == Some(&*cwd) && entry["message_count"].as_u64() != Some(0))
                .filter_map(|entry| {
                    let title = entry["first_prompt"].as_str().filter(|t| !t.is_empty()).or(entry["name"].as_str())?;
                    Some(SessionInfo {
                        id: entry["session_id"].as_str()?.to_string(),
                        title: title.trim().to_string(),
                        updated: entry["updated_at"].as_str().unwrap_or_default().to_string(),
                    })
                })
                .collect();
            sessions.sort_by(|a, b| b.updated.cmp(&a.updated));
            sessions.truncate(MAX_SESSIONS);
            self.emit(Event::Sessions(sessions));
            return Ok(());
        }
        let mut response = self.post(&self.quick, &self.url("/api/sessions"), &json!({ "cwd": self.cwd.to_string_lossy() }))?;
        let created: Value = response.body_mut().read_json()?;
        let id = created["session_id"].as_str().context("the daemon returned no session id")?;
        // The daemon falls back to its own directory when it can't use ours, and a daemon from
        // before it honored `cwd` does so always. Running tools in the wrong repo is worse than
        // failing; an empty `cwd` is an older daemon that doesn't say, so nothing to check.
        if let Some(actual) = created["cwd"].as_str().filter(|c| !c.is_empty() && Path::new(c) != self.cwd) {
            bail!("the dex daemon runs this session in {actual}, not {}", self.cwd.display());
        }
        *self.session.lock().expect("session lock") = Some(id.to_string());
        self.load_options();
        self.emit(Event::Ready { agent: "dex".into(), fresh: true });
        Ok(())
    }

    /// Offers the daemon's models, and the modes it allows, as the session's settings. A daemon
    /// that can't say leaves the session without them rather than failing it.
    fn load_options(&self) {
        let info = self.get(&self.url("/api/config")).and_then(|mut response| Ok(response.body_mut().read_json::<Value>()?));
        let options = match info {
            Ok(info) => options_from(&info),
            Err(error) => {
                log::warn!("dex: no session settings: {error:#}");
                return;
            }
        };
        *self.options.lock().expect("options lock") = options.clone();
        if !options.is_empty() {
            self.emit(Event::Options(options));
        }
    }

    /// Reads one turn's SSE stream to its end, then forgets any questions it left unanswered.
    fn run_turn(&self, prompt: &str) -> Result<()> {
        let result = self.stream_turn(prompt);
        self.batches.lock().expect("batches lock").clear();
        result
    }

    fn stream_turn(&self, prompt: &str) -> Result<()> {
        let url = self.session_url("chat")?;
        let mut body = json!({ "prompt": prompt });
        for option in self.options.lock().expect("options lock").iter() {
            body[&option.id] = json!(option.current);
        }
        let response = match self.post(&self.stream, &url, &body) {
            Ok(response) => response,
            Err(error) if is_status(&error, 409) => return Err(anyhow!("The agent is still working on the last message.")),
            Err(error) => return Err(error),
        };
        let mut mapper = Mapper::default();
        let mut finished = false;
        for line in BufReader::new(response.into_body().into_reader()).lines() {
            let line = line.context("the connection to dex dropped")?;
            let Some(data) = line.strip_prefix("data:").map(str::trim).filter(|d| !d.is_empty() && *d != "ping") else {
                continue;
            };
            let Ok(envelope) = serde_json::from_str::<Value>(data) else { continue };
            for event in mapper.map(&envelope, false, self) {
                finished |= matches!(event, Event::Stopped(_) | Event::Failed(_));
                self.emit(event);
            }
            if finished {
                break;
            }
        }
        if !finished {
            return Err(anyhow!("dex closed the stream before the turn ended"));
        }
        Ok(())
    }

    /// Replays a session's journal as updates, then reports it ready.
    fn replay(&self) -> Result<()> {
        let base = self.session_url("reattach")?;
        let mut response = self.post(&self.quick, &base, &json!({}))?;
        let _: Value = response.body_mut().read_json()?;
        let mut since = 0u64;
        let mut mapper = Mapper::default();
        loop {
            let url = format!("{}?since={since}", self.session_url("events")?);
            let mut response = self.get(&url)?;
            let page: Value = response.body_mut().read_json()?;
            let events = page["events"].as_array().cloned().unwrap_or_default();
            let next = page["next_seq"].as_u64().unwrap_or(since);
            for envelope in &events {
                for event in mapper.map(envelope, true, self) {
                    self.emit(event);
                }
            }
            if events.is_empty() || next <= since {
                break;
            }
            since = next;
        }
        self.load_options();
        self.emit(Event::Ready { agent: "dex".into(), fresh: true });
        Ok(())
    }

    fn fail(&self, error: anyhow::Error) {
        self.emit(Event::Failed(format!("{error:#}")));
    }

    fn resolve_question(&self, batch: &str, index: usize, answer: Value) {
        let done = {
            let mut batches = self.batches.lock().expect("batches lock");
            let Some(entry) = batches.get_mut(batch) else { return };
            if let Some(slot) = entry.answers.get_mut(index) {
                *slot = Some(answer);
            }
            if entry.answers.iter().all(Option::is_some) { batches.remove(batch) } else { None }
        };
        if let Some(entry) = done {
            let answers: Vec<Value> = entry.answers.into_iter().flatten().collect();
            let body = json!({ "request_id": batch, "answers": answers });
            if let Err(error) = self.session_url("answer").and_then(|url| self.post(&self.quick, &url, &body)) {
                log::error!("dex: answering failed: {error:#}");
            }
        }
    }
}

/// The settings `GET /api/config` offers: the model, when there is more than one to pick, and
/// the mode, when the daemon allows more than one.
fn options_from(info: &Value) -> Vec<ConfigOption> {
    let mut options = Vec::new();
    let current = info["model"].as_str().unwrap_or_default();
    let mut models: Vec<&str> = info["available_models"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !current.is_empty() && !models.contains(&current) {
        models.insert(0, current);
    }
    if !current.is_empty() && models.len() > 1 {
        options.push(ConfigOption {
            id: "model".into(),
            name: "Model".into(),
            category: "model".into(),
            current: current.into(),
            choices: models.into_iter().map(|model| (model.to_string(), model.to_string())).collect(),
        });
    }
    let permission = info["permission"].as_str().unwrap_or_default().to_ascii_lowercase().replace('_', "-");
    if let Some(ceiling) = MODES.iter().position(|(_, _, needs)| *needs == permission).filter(|&at| at > 0) {
        options.push(ConfigOption {
            id: "mode".into(),
            name: "Mode".into(),
            category: "mode".into(),
            current: MODES[ceiling].0.into(),
            choices: MODES[..=ceiling].iter().map(|(value, name, _)| (value.to_string(), name.to_string())).collect(),
        });
    }
    options
}

/// Whether a bearer token may be sent to `url`: https, or a loopback address.
fn private_transport(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else { return false };
    let host = rest.split(['/', '?']).next().unwrap_or_default();
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn is_status(error: &anyhow::Error, code: u16) -> bool {
    matches!(error.downcast_ref::<ureq::Error>(), Some(ureq::Error::StatusCode(status)) if *status == code)
}

/// Translates the daemon's events to ACP updates, remembering which tool calls are open.
#[derive(Default)]
struct Mapper {
    /// Tool calls started and not yet resulted, as (tool name, call id).
    open: Vec<(String, String)>,
    counter: u32,
}

impl Mapper {
    fn map(&mut self, envelope: &Value, replay: bool, shared: &Shared) -> Vec<Event> {
        let data = &envelope["data"];
        let update = |value: Value| vec![Event::Update(value)];
        match envelope["type"].as_str().unwrap_or_default() {
            "assistant_text" => update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "text": data } })),
            "thinking" => update(json!({ "sessionUpdate": "agent_thought_chunk", "content": { "text": data } })),
            "tool_call" => {
                let name = data["name"].as_str().unwrap_or("tool").to_string();
                let mut id = data["id"].as_str().unwrap_or_default().to_string();
                if id.is_empty() {
                    self.counter += 1;
                    id = format!("call-{}", self.counter);
                }
                self.open.push((name.clone(), id.clone()));
                let args = &data["args"];
                let title = match args.as_str().filter(|a| !a.is_empty()) {
                    Some(arg) => format!("{name} {arg}"),
                    None => name,
                };
                update(json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": title, "status": "in_progress", "rawInput": args }))
            }
            "tool_result" => {
                let name = data["name"].as_str().unwrap_or_default();
                let mut id = data["id"].as_str().unwrap_or_default().to_string();
                if id.is_empty() {
                    // Journals from before ids pair a result with the oldest open call of that tool.
                    if let Some(at) = self.open.iter().position(|(open, _)| open == name) {
                        id = self.open[at].1.clone();
                    }
                }
                self.open.retain(|(_, open)| *open != id);
                let mut text = data["summary"].as_str().unwrap_or_default().to_string();
                for line in data["preview"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    text.push('\n');
                    text.push_str(line);
                }
                let status = if data["success"].as_bool().unwrap_or(true) { "completed" } else { "failed" };
                update(json!({
                    "sessionUpdate": "tool_call_update", "toolCallId": id, "status": status,
                    "content": [{ "type": "content", "content": { "text": text } }]
                }))
            }
            "plan" => {
                let entries: Vec<Value> = data["steps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|step| json!({ "content": step[0], "status": if step[1].as_bool() == Some(true) { "completed" } else { "pending" } }))
                    .collect();
                update(json!({ "sessionUpdate": "plan", "entries": entries }))
            }
            "approval_required" if !replay => {
                let name = data["name"].as_str().unwrap_or("a tool");
                let who = data["agent"].as_str().map(|agent| format!("[{agent}] ")).unwrap_or_default();
                vec![Event::Permission {
                    request: json!({ "approval": data["request_id"] }),
                    title: format!("{who}{name}: {}", data["input"].as_str().unwrap_or_default().chars().take(200).collect::<String>()),
                    options: vec![
                        PermissionOption { id: "allow_once".into(), name: "Allow once".into(), kind: "allow_once".into() },
                        PermissionOption { id: "allow_session".into(), name: "Allow for session".into(), kind: "allow_always".into() },
                        PermissionOption { id: "deny".into(), name: "Deny".into(), kind: "reject_once".into() },
                    ],
                }]
            }
            "question_required" if !replay => {
                let id = data["request_id"].as_str().unwrap_or_default().to_string();
                let questions = data["questions"].as_array().cloned().unwrap_or_default();
                shared.batches.lock().expect("batches lock").insert(id.clone(), Batch { answers: vec![None; questions.len()] });
                questions
                    .iter()
                    .enumerate()
                    .map(|(index, question)| Event::Permission {
                        request: json!({ "question": id, "index": index }),
                        title: question["question"].as_str().unwrap_or("The agent has a question").to_string(),
                        options: question["options"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .enumerate()
                            .map(|(n, option)| PermissionOption {
                                id: n.to_string(),
                                name: option["label"].as_str().unwrap_or_default().to_string(),
                                kind: option["kind"].as_str().unwrap_or_default().to_string(),
                            })
                            .collect(),
                    })
                    .collect()
            }
            "user_prompt" if replay => update(json!({ "sessionUpdate": "user_message_chunk", "content": { "text": data["content"] } })),
            "steering_accepted" | "followup_accepted" => {
                let content = data["content"].as_str().unwrap_or_default().to_string();
                if replay {
                    update(json!({ "sessionUpdate": "user_message_chunk", "content": { "text": content } }))
                } else {
                    let queue = if envelope["type"] == "steering_accepted" { Queue::Steer } else { Queue::Followup };
                    vec![Event::Accepted { content, queue }]
                }
            }
            "error" => update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "text": format!("\n\n**Error:** {}\n\n", data.as_str().unwrap_or_default()) } })),
            "turn_complete" if !replay => vec![Event::Stopped("end_turn".into())],
            "turn_failed" if !replay => vec![Event::Failed(data["error"].as_str().unwrap_or("the turn failed").to_string())],
            _ => vec![],
        }
    }
}

/// The daemon dies with po even when po is killed: a job object that kills its members when the
/// last handle to it closes, which is when po exits. The Windows counterpart of `PR_SET_PDEATHSIG`.
#[cfg(windows)]
fn kill_with_po(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    // The handle is never closed: it lives as long as po does.
    static JOB: OnceLock<usize> = OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if !job.is_null() {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
        }
        job as usize
    });
    if job != 0 {
        // SAFETY: both handles are valid; a failure just leaves the daemon unmanaged.
        unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) };
    }
}

/// A daemon po started. Threads share one daemon per URL, so it belongs to the process rather
/// than to the client that happened to start it, and goes away with the last client using it.
struct Owned {
    child: Child,
    /// Dropped with the daemon; the worker that forked it waits on its other end, because the
    /// kernel ties the daemon to that thread rather than to the process.
    _release: std::sync::mpsc::Sender<()>,
}

#[derive(Default)]
struct Registry {
    /// Live clients per daemon URL.
    users: HashMap<String, usize>,
    owned: HashMap<String, Owned>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

/// Held while a daemon is checked for and started, so threads opened together start one.
static STARTUP_LOCK: Mutex<()> = Mutex::new(());

fn registry<R>(work: impl FnOnce(&mut Registry) -> R) -> R {
    work(REGISTRY.lock().expect("registry lock").get_or_insert_with(Registry::default))
}

pub struct Client {
    shared: Arc<Shared>,
    /// The URL this client counts as a user of.
    url: String,
}

impl Client {
    /// Connects to the daemon at `url`, starting `command` (e.g. `dex serve`) if nothing answers.
    pub fn spawn(url: &str, command: &str, cwd: &Path, resume: bool) -> Result<(Self, UnboundedReceiver<Event>)> {
        let (events, receiver) = unbounded();
        let agent = |timeout: Option<Duration>| -> ureq::Agent {
            ureq::Agent::config_builder().timeout_global(timeout).build().into()
        };
        let shared = Arc::new(Shared {
            base: url.trim_end_matches('/').to_string(),
            token: std::env::var("DEX_DAEMON_TOKEN")
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .filter(|_| {
                    let safe = private_transport(url);
                    if !safe {
                        log::warn!("dex: not sending DEX_DAEMON_TOKEN over plain http to {url}");
                    }
                    safe
                }),
            cwd: cwd.to_path_buf(),
            resume,
            quick: agent(Some(Duration::from_secs(15))),
            stream: agent(None),
            session: Mutex::new(None),
            events,
            batches: Mutex::new(HashMap::new()),
            options: Mutex::new(Vec::new()),
        });
        let url = shared.base.clone();
        registry(|r| *r.users.entry(url.clone()).or_default() += 1);
        let (worker, command, cwd) = (shared.clone(), command.to_string(), cwd.to_path_buf());
        std::thread::spawn(move || {
            let mut held = None;
            let result = (|| -> Result<()> {
                let _starting = STARTUP_LOCK.lock().expect("startup lock");
                if !worker.healthy() {
                    let mut parts = command.split_whitespace();
                    let program = parts.next().context("the dex daemon isn't running and no command to start it is configured")?;
                    let mut process = Command::new(program);
                    process.args(parts);
                    // The daemon dies with po even when po is killed and never runs `Drop`.
                    #[cfg(target_os = "linux")]
                    {
                        use std::os::unix::process::CommandExt;
                        // SAFETY: only async-signal-safe `prctl` runs between fork and exec.
                        unsafe {
                            process.pre_exec(|| {
                                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                                Ok(())
                            });
                        }
                    }
                    let child = process
                        .current_dir(&cwd)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::inherit())
                        .spawn()
                        .with_context(|| format!("could not start `{command}`"))?;
                    #[cfg(windows)]
                    kill_with_po(&child);
                    let (release, hold) = std::sync::mpsc::channel::<()>();
                    let adopted = registry(|r| {
                        // Every client using this daemon was dropped while it started.
                        if r.users.get(&worker.base).copied().unwrap_or(0) == 0 {
                            return Err(child);
                        }
                        r.owned.insert(worker.base.clone(), Owned { child, _release: release });
                        Ok(())
                    });
                    if let Err(mut child) = adopted {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Ok(());
                    }
                    held = Some(hold);
                    let deadline = Instant::now() + STARTUP;
                    while !worker.healthy() {
                        let status = registry(|r| {
                            if r.users.get(&worker.base).copied().unwrap_or(0) == 0 {
                                return Err(None);
                            }
                            match r.owned.get_mut(&worker.base).and_then(|o| o.child.try_wait().ok().flatten()) {
                                Some(status) => Err(Some(status)),
                                None => Ok(()),
                            }
                        });
                        match status {
                            Err(None) => return Ok(()),
                            Err(Some(status)) => {
                                registry(|r| r.owned.remove(&worker.base));
                                return Err(anyhow!("`{command}` exited ({status}) before the daemon came up"));
                            }
                            Ok(()) => {}
                        }
                        if Instant::now() > deadline {
                            return Err(anyhow!("the dex daemon didn't come up at {}", worker.base));
                        }
                        std::thread::sleep(Duration::from_millis(150));
                    }
                }
                worker.open()
            })();
            if let Err(error) = result {
                worker.fail(error);
            }
            // Outlive the daemon this worker forked.
            if let Some(hold) = held {
                let _ = hold.recv();
            }
        });
        Ok((Self { shared, url }, receiver))
    }

    fn background(&self, work: impl FnOnce(&Shared) -> Result<()> + Send + 'static) {
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            if let Err(error) = work(&shared) {
                shared.fail(error);
            }
        });
    }

    pub fn prompt(&self, text: &str) -> bool {
        if self.shared.session.lock().expect("session lock").is_none() {
            return false;
        }
        let text = text.to_string();
        self.background(move |shared| shared.run_turn(&text));
        true
    }

    pub fn load(&self, id: &str) -> bool {
        *self.shared.session.lock().expect("session lock") = Some(id.to_string());
        self.background(|shared| shared.replay());
        true
    }

    /// The session this client has open, once it has one.
    pub fn session(&self) -> Option<String> {
        self.shared.session.lock().expect("session lock").clone()
    }

    pub fn cancel(&self) {
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            if let Err(error) = shared.session_url("cancel").and_then(|url| shared.post(&shared.quick, &url, &json!({}))) {
                log::error!("dex: cancel failed: {error:#}");
            }
        });
    }

    /// Answers a permission or question. `option` is the chosen option's id; `None` declines.
    pub fn answer(&self, request: Value, option: Option<&str>) {
        let option = option.map(String::from);
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            if let Some(id) = request["question"].as_str() {
                let index = request["index"].as_u64().unwrap_or(0) as usize;
                let answer = match option.and_then(|o| o.parse::<usize>().ok()) {
                    Some(choice) => json!({ "type": "choice", "data": choice }),
                    None => json!({ "type": "dismiss" }),
                };
                shared.resolve_question(id, index, answer);
                return;
            }
            let decision = match option.as_deref() {
                Some("allow_once") => "allow_once",
                Some("allow_session") => "allow_session",
                _ => "deny",
            };
            let body = json!({ "request_id": request["approval"], "decision": decision });
            if let Err(error) = shared.session_url("approve").and_then(|url| shared.post(&shared.quick, &url, &body)) {
                log::error!("dex: approving failed: {error:#}");
            }
        });
    }

    /// Changes a session setting. It goes with the next message, as the daemon takes settings
    /// with each one, so it is taken at once.
    pub fn set_option(&self, id: &str, value: &str) -> bool {
        if self.session().is_none() {
            return false;
        }
        let options = {
            let mut options = self.shared.options.lock().expect("options lock");
            let Some(option) = options.iter_mut().find(|o| o.id == id && o.choices.iter().any(|(choice, _)| choice == value)) else {
                return false;
            };
            option.current = value.to_string();
            options.clone()
        };
        self.shared.emit(Event::Options(options));
        self.shared.emit(Event::OptionSet { id: id.to_string(), value: value.to_string() });
        true
    }

    /// Adds `content` to the running turn. If the daemon has no turn to add it to, the
    /// reader hears `Event::Rejected` and can send it as a prompt instead.
    pub fn enqueue(&self, content: &str, queue: Queue) {
        let (shared, content) = (self.shared.clone(), content.to_string());
        std::thread::spawn(move || {
            let sent = shared
                .session_url(queue.route())
                .and_then(|url| shared.post(&shared.quick, &url, &json!({ "content": content })));
            let Err(error) = sent else { return };
            // Only a refusal means the daemon didn't take it. After a timeout or a dropped
            // connection it may have, and `Event::Accepted` will say so; if it never does, the
            // message is sent when the turn ends.
            let refused = error.downcast_ref::<ureq::Error>().is_none_or(|e| matches!(e, ureq::Error::StatusCode(_)));
            if refused {
                log::warn!("dex: {} not accepted: {error:#}", queue.route());
                shared.emit(Event::Rejected { content, queue });
            } else {
                log::warn!("dex: {} may not have been accepted: {error:#}", queue.route());
            }
        });
    }

    /// Takes a queued message back before the agent has taken it. The reader has already
    /// dropped it; if the agent got there first, `Event::Accepted` still follows.
    pub fn recall(&self, content: &str, queue: Queue) {
        let (shared, content) = (self.shared.clone(), content.to_string());
        std::thread::spawn(move || {
            let body = json!({ "content": content, "followup": queue == Queue::Followup });
            if let Err(error) = shared.session_url("recall").and_then(|url| shared.post(&shared.quick, &url, &body)) {
                log::warn!("dex: recall failed: {error:#}");
            }
        });
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // The daemon po started goes with its last user; one that was already running is left alone.
        let last = registry(|r| match r.users.get_mut(&self.url) {
            Some(n) if *n > 1 => {
                *n -= 1;
                None
            }
            _ => {
                r.users.remove(&self.url);
                r.owned.remove(&self.url)
            }
        });
        if let Some(mut owned) = last {
            let _ = owned.child.kill();
            let _ = owned.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The next event other than the session's settings, which a real daemon sends before `Ready`.
    fn next_past_options(events: &mut UnboundedReceiver<Event>) -> Option<Event> {
        loop {
            match next(events)? {
                Event::Options(_) => continue,
                event => return Some(event),
            }
        }
    }

    fn next(events: &mut UnboundedReceiver<Event>) -> Option<Event> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Ok(event) = events.try_recv() {
                return Some(event);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    /// A client state with no daemon behind it, for the pure event mapping.
    fn idle() -> Shared {
        let (events, _) = unbounded();
        Shared {
            base: String::new(),
            token: None,
            cwd: PathBuf::new(),
            resume: false,
            quick: ureq::Agent::new_with_defaults(),
            stream: ureq::Agent::new_with_defaults(),
            session: Mutex::new(None),
            events,
            batches: Mutex::new(HashMap::new()),
            options: Mutex::new(Vec::new()),
        }
    }

    fn mapped(value: Value) -> Vec<Event> {
        let shared = idle();
        Mapper::default().map(&value, false, &shared)
    }

    #[test]
    fn maps_tool_calls_and_results_by_id() {
        let mut mapper = Mapper::default();
        let shared = idle();
        let call = json!({ "seq": 1, "type": "tool_call", "data": { "name": "read", "args": "src/main.rs", "id": "t1" } });
        let result = json!({ "seq": 2, "type": "tool_result", "data": { "name": "read", "summary": "ok", "success": false, "preview": ["a"], "id": "" } });
        let Some(Event::Update(call)) = mapper.map(&call, false, &shared).pop() else { panic!("no update") };
        assert_eq!(call["toolCallId"], "t1");
        assert_eq!(call["title"], "read src/main.rs");
        // A result without an id pairs with the open call of the same tool.
        let Some(Event::Update(result)) = mapper.map(&result, false, &shared).pop() else { panic!("no update") };
        assert_eq!(result["toolCallId"], "t1");
        assert_eq!(result["status"], "failed");
        assert_eq!(result["content"][0]["content"]["text"], "ok\na");
    }

    #[test]
    fn tokens_only_go_to_private_transports() {
        assert!(private_transport("http://127.0.0.1:8420"));
        assert!(private_transport("http://localhost/x"));
        assert!(private_transport("http://[::1]:8420"));
        assert!(private_transport("https://dex.example.com"));
        assert!(!private_transport("http://dex.example.com:8420"));
        assert!(!private_transport("http://127.0.0.1.evil.com"));
    }

    #[test]
    fn maps_queue_events_live_and_in_replay() {
        let accepted = json!({ "seq": 3, "type": "steering_accepted", "data": { "content": "use tabs" } });
        assert!(matches!(&mapped(accepted)[..], [Event::Accepted { content, queue: Queue::Steer }] if content == "use tabs"));
        let done = json!({ "seq": 4, "type": "turn_complete", "data": { "response": "x" } });
        assert!(matches!(&mapped(done)[..], [Event::Stopped(_)]));
        let failed = json!({ "seq": 5, "type": "turn_failed", "data": { "error": "boom" } });
        assert!(matches!(&mapped(failed)[..], [Event::Failed(e)] if e == "boom"));
    }

    /// Serves one canned SSE response to the first request that arrives.
    fn serve_once(body: String) -> String {
        serve_and_capture(body).0
    }

    /// Like [`serve_once`], also handing over the request's JSON body.
    fn serve_and_capture(body: String) -> (String, std::sync::mpsc::Receiver<Value>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut seen = Vec::new();
            let mut chunk = [0u8; 1024];
            let end = loop {
                if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
                let n = stream.read(&mut chunk).unwrap();
                seen.extend_from_slice(&chunk[..n]);
            };
            let head = String::from_utf8_lossy(&seen[..end]).to_ascii_lowercase();
            let length = head.lines().find_map(|line| line.strip_prefix("content-length:")).and_then(|n| n.trim().parse().ok()).unwrap_or(0);
            while seen.len() < end + length {
                let n = stream.read(&mut chunk).unwrap();
                seen.extend_from_slice(&chunk[..n]);
            }
            let _ = sent.send(serde_json::from_slice(&seen[end..end + length]).unwrap_or(Value::Null));
            let head = format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}");
            stream.write_all(head.as_bytes()).unwrap();
        });
        (url, received)
    }

    #[test]
    fn offers_the_models_and_the_modes_the_daemon_allows() {
        let info = json!({ "model": "opus", "available_models": ["sonnet", "haiku"], "permission": "ask" });
        let options = options_from(&info);
        assert_eq!(options.len(), 2);
        assert_eq!((options[0].id.as_str(), options[0].current.as_str()), ("model", "opus"));
        // The daemon's own model is offered even when its list leaves it out.
        let models: Vec<&str> = options[0].choices.iter().map(|(value, _)| value.as_str()).collect();
        assert_eq!(models, ["opus", "sonnet", "haiku"]);
        // An `ask` daemon refuses `auto`, so it isn't offered.
        assert_eq!((options[1].id.as_str(), options[1].current.as_str()), ("mode", "manual"));
        let modes: Vec<&str> = options[1].choices.iter().map(|(value, _)| value.as_str()).collect();
        assert_eq!(modes, ["plan", "manual"]);
        // One model and a read-only daemon leave nothing to choose.
        assert!(options_from(&json!({ "model": "opus", "available_models": [], "permission": "read-only" })).is_empty());
        assert!(options_from(&json!({})).is_empty());
    }

    #[test]
    fn a_setting_is_taken_at_once_and_goes_with_the_next_turn() {
        let done = json!({ "seq": 0, "type": "turn_complete", "data": { "response": "ok" } });
        let (base, request) = serve_and_capture(format!("data: {done}\n\n"));
        let (events, mut receiver) = unbounded();
        let shared = Arc::new(Shared { base, events, ..idle() });
        *shared.options.lock().unwrap() = options_from(&json!({ "model": "opus", "available_models": ["sonnet"], "permission": "trusted" }));
        let client = Client { shared: shared.clone(), url: String::new() };
        assert!(!client.set_option("model", "sonnet"), "set before there is a session");
        *shared.session.lock().unwrap() = Some("s1".into());
        assert!(!client.set_option("model", "gpt"), "set to a model the daemon doesn't offer");
        assert!(client.set_option("model", "sonnet"));
        assert!(client.set_option("mode", "plan"));
        assert!(matches!(receiver.try_recv(), Ok(Event::Options(options)) if options[0].current == "sonnet"));
        assert!(matches!(receiver.try_recv(), Ok(Event::OptionSet { id, value }) if id == "model" && value == "sonnet"));
        shared.run_turn("hi").unwrap();
        let body = request.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(body, json!({ "prompt": "hi", "model": "sonnet", "mode": "plan" }));
    }

    #[test]
    fn a_turn_streams_text_a_steer_and_the_end() {
        let frames = [
            json!({ "seq": 0, "type": "assistant_text", "data": "Looking" }),
            json!({ "seq": 1, "type": "tool_call", "data": { "name": "read", "args": "a.rs", "id": "t1" } }),
            json!({ "seq": 2, "type": "tool_result", "data": { "name": "read", "summary": "ok", "success": true, "id": "t1" } }),
            json!({ "seq": 3, "type": "steering_accepted", "data": { "content": "use tabs" } }),
            json!({ "seq": 4, "type": "turn_complete", "data": { "response": "done" } }),
        ];
        let body: String = frames.iter().map(|f| format!("data: {f}\n\n")).collect();
        let (events, mut receiver) = unbounded();
        let shared = Shared { base: serve_once(format!("data: ping\n\n{body}")), events, ..idle() };
        *shared.session.lock().unwrap() = Some("s1".into());
        shared.run_turn("hi").unwrap();
        let mut seen = vec![];
        while let Ok(event) = receiver.try_recv() {
            seen.push(event);
        }
        assert!(matches!(&seen[..], [
            Event::Update(_), Event::Update(_), Event::Update(_),
            Event::Accepted { queue: Queue::Steer, .. }, Event::Stopped(_),
        ]));
    }

    #[test]
    fn a_stream_that_ends_early_is_an_error() {
        let (events, _receiver) = unbounded();
        let frame = json!({ "seq": 0, "type": "assistant_text", "data": "Hi" });
        let shared = Shared { base: serve_once(format!("data: {frame}\n\n")), events, ..idle() };
        *shared.session.lock().unwrap() = Some("s1".into());
        assert!(shared.run_turn("hi").is_err());
    }

    /// Needs a `dex` on PATH; starts a real daemon and talks to it.
    #[test]
    fn talks_to_a_real_daemon() {
        if Command::new("dex").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_err() {
            eprintln!("skipped: no dex on PATH");
            return;
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let dir = std::env::temp_dir().join(format!("po-dex-test-{port}"));
        std::fs::create_dir_all(&dir).unwrap();
        let (client, mut events) = Client::spawn(&format!("http://127.0.0.1:{port}"), &format!("dex serve 127.0.0.1:{port}"), &dir, false).unwrap();
        assert!(matches!(next_past_options(&mut events), Some(Event::Ready { .. })), "no session");
        // With no turn running there is nothing to steer.
        client.enqueue("hello", Queue::Steer);
        assert!(matches!(next(&mut events), Some(Event::Rejected { queue: Queue::Steer, .. })));
        client.enqueue("later", Queue::Followup);
        assert!(matches!(next(&mut events), Some(Event::Rejected { queue: Queue::Followup, .. })));
        drop(client);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Threads share one daemon: it outlives the client that started it and dies with the last.
    #[test]
    fn a_shared_daemon_goes_with_its_last_client() {
        if Command::new("dex").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_err() {
            eprintln!("skipped: no dex on PATH");
            return;
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let dir = std::env::temp_dir().join(format!("po-dex-share-{port}"));
        std::fs::create_dir_all(&dir).unwrap();
        let (url, command) = (format!("http://127.0.0.1:{port}"), format!("dex serve 127.0.0.1:{port}"));
        let (first, mut first_events) = Client::spawn(&url, &command, &dir, false).unwrap();
        let (second, mut second_events) = Client::spawn(&url, &command, &dir, false).unwrap();
        assert!(matches!(next_past_options(&mut first_events), Some(Event::Ready { .. })));
        assert!(matches!(next_past_options(&mut second_events), Some(Event::Ready { .. })));
        let alive = || ureq::get(&format!("{url}/health")).call().is_ok();
        drop(first);
        assert!(alive(), "dropping the client that started the daemon killed it");
        drop(second);
        std::thread::sleep(Duration::from_millis(500));
        assert!(!alive(), "the daemon outlived its last client");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
