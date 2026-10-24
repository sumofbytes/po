//! How a process outside the window, Po's MCP server for an agent, asks Po for something: a
//! local socket that takes one JSON request per connection. A thread is identified by its token
//! *and* its process: the token says which thread the connection claims to be, and the peer's
//! process chain must reach the agent Po started for it, so a token taken from another thread's
//! environment is useless from outside that thread's agent. A connection with no token is the
//! user's only if no agent Po spawned sits among its ancestors; the socket's directory is only
//! this user's to enter, which is the rest of that check.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, mpsc::Sender};

use anyhow::Result;
use futures::channel::mpsc::UnboundedReceiver;
use serde_json::Value;

use crate::command::{Caller, Command, Reply};

/// A parsed request for the window to answer.
pub struct Request {
    pub caller: Caller,
    pub command: Command,
    pub reply: Sender<Reply>,
}

/// What a token stands for: the thread it was made for, and the agent process Po started for
/// it, whose descendants the token works from. `None` where the thread has no process of its
/// own (a shared daemon), which leaves the token to say who is asking.
struct Registered {
    thread: u64,
    agent: Option<u32>,
}

type Tokens = Arc<Mutex<HashMap<String, Registered>>>;
/// The pids of every agent process Po started, so that `po ctl` run from inside one is refused.
/// A pid here is never forgotten, so a reused one is refused too: refusing is the safe side.
type Agents = Arc<Mutex<HashSet<u32>>>;

#[derive(Clone)]
pub struct Control {
    tokens: Tokens,
    agents: Agents,
    socket: Arc<SocketFile>,
}

/// The socket's file, removed when the last clone of [`Control`] goes: a clone that is dropped
/// early (a closure that registered a thread) must not take the socket away from the server.
struct SocketFile(std::path::PathBuf);

impl Drop for SocketFile {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

impl Control {
    /// The environment that lets thread `id`'s MCP server reach this window. `agent` is the pid
    /// of the process Po started for the thread; a token then works only from that process and
    /// its descendants. A thread asking again gets a new token, and the old one stops working.
    pub fn register(&self, id: u64, agent: Option<u32>) -> Vec<(String, String)> {
        let token = token();
        let mut tokens = self.tokens.lock().expect("tokens lock");
        tokens.retain(|_, registered| registered.thread != id);
        tokens.insert(token.clone(), Registered { thread: id, agent });
        if let Some(pid) = agent {
            self.agents.lock().expect("agents lock").insert(pid);
        }
        vec![("PO_SOCKET".into(), self.socket.0.to_string_lossy().into_owned()), ("PO_TOKEN".into(), token)]
    }
}

fn token() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    // Without a random source there is no token worth handing out, so there is no socket.
    std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes)).expect("random token");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// What the connection at the other end of a socket looks like to us: its process, when the
/// platform can read it, and whose user it runs as, when the kernel says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    pub pid: Option<u32>,
    pub uid: Option<u32>,
}

impl Peer {
    /// The peer's ancestors, with the peer first, as far up as `parent` (the process table)
    /// goes. Capped, so a cyclic process table can't spin here.
    fn chain_with(self, parent: &dyn Fn(u32) -> Option<u32>) -> Vec<u32> {
        let Some(start) = self.pid else { return vec![] };
        let mut chain = vec![start];
        let mut current = start;
        for _ in 0..64 {
            match parent(current) {
                Some(parent) if parent > 0 && parent != current => {
                    chain.push(parent);
                    current = parent;
                }
                _ => break,
            }
        }
        chain
    }
}

/// Who a connection is, judged by what it claims and by the process it comes from. Pure, so it
/// is testable. `our_uid` is this process's.
fn classify(token: Option<&str>, peer: Peer, tokens: &HashMap<String, Registered>, agents: &HashSet<u32>, our_uid: u32) -> std::result::Result<Caller, String> {
    classify_with(token, peer, tokens, agents, our_uid, &peer::parent)
}

/// [`classify`], with the process table (a process's parent) given.
fn classify_with(
    token: Option<&str>,
    peer: Peer,
    tokens: &HashMap<String, Registered>,
    agents: &HashSet<u32>,
    our_uid: u32,
    parent: &dyn Fn(u32) -> Option<u32>,
) -> std::result::Result<Caller, String> {
    match token {
        Some(token) => {
            let registered = tokens.get(token).ok_or("unknown token")?;
            match (registered.agent, peer.pid) {
                // No agent pid registered (a shared daemon): the token alone says who.
                (None, _) => Ok(Caller::Thread(registered.thread)),
                // Bound to an agent, and the platform can't read the peer: nothing to prove
                // the chain with, so refuse rather than take the token at its word.
                (Some(_), None) => Err("cannot tell who is asking".into()),
                (Some(_), Some(_)) if peer.chain_with(parent).contains(&registered.agent.unwrap()) => Ok(Caller::Thread(registered.thread)),
                (Some(_), Some(_)) => Err("this token works only inside its own thread's agent".into()),
            }
        }
        None => {
            // Where the platform can read a peer but didn't (a connection other than ours,
            // say), there is nothing to go by: refuse.
            if peer::CAN_READ_PEER && peer.pid.is_none() {
                return Err("cannot tell who is asking".into());
            }
            if peer.uid.is_some_and(|uid| uid != our_uid) {
                return Err("not this user's connection".into());
            }
            let chain = peer.chain_with(parent);
            if chain.iter().any(|pid| agents.contains(pid)) {
                return Err("no control from inside an agent's process".into());
            }
            // The user is whoever is *not* an agent's, so the chain has to be followed all the
            // way up: one that breaks off (a peer that already exited, a process table that
            // can't be read) proves nothing, and is not taken for the user's.
            if peer::CAN_READ_PEER && !chain.contains(&1) {
                return Err("cannot tell where this process comes from".into());
            }
            Ok(Caller::User)
        }
    }
}

/// What `reply` should say about `line`, the request as the client wrote it, coming `from` the
/// process the kernel named.
#[cfg(test)]
fn answer(line: &str, from: Peer, tokens: &Tokens, agents: &Agents, our_uid: u32, send: &dyn Fn(Request) -> Result<(), String>) -> Reply {
    answer_stream(line, from, tokens, agents, our_uid, send, &|| true).0
}

/// [`answer`], and for a command that goes on talking (`events.subscribe`) the channel the
/// window sends the rest on, which ends when the window drops its end.
fn answer_stream(line: &str, from: Peer, tokens: &Tokens, agents: &Agents, our_uid: u32, send: &dyn Fn(Request) -> Result<(), String>, alive: &dyn Fn() -> bool) -> (Reply, Option<std::sync::mpsc::Receiver<Reply>>) {
    match ask(line, from, tokens, agents, our_uid, send, alive) {
        Ok((first, rest, streams)) => {
            let rest = (streams && first.is_ok()).then_some(rest);
            (first, rest)
        }
        Err(error) => (Err(error), None),
    }
}

/// Judges who is asking, hands the command to the window and waits for its first answer, with
/// whether the command goes on streaming after it.
fn ask(line: &str, from: Peer, tokens: &Tokens, agents: &Agents, our_uid: u32, send: &dyn Fn(Request) -> Result<(), String>, alive: &dyn Fn() -> bool) -> Result<(Reply, std::sync::mpsc::Receiver<Reply>, bool), String> {
    let request: Value = serde_json::from_str(line).map_err(|error| format!("not JSON: {error}"))?;
    // Who is asking, before what they want: a refusal says nothing about the command.
    let caller = classify(
        request["token"].as_str(),
        from,
        &tokens.lock().expect("tokens lock"),
        &agents.lock().expect("agents lock"),
        our_uid,
    )?;
    let command = Command::parse(&request)?;
    let streams = matches!(command, Command::EventsSubscribe);
    let (reply, answer) = std::sync::mpsc::channel();
    let wait = command.timeout();
    send(Request { caller, command, reply })?;
    // Each command says how long it may take; the client's read timeout is a little longer.
    // A margin past the command's own timeout, which the window starts counting a little later:
    // a wait that runs out answers `timed_out`, instead of being given up on here first.
    // `alive` is asked now and then, so a client that hangs up mid-wait stops holding its
    // connection (and, once the window drops the waiter, its share of waits).
    let end = std::time::Instant::now() + wait + std::time::Duration::from_secs(2);
    let first = loop {
        let slice = end.saturating_duration_since(std::time::Instant::now()).min(std::time::Duration::from_secs(1));
        match answer.recv_timeout(slice) {
            Ok(first) => break first,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if std::time::Instant::now() < end => {
                if !alive() {
                    return Err("the client hung up".into());
                }
            }
            Err(_) => return Err("Po did not answer".into()),
        }
    };
    Ok((first, answer, streams))
}

#[cfg(unix)]
mod server {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};

    use futures::channel::mpsc::{UnboundedSender, unbounded};
    use serde_json::json;

    use super::*;

    /// Requests handled at once; more are dropped unanswered.
    const MAX_CONNECTIONS: usize = 16;

    /// A directory only this user can enter, so the socket needs no permissions of its own.
    fn directory() -> std::path::PathBuf {
        let base = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) => std::path::PathBuf::from(dir),
            None => std::env::temp_dir().join(format!("po-{}", unsafe { libc::getuid() })),
        };
        base.join("po")
    }

    /// Every window's socket, as files: some may be left from a window that is gone.
    pub fn sockets() -> Vec<std::path::PathBuf> {
        let Ok(entries) = std::fs::read_dir(directory()) else { return vec![] };
        let mut sockets: Vec<_> = entries.filter_map(Result::ok).map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|ext| ext == "sock")).collect();
        sockets.sort();
        sockets
    }

    pub fn start() -> Option<(Control, UnboundedReceiver<Request>)> {
        let directory = directory();
        let made = std::fs::create_dir_all(&directory)
            .and_then(|()| std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)));
        if let Err(error) = made {
            log::warn!("control: {}: {error}", directory.display());
            return None;
        }
        // One window per process, but the tests start more than one server in a process: a
        // counter keeps their sockets apart.
        static SAID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let said = SAID.fetch_add(1, Ordering::SeqCst);
        let socket = directory.join(format!("{}-{said}.sock", std::process::id()));
        std::fs::remove_file(&socket).ok();
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) => {
                log::warn!("control: {}: {error}", socket.display());
                return None;
            }
        };
        let tokens = Tokens::default();
        let agents = Agents::default();
        let (requests, receiver) = unbounded();
        let (shared_tokens, shared_agents) = (tokens.clone(), agents.clone());
        std::thread::spawn(move || {
            let open = Arc::new(AtomicUsize::new(0));
            for stream in listener.incoming().map_while(Result::ok) {
                // A client that connects and says nothing holds a thread for the read timeout.
                if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    open.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let (tokens, agents, requests, open) = (shared_tokens.clone(), shared_agents.clone(), requests.clone(), open.clone());
                std::thread::spawn(move || {
                    serve(stream, &tokens, &agents, &requests);
                    open.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Some((Control { tokens, agents, socket: Arc::new(SocketFile(socket)) }, receiver))
    }

    fn serve(stream: std::os::unix::net::UnixStream, tokens: &Tokens, agents: &Agents, requests: &UnboundedSender<Request>) {
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
        let from = peer::of(&stream);
        let (reply, rest) = match read_request(&stream) {
            Ok(line) => answer_stream(
                &line,
                from,
                tokens,
                agents,
                unsafe { libc::getuid() as u32 },
                &|request| requests.unbounded_send(request).map_err(|_| "Po is closing".to_string()),
                &|| !hung_up(&stream),
            ),
            Err(error) => (Err(error), None),
        };
        let line = |reply: Reply| match reply {
            Ok(value) => json!({ "ok": value }),
            Err(error) => json!({ "error": error }),
        };
        if (&stream).write_all(format!("{}\n", line(reply)).as_bytes()).is_err() {
            return;
        }
        // A subscription goes on until the client hangs up, which a failed write finds out, or
        // the check between events does, so that a quiet window doesn't keep the slot.
        let Some(rest) = rest else { return };
        loop {
            match rest.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(next) => {
                    if (&stream).write_all(format!("{}\n", line(next)).as_bytes()).is_err() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !hung_up(&stream) => {}
                Err(_) => break,
            }
        }
    }

    /// One line, the request. A little over the biggest body is read (it is JSON-escaped), and
    /// all of it within a few seconds, so a client that drips bytes can't hold a slot.
    pub(super) fn read_request(stream: &std::os::unix::net::UnixStream) -> Result<String, String> {
        let limit = crate::command::MAX_BODY * 2 + 4096;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        loop {
            let buf = reader.fill_buf().map_err(|error| error.to_string())?;
            if buf.is_empty() {
                break;
            }
            let (take, done) = match buf.iter().position(|byte| *byte == b'\n') {
                Some(at) => (at + 1, true),
                None => (buf.len(), false),
            };
            line.extend_from_slice(&buf[..take]);
            reader.consume(take);
            if done {
                break;
            }
            if line.len() > limit {
                return Err("request too long".into());
            }
            if std::time::Instant::now() > deadline {
                return Err("request too slow".into());
            }
        }
        String::from_utf8(line).map_err(|_| "not UTF-8".to_string())
    }

    /// Whether the client has closed its end, found without reading anything.
    pub(super) fn hung_up(stream: &std::os::unix::net::UnixStream) -> bool {
        use std::os::unix::io::AsRawFd;
        let mut byte = 0u8;
        let got = unsafe { libc::recv(stream.as_raw_fd(), &mut byte as *mut u8 as *mut libc::c_void, 1, libc::MSG_PEEK | libc::MSG_DONTWAIT) };
        match got {
            0 => true,
            n if n > 0 => false,
            _ => std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock,
        }
    }

    /// Sends `request` and calls `each` with every line the window answers with, until it hangs
    /// up or `each` says stop.
    pub fn watch_at(socket: &str, request: &Value, mut each: impl FnMut(Value) -> bool) -> Result<()> {
        use anyhow::Context;
        let mut stream = std::os::unix::net::UnixStream::connect(socket).with_context(|| format!("could not reach Po at {socket}"))?;
        stream.write_all(format!("{request}\n").as_bytes())?;
        for line in BufReader::new(&stream).lines() {
            let value: Value = serde_json::from_str(&line?).context("Po sent something that is not JSON")?;
            if !each(value) {
                break;
            }
        }
        Ok(())
    }

    /// Sends one request to the window named by `PO_SOCKET` and returns its answer.
    pub fn call(request: &Value) -> Result<Value> {
        use anyhow::Context;
        let socket = std::env::var("PO_SOCKET").context("PO_SOCKET is not set: `po mcp` works when Po starts it for an agent")?;
        call_at(&socket, request)
    }

    pub fn call_at(socket: &str, request: &Value) -> Result<Value> {
        use anyhow::Context;
        let mut stream = std::os::unix::net::UnixStream::connect(socket).with_context(|| format!("could not reach Po at {socket}"))?;
        // The window's own timeout for the command, plus a little for the way there and back.
        let wait = Command::parse(request).map(|command| command.timeout() + std::time::Duration::from_secs(5)).unwrap_or(std::time::Duration::from_secs(15));
        stream.set_read_timeout(Some(wait)).ok();
        stream.write_all(format!("{request}\n").as_bytes())?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        serde_json::from_str(&line).context("Po sent something that is not JSON")
    }
}

#[cfg(unix)]
pub use server::{call, call_at, sockets, start, watch_at};

#[cfg(not(unix))]
fn call(_: &Value) -> Result<Value> {
    anyhow::bail!("Po's MCP server is not available on this platform yet")
}

#[cfg(not(unix))]
pub fn watch_at(_: &str, _: &Value, _: impl FnMut(Value) -> bool) -> Result<()> {
    anyhow::bail!("Po's control is not available on this platform yet")
}

#[cfg(not(unix))]
pub fn sockets() -> Vec<std::path::PathBuf> {
    vec![]
}

#[cfg(not(unix))]
pub fn call_at(_: &str, _: &Value) -> Result<Value> {
    anyhow::bail!("Po's control is not available on this platform yet")
}

#[cfg(not(unix))]
pub fn start() -> Option<(Control, UnboundedReceiver<Request>)> {
    None
}

/// Sends `request`, signed with this thread's `PO_TOKEN`, to the window that started it, and
/// returns what it answered or why it refused.
pub fn send(mut request: Value) -> Result<Value> {
    use anyhow::Context;
    request["token"] = std::env::var("PO_TOKEN").context("PO_TOKEN is not set")?.into();
    let mut reply = call(&request)?;
    match reply["error"].as_str() {
        Some(error) => anyhow::bail!("{error}"),
        None => Ok(reply["ok"].take()),
    }
}

/// Who is on the other end of a connected socket, and how they stand to it in the process tree.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod peer {
    use super::Peer;
    use std::os::unix::io::AsRawFd;

    /// This platform can read the process at the other end, so an unreadable one is refused.
    pub const CAN_READ_PEER: bool = true;

    pub fn of(stream: &std::os::unix::net::UnixStream) -> Peer {
        match of_pid(stream) {
            Some(pid) => Peer {
                pid: Some(pid),
                #[cfg(target_os = "macos")]
                uid: bsd(pid).and_then(|info| info.uid),
                #[cfg(not(target_os = "macos"))]
                uid: None,
            },
            None => Peer { pid: None, uid: None },
        }
    }

    /// The peer's process, from the socket itself: Linux tells the connection's credentials,
    /// macOS the peer's pid.
    #[cfg(target_os = "linux")]
    fn of_pid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
        // `SO_PEERCRED` is 17 on every architecture the uapi lists. libc has the constant only
        // behind newer interfaces, and no `ucred` for the gnu and musl targets at all.
        const SO_PEERCRED: libc::c_int = 17;
        #[repr(C)]
        struct Ucred {
            pid: libc::pid_t,
            uid: libc::uid_t,
            gid: libc::gid_t,
        }
        let mut cred = Ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = std::mem::size_of::<Ucred>() as libc::socklen_t;
        let ok = unsafe {
            libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, SO_PEERCRED, &mut cred as *mut Ucred as *mut libc::c_void, &mut len)
        };
        (ok == 0 && cred.pid > 0).then_some(cred.pid as u32)
    }

    /// The peer's process, from the socket: macOS says only the pid.
    #[cfg(target_os = "macos")]
    fn of_pid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
        let mut pid: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let ok = unsafe {
            libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::LOCAL_PEERPID, &mut pid as *mut _ as *mut libc::c_void, &mut len)
        };
        (ok == 0 && pid > 0).then_some(pid as u32)
    }

    /// The parent of `pid`. Linux reads it from `/proc`; macOS from the process table.
    pub fn parent(pid: u32) -> Option<u32> {
        bsd(pid).map(|info| info.ppid).filter(|ppid| *ppid > 0 && *ppid != pid)
    }

    /// What the process table says of `pid`: its parent, and its user where it can be read.
    #[cfg(target_os = "linux")]
    fn bsd(pid: u32) -> Option<Process> {
        // `/proc/<pid>/stat`: pid (command) state ppid … The command may hold spaces and
        // parentheses, so everything before the last `)` is skipped.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (_, after) = stat.rsplit_once(')')?;
        let mut fields = after.split_whitespace();
        let _state = fields.next()?;
        let ppid: u32 = fields.next()?.parse().ok()?;
        Some(Process { ppid })
    }

    #[cfg(target_os = "macos")]
    fn bsd(pid: u32) -> Option<Process> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let got = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int,
            )
        };
        (got as usize >= std::mem::size_of::<libc::proc_bsdinfo>()).then_some(Process { ppid: info.pbi_ppid, uid: Some(info.pbi_uid as u32) })
    }

    struct Process {
        ppid: u32,
        /// Read on macOS only, where the socket doesn't say whose user the peer is.
        #[cfg(target_os = "macos")]
        uid: Option<u32>,
    }
}

/// Where the process at the other end can't be read: the token alone says who is asking.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod peer {
    use super::Peer;

    pub const CAN_READ_PEER: bool = false;

    pub fn of(_: &std::os::unix::net::UnixStream) -> Peer {
        Peer { pid: None, uid: None }
    }

    pub fn parent(_: u32) -> Option<u32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered(agent: Option<u32>) -> HashMap<String, Registered> {
        HashMap::from([("good".to_string(), Registered { thread: 7, agent })])
    }

    fn agents(pids: &[u32]) -> HashSet<u32> {
        pids.iter().copied().collect()
    }

    fn peer(pid: Option<u32>, uid: Option<u32>) -> Peer {
        Peer { pid, uid }
    }

    const UID: u32 = 1000;

    #[test]
    fn a_request_needs_a_known_token() {
        let refuse = |_: Request| -> Result<(), String> { panic!("must not reach the window") };
        let tokens = Arc::new(Mutex::new(registered(None)));
        let agents = Arc::new(Mutex::new(agents(&[])));
        let none = peer(None, None);
        assert!(answer("not json", none, &tokens, &agents, UID, &refuse).is_err());
        assert!(answer(r#"{"token":"bad","command":"view.close","id":"v1"}"#, none, &tokens, &agents, UID, &refuse).is_err());
    }

    #[test]
    fn a_good_request_reaches_the_window_as_its_thread() {
        let send = |request: Request| -> Result<(), String> {
            assert_eq!(request.caller, Caller::Thread(7));
            assert_eq!(request.command, Command::ViewClose { id: "v1".into() });
            request.reply.send(Ok(serde_json::json!({ "id": "v1" }))).unwrap();
            Ok(())
        };
        let tokens = Arc::new(Mutex::new(registered(None)));
        let agents = Arc::new(Mutex::new(agents(&[])));
        let none = peer(None, None);
        assert_eq!(answer(r#"{"token":"good","command":"view.close","id":"v1"}"#, none, &tokens, &agents, UID, &send), Ok(serde_json::json!({ "id": "v1" })));
    }

    #[cfg(unix)]
    #[test]
    fn a_dropped_clone_leaves_the_socket_in_place() {
        let (control, _requests) = start().expect("a socket");
        let file = control.socket.0.clone();
        let clone = control.clone();
        drop(clone);
        assert!(file.exists(), "dropping a clone removed the socket");
        drop(control);
        assert!(!file.exists(), "the last one should remove it");
    }

    #[test]
    fn a_subscription_goes_on_after_its_first_answer() {
        let tokens = Tokens::default();
        let agents = Agents::default();
        let me = Peer { pid: Some(std::process::id()), uid: Some(UID) };
        // The window keeps the sender and says more later.
        let kept = std::sync::Mutex::new(None);
        let send = |request: Request| -> Result<(), String> {
            request.reply.send(Ok(serde_json::json!({ "subscribed": true }))).unwrap();
            *kept.lock().unwrap() = Some(request.reply);
            Ok(())
        };
        let (first, rest) = answer_stream(r#"{"command":"events.subscribe"}"#, me, &tokens, &agents, UID, &send, &|| true);
        assert_eq!(first, Ok(serde_json::json!({ "subscribed": true })));
        let rest = rest.expect("a stream");
        kept.lock().unwrap().as_ref().unwrap().send(Ok(serde_json::json!({ "event": "thread.state" }))).unwrap();
        assert_eq!(rest.recv().unwrap(), Ok(serde_json::json!({ "event": "thread.state" })));
        // A command that answers once has no stream.
        let once = |request: Request| -> Result<(), String> {
            request.reply.send(Ok(serde_json::json!({}))).unwrap();
            Ok(())
        };
        assert!(answer_stream(r#"{"command":"window.info"}"#, me, &tokens, &agents, UID, &once, &|| true).1.is_none());
        // Nor does a subscription the window refused.
        let refuse = |request: Request| -> Result<(), String> {
            request.reply.send(Err("no".into())).unwrap();
            Ok(())
        };
        assert!(answer_stream(r#"{"command":"events.subscribe"}"#, me, &tokens, &agents, UID, &refuse, &|| true).1.is_none());
    }

    #[test]
    fn a_wait_that_runs_out_answers_instead_of_being_given_up_on() {
        // A thread with no agent pid of its own, so its token alone says who it is.
        let tokens: Tokens = Arc::new(Mutex::new(registered(None)));
        let me = Peer { pid: Some(std::process::id()), uid: Some(UID) };
        // The wait's own timeout is 0 s; the window answers a second later, past it but within
        // the margin the server allows, since the window starts its timer a little after.
        let late = |request: Request| -> Result<(), String> {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1000));
                request.reply.send(Ok(serde_json::json!({ "timed_out": true }))).ok();
            });
            Ok(())
        };
        let reply = answer(r#"{"command":"thread.wait","timeout":0,"token":"good"}"#, me, &tokens, &Agents::default(), UID, &late);
        assert_eq!(reply, Ok(serde_json::json!({ "timed_out": true })));
    }

    #[test]
    fn a_client_that_hangs_up_mid_wait_is_noticed() {
        let tokens = Tokens::default();
        let agents = Agents::default();
        let me = Peer { pid: Some(std::process::id()), uid: Some(UID) };
        // The window takes the request and never answers; the client is gone.
        let keep = std::sync::Mutex::new(vec![]);
        let hold = |request: Request| -> Result<(), String> {
            keep.lock().unwrap().push(request.reply);
            Ok(())
        };
        let started = std::time::Instant::now();
        let (reply, rest) = answer_stream(r#"{"command":"events.subscribe"}"#, me, &tokens, &agents, UID, &hold, &|| false);
        assert_eq!(reply, Err("the client hung up".into()));
        assert!(rest.is_none() && started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn a_closed_socket_is_seen_and_a_dripping_client_is_not_waited_for() {
        use std::io::Write;
        let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(!server::hung_up(&server));
        client.write_all(b"{\"command\":\"window.info\"}\n").unwrap();
        assert_eq!(server::read_request(&server).unwrap().trim(), "{\"command\":\"window.info\"}");
        drop(client);
        assert!(server::hung_up(&server));
        // A request longer than any body is refused rather than read to the end.
        let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
        let writer = std::thread::spawn(move || {
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..16 {
                if client.write_all(&chunk).is_err() {
                    break;
                }
            }
        });
        assert_eq!(server::read_request(&server), Err("request too long".into()));
        drop(server);
        writer.join().unwrap();
    }

    #[test]
    fn a_token_works_only_from_the_agent_it_was_made_for() {
        // The token was made for an agent at pid 100; the connection claims to come from a
        // process whose chain (its own pid and its parents) does not reach it.
        let tokens = registered(Some(100));
        let agents = agents(&[100]);
        assert_eq!(
            classify(Some("good"), peer(Some(500), Some(UID)), &tokens, &agents, UID),
            Err("this token works only inside its own thread's agent".into())
        );
        // With no agent pid registered (a shared daemon, say), the token alone counts.
        let tokens = registered(None);
        assert_eq!(classify(Some("good"), peer(Some(500), Some(UID)), &tokens, &agents, UID), Ok(Caller::Thread(7)));
        // An unknown token is refused whatever the process says.
        assert!(classify(Some("other"), peer(Some(100), Some(UID)), &tokens, &agents, UID).is_err());
    }

    #[test]
    fn a_connection_without_a_token_is_the_users_unless_an_agent_sits_in_its_chain() {
        let tokens = registered(Some(100));
        // The peer itself counts as its own chain's start, so an agent pid of the peer's own
        // is refused; 0 is in no chain, so it stands for "no agent here".
        let no_agent = agents(&[0]);
        // 500 is a shell under init; 600 sits under the agent at 100, which is under init too.
        let table = tree(&[(500, 1), (600, 100), (100, 1)]);
        // A process of this user's, none of whose chain is an agent's: the user.
        assert_eq!(classify_with(None, peer(Some(500), Some(UID)), &tokens, &no_agent, UID, &table), Ok(Caller::User));
        // A process under an agent's is refused, whatever it claims.
        assert_eq!(classify_with(None, peer(Some(600), Some(UID)), &tokens, &agents(&[100]), UID, &table), Err("no control from inside an agent's process".into()));
        assert_eq!(classify_with(None, peer(Some(100), Some(UID)), &tokens, &agents(&[100]), UID, &table), Err("no control from inside an agent's process".into()));
        // Another user's connection is refused out of hand.
        assert!(classify_with(None, peer(Some(500), Some(UID + 1)), &tokens, &no_agent, UID, &table).is_err());
    }

    // A chain that can't be followed to init proves nothing about the peer: a client that wrote
    // its request and exited before its process was read, or one whose entry can't be read,
    // is not the user's by default.
    #[test]
    fn a_chain_that_breaks_off_is_not_taken_for_the_users() {
        let tokens = registered(Some(100));
        let none = agents(&[100]);
        let gone = tree(&[]);
        assert!(classify_with(None, peer(Some(500), Some(UID)), &tokens, &none, UID, &gone).is_err());
        // Broken halfway: 500's parent is 700, which is not in the table.
        let halfway = tree(&[(500, 700)]);
        assert!(classify_with(None, peer(Some(500), Some(UID)), &tokens, &none, UID, &halfway).is_err());
        // A peer that is init's own child reaches 1 and is judged by its chain as usual.
        let whole = tree(&[(500, 1)]);
        assert_eq!(classify_with(None, peer(Some(500), Some(UID)), &tokens, &none, UID, &whole), Ok(Caller::User));
    }

    #[test]
    fn a_peer_that_cannot_be_read_is_refused_where_reading_is_possible() {
        let tokens = registered(None);
        let agents = agents(&[]);
        let unreadable = peer(None, None);
        // A token still counts where there is no agent pid to check against.
        assert_eq!(classify(Some("good"), unreadable, &tokens, &agents, UID), Ok(Caller::Thread(7)));
        // Without one, there is nothing to go by.
        assert!(classify(None, unreadable, &tokens, &agents, UID).is_err());
        // With an agent pid to check against, neither works: the token can't prove its chain.
        let tokens = registered(Some(100));
        assert!(classify(Some("good"), unreadable, &tokens, &agents, UID).is_err());
    }

    fn tree(pairs: &[(u32, u32)]) -> impl Fn(u32) -> Option<u32> + use<> {
        let pairs = pairs.to_vec();
        move |pid| pairs.iter().find(|(child, _)| *child == pid).map(|(_, parent)| *parent)
    }

    #[test]
    fn an_ancestor_chain_is_capped() {
        assert!(peer(Some(1), None).chain_with(&|pid| Some(pid + 1)).len() <= 65);
    }

    #[cfg(unix)]
    #[test]
    fn a_request_goes_over_the_socket_and_back() {
        use futures::StreamExt;
        let (control, mut requests) = start().expect("socket");
        let env = control.register(5, None);
        let (socket, token) = (env[0].1.clone(), env[1].1.clone());
        let pump = std::thread::spawn(move || {
            let request = futures::executor::block_on(requests.next()).expect("a request");
            assert_eq!(request.caller, Caller::Thread(5));
            request.reply.send(Ok(serde_json::json!({ "id": "v9" }))).unwrap();
        });
        let reply = server::call_at(&socket, &serde_json::json!({ "token": token, "command": "view.close", "id": "v9" })).unwrap();
        assert_eq!(reply, serde_json::json!({ "ok": { "id": "v9" } }));
        pump.join().unwrap();
        let bad = server::call_at(&socket, &serde_json::json!({ "token": "nope", "command": "view.close", "id": "v9" })).unwrap();
        assert_eq!(bad["error"], "unknown token");
        use std::os::unix::fs::PermissionsExt;
        let directory = std::path::Path::new(&socket).parent().unwrap();
        assert_eq!(std::fs::metadata(directory).unwrap().permissions().mode() & 0o777, 0o700);
    }

    /// The child that connects for the chain test. Runs as a test in its own right, and only
    /// when the parent hands it the socket, token and an out file in its environment.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn control_chain_helper() {
        let (Ok(socket), Ok(token), Ok(out)) = (std::env::var("PO_TEST_SOCKET"), std::env::var("PO_TEST_TOKEN"), std::env::var("PO_TEST_OUT")) else { return };
        let reply = server::call_at(&socket, &serde_json::json!({ "token": token, "command": "view.close", "id": "v1" })).unwrap();
        std::fs::write(out, serde_json::to_string(&reply).unwrap()).unwrap();
    }

    // The token was made for the *test process*: a child of it, connecting to the socket, has
    // this process in its parent chain and is let in. A token made for an agent that is nobody
    // here is refused even from this process's own children.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_token_is_judged_by_the_process_chain() {
        use futures::StreamExt;
        use std::process::Command;
        let (control, mut requests) = start().expect("socket");
        let mine = control.register(5, Some(std::process::id()));
        let others = control.register(6, Some(2)); // not a pid in any chain of ours
        let (socket, mine, others) = (mine[0].1.clone(), mine[1].1.clone(), others[1].1.clone());
        let pump = std::thread::spawn(move || {
            // One call gets through: the token made for an agent nobody here runs is refused
            // by the server, before the window hears of it.
            let request = futures::executor::block_on(requests.next()).expect("a request");
            assert_eq!(request.caller, Caller::Thread(5));
            request.reply.send(Ok(serde_json::json!({ "id": "v1" }))).unwrap();
        });
        let run = |token: String, out: std::path::PathBuf| {
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["control_chain_helper", "--nocapture"])
                .env("PO_TEST_SOCKET", &socket)
                .env("PO_TEST_TOKEN", token)
                .env("PO_TEST_OUT", &out)
                .status()
                .expect("child");
            let answer = std::fs::read_to_string(&out).unwrap_or_else(|_| format!("{status} said nothing"));
            serde_json::from_str::<Value>(&answer).expect("the helper answered")
        };
        let scratch = std::env::temp_dir();
        // Through the chain: let in, and answered by the window as thread 5.
        assert_eq!(run(mine, scratch.join("po-chain-mine")), serde_json::json!({ "ok": { "id": "v1" } }));
        // Off the chain: refused by the server, without reaching the window.
        assert_eq!(run(others, scratch.join("po-chain-others"))["error"], "this token works only inside its own thread's agent");
        pump.join().unwrap();
    }

    #[test]
    fn registering_again_revokes_the_old_token() {
        let control = Control { tokens: Tokens::default(), agents: Agents::default(), socket: Arc::new(SocketFile(std::path::PathBuf::from("/nonexistent/po.sock"))) };
        let first = control.register(3, None)[1].1.clone();
        let second = control.register(3, None)[1].1.clone();
        let tokens = control.tokens.lock().unwrap();
        assert!(!tokens.contains_key(&first));
        assert_eq!(tokens.get(&second).map(|r| r.thread), Some(3));
    }

    #[test]
    fn registering_an_agent_marks_its_process() {
        let control = Control { tokens: Tokens::default(), agents: Agents::default(), socket: Arc::new(SocketFile(std::path::PathBuf::from("/nonexistent/po.sock"))) };
        control.register(3, Some(4242));
        assert!(control.agents.lock().unwrap().contains(&4242));
    }

    #[test]
    fn a_command_says_how_long_it_may_take() {
        for command in [Command::ViewerClose, Command::DiffShow { path: None }] {
            assert!(command.timeout() > std::time::Duration::ZERO);
        }
    }
}
