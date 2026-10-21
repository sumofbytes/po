//! How a process outside the window, an agent through `po ctl`, asks Po for something: a local
//! socket that takes one JSON request per connection. Each thread's agent is handed a token in its
//! environment, and the token is the only thing that says who is asking.
use std::collections::HashMap;
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

type Tokens = Arc<Mutex<HashMap<String, u64>>>;

pub struct Control {
    tokens: Tokens,
    socket: std::path::PathBuf,
}

impl Control {
    /// The environment that lets thread `id`'s agent reach this window. A thread asking again
    /// gets a new token, and the old one stops working.
    pub fn register(&self, id: u64) -> Vec<(String, String)> {
        let token = token();
        let mut tokens = self.tokens.lock().expect("tokens lock");
        tokens.retain(|_, thread| *thread != id);
        tokens.insert(token.clone(), id);
        vec![("PO_SOCKET".into(), self.socket.to_string_lossy().into_owned()), ("PO_TOKEN".into(), token)]
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        std::fs::remove_file(&self.socket).ok();
    }
}

fn token() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    // Without a random source there is no token worth handing out, so there is no socket.
    std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes)).expect("random token");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// What `reply` should say about `line`, the request as the client wrote it.
fn answer(line: &str, tokens: &Tokens, send: &dyn Fn(Request) -> Result<(), String>) -> Reply {
    let request: Value = serde_json::from_str(line).map_err(|error| format!("not JSON: {error}"))?;
    let token = request["token"].as_str().ok_or("no token")?;
    let thread = tokens.lock().expect("tokens lock").get(token).copied().ok_or("unknown token")?;
    let command = Command::parse(&request)?;
    let (reply, answer) = std::sync::mpsc::channel();
    send(Request { caller: Caller::Thread(thread), command, reply })?;
    answer.recv_timeout(std::time::Duration::from_secs(10)).map_err(|_| "Po did not answer".to_string())?
}

#[cfg(unix)]
mod server {
    use std::io::{BufRead, BufReader, Read, Write};
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

    pub fn start() -> Option<(Control, UnboundedReceiver<Request>)> {
        let directory = directory();
        let made = std::fs::create_dir_all(&directory)
            .and_then(|()| std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)));
        if let Err(error) = made {
            log::warn!("control: {}: {error}", directory.display());
            return None;
        }
        let socket = directory.join(format!("{}.sock", std::process::id()));
        std::fs::remove_file(&socket).ok();
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) => {
                log::warn!("control: {}: {error}", socket.display());
                return None;
            }
        };
        let tokens = Tokens::default();
        let (requests, receiver) = unbounded();
        let shared = tokens.clone();
        std::thread::spawn(move || {
            let open = Arc::new(AtomicUsize::new(0));
            for stream in listener.incoming().map_while(Result::ok) {
                // A client that connects and says nothing holds a thread for the read timeout.
                if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    open.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let (tokens, requests, open) = (shared.clone(), requests.clone(), open.clone());
                std::thread::spawn(move || {
                    serve(stream, &tokens, &requests);
                    open.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Some((Control { tokens, socket }, receiver))
    }

    fn serve(stream: std::os::unix::net::UnixStream, tokens: &Tokens, requests: &UnboundedSender<Request>) {
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
        let mut line = String::new();
        // A little over the biggest body, which is JSON-escaped.
        let read = BufReader::new((&stream).take(crate::command::MAX_BODY as u64 * 2 + 4096)).read_line(&mut line);
        let reply = match read {
            Ok(_) => answer(&line, tokens, &|request| requests.unbounded_send(request).map_err(|_| "Po is closing".to_string())),
            Err(error) => Err(error.to_string()),
        };
        let reply = match reply {
            Ok(value) => json!({ "ok": value }),
            Err(error) => json!({ "error": error }),
        };
        (&stream).write_all(format!("{reply}\n").as_bytes()).ok();
    }

    /// Sends one request to the window named by `PO_SOCKET` and returns its answer.
    pub fn call(request: &Value) -> Result<Value> {
        use anyhow::Context;
        let socket = std::env::var("PO_SOCKET").context("PO_SOCKET is not set: `po ctl` works from an agent that Po started")?;
        call_at(&socket, request)
    }

    pub(super) fn call_at(socket: &str, request: &Value) -> Result<Value> {
        use anyhow::Context;
        let mut stream = std::os::unix::net::UnixStream::connect(socket).with_context(|| format!("could not reach Po at {socket}"))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(15))).ok();
        stream.write_all(format!("{request}\n").as_bytes())?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        serde_json::from_str(&line).context("Po sent something that is not JSON")
    }
}

#[cfg(unix)]
pub use server::{call, start};

#[cfg(not(unix))]
fn call(_: &Value) -> Result<Value> {
    anyhow::bail!("po ctl is not available on this platform yet")
}

#[cfg(not(unix))]
pub fn start() -> Option<(Control, UnboundedReceiver<Request>)> {
    None
}

/// `po ctl <command> [--key value]…`: a command for the window that started this agent. The body
/// of `view.show` is read from standard input unless `--body` is given; `view.update` reads
/// standard input only for `--body -`, so changing a title alone never waits on a pipe.
pub fn ctl(args: &[String]) -> Result<String> {
    use anyhow::Context;
    let (name, flags) = args.split_first().context("usage: po ctl <command> [--key value]…, e.g. po ctl view.show --title Plan < plan.md")?;
    let mut request = serde_json::json!({ "command": name });
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        let key = flag.strip_prefix("--").with_context(|| format!("unexpected `{flag}`"))?;
        request[key] = flags.next().with_context(|| format!("--{key} needs a value"))?.clone().into();
    }
    if reads_stdin(name, &request["body"]) {
        use std::io::Read;
        let mut body = String::new();
        std::io::stdin().read_to_string(&mut body).context("could not read the body from standard input")?;
        request["body"] = body.into();
    }
    request["token"] = std::env::var("PO_TOKEN").context("PO_TOKEN is not set")?.into();
    let reply = call(&request)?;
    match reply["error"].as_str() {
        Some(error) => anyhow::bail!("{error}"),
        None => Ok(reply["ok"].to_string()),
    }
}

/// Whether `po ctl name` takes its body from standard input: a new view does unless given
/// one, and `--body -` asks for it on any command that has a body.
fn reads_stdin(name: &str, body: &Value) -> bool {
    match body.as_str() {
        Some("-") => name.starts_with("view.") && name != "view.close",
        Some(_) => false,
        None => name == "view.show",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_input_is_read_only_when_it_is_the_body() {
        let none = Value::Null;
        assert!(reads_stdin("view.show", &none));
        assert!(!reads_stdin("view.update", &none));
        assert!(!reads_stdin("view.close", &none));
        assert!(reads_stdin("view.update", &Value::from("-")));
        assert!(!reads_stdin("view.show", &Value::from("text")));
        assert!(!reads_stdin("layout.preset", &Value::from("-")));
    }

    fn tokens() -> Tokens {
        Arc::new(Mutex::new(HashMap::from([("good".to_string(), 7)])))
    }

    #[test]
    fn a_request_needs_a_known_token() {
        let refuse = |_: Request| -> Result<(), String> { panic!("must not reach the window") };
        assert!(answer("not json", &tokens(), &refuse).is_err());
        assert!(answer(r#"{"command":"view.close","id":"v1"}"#, &tokens(), &refuse).is_err());
        assert!(answer(r#"{"token":"bad","command":"view.close","id":"v1"}"#, &tokens(), &refuse).is_err());
    }

    #[test]
    fn a_good_request_reaches_the_window_as_its_thread() {
        let send = |request: Request| -> Result<(), String> {
            assert_eq!(request.caller, Caller::Thread(7));
            assert_eq!(request.command, Command::ViewClose { id: "v1".into() });
            request.reply.send(Ok(serde_json::json!({ "id": "v1" }))).unwrap();
            Ok(())
        };
        assert_eq!(answer(r#"{"token":"good","command":"view.close","id":"v1"}"#, &tokens(), &send), Ok(serde_json::json!({ "id": "v1" })));
    }

    #[cfg(unix)]
    #[test]
    fn a_request_goes_over_the_socket_and_back() {
        use futures::StreamExt;
        let (control, mut requests) = start().expect("socket");
        let env = control.register(5);
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

    #[test]
    fn registering_again_revokes_the_old_token() {
        let (control, _) = {
            let tokens = Tokens::default();
            (Control { tokens, socket: std::path::PathBuf::from("/nonexistent/po.sock") }, ())
        };
        let first = control.register(3)[1].1.clone();
        let second = control.register(3)[1].1.clone();
        let tokens = control.tokens.lock().unwrap();
        assert!(!tokens.contains_key(&first));
        assert_eq!(tokens.get(&second), Some(&3));
    }
}
