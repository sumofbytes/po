//! `po ctl`: the commands of [`crate::command`] from a terminal, to a window that is running.
//! The window knows who is asking by the process, so there is no token to give: this is refused
//! from inside an agent Po started, and works from the user's own shell.
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// What the command line asked for.
#[derive(Debug, PartialEq)]
pub enum Task {
    /// `po ctl windows`
    Windows,
    /// `po ctl watch`: prints a line for every change of a thread's state.
    Watch { window: Option<u32> },
    /// `po ctl wait thread=7 [until=ready] [timeout=600]`: polls until the thread is there.
    Wait { window: Option<u32>, thread: u64, until: String, timeout: u64 },
    /// Any other command, as the request it makes.
    Send { window: Option<u32>, request: Value },
}

/// A value from `key=value`: a number or a boolean when it is one, text otherwise.
fn value(text: &str) -> Value {
    match text {
        "true" => json!(true),
        "false" => json!(false),
        _ => text.parse::<u64>().map_or_else(|_| json!(text), |number| json!(number)),
    }
}

/// Reads `po ctl`'s arguments (after `ctl`).
pub fn parse(args: &[String]) -> Result<Task> {
    let mut args = args.iter();
    let name = args.next().context("po ctl <command> [--window pid] [key=value …] [--json '{…}']")?;
    let mut window = None;
    let mut request = json!({});
    while let Some(arg) = args.next() {
        if arg == "--window" {
            window = Some(args.next().context("--window needs a pid")?.parse().context("--window needs a pid")?);
        } else if arg == "--json" {
            let given: Value = serde_json::from_str(args.next().context("--json needs an object")?).context("--json is not JSON")?;
            let Value::Object(given) = given else { bail!("--json needs an object") };
            for (key, given) in given {
                request[key] = given;
            }
        } else if let Some((key, text)) = arg.split_once('=') {
            request[key] = value(text);
        } else {
            bail!("`{arg}` is not key=value");
        }
    }
    if name == "windows" {
        return Ok(Task::Windows);
    }
    if name == "watch" {
        return Ok(Task::Watch { window });
    }
    if name == "wait" {
        return Ok(Task::Wait {
            window,
            thread: request["thread"].as_u64().context("po ctl wait needs thread=<id>")?,
            until: request["until"].as_str().unwrap_or("done").to_string(),
            timeout: request["timeout"].as_u64().unwrap_or(600),
        });
    }
    request["command"] = json!(name);
    Ok(Task::Send { window, request })
}

/// A window found by its socket.
pub struct Found {
    pub socket: PathBuf,
    pub info: Value,
}

/// The windows that answer, asking each its `window.info`. A socket that refuses the
/// connection belongs to a window that is gone, and is removed; the pid is not trusted for that,
/// since it can be reused.
pub fn windows() -> Vec<Found> {
    let mut found = vec![];
    for socket in crate::control::sockets() {
        let path = socket.to_string_lossy().into_owned();
        match crate::control::call_at(&path, &json!({ "command": "window.info" })) {
            Ok(mut reply) if reply["error"].is_null() => found.push(Found { socket, info: reply["ok"].take() }),
            Ok(_) => {}
            Err(error) => {
                let refused = error.root_cause().downcast_ref::<std::io::Error>().is_some_and(|error| matches!(error.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound));
                if refused {
                    std::fs::remove_file(&socket).ok();
                }
            }
        }
    }
    found
}

/// Which window to talk to: the one asked for, else the one with the working directory among
/// its projects, else the only one.
pub fn pick<'a>(windows: &'a [Found], window: Option<u32>, cwd: &std::path::Path) -> Result<&'a Found> {
    if let Some(pid) = window {
        return windows.iter().find(|found| found.info["pid"].as_u64() == Some(pid as u64)).with_context(|| format!("no Po window with pid {pid}; `po ctl windows` lists them"));
    }
    match windows {
        [] => bail!("no Po window is running"),
        [only] => Ok(only),
        many => {
            let here = |found: &&Found| {
                found.info["projects"].as_array().is_some_and(|projects| projects.iter().any(|project| project["root"].as_str().is_some_and(|root| cwd.starts_with(root))))
            };
            let mut here = many.iter().filter(here);
            match (here.next(), here.next()) {
                (Some(found), None) => Ok(found),
                _ => bail!("more than one window; pick one with --window <pid> (`po ctl windows`)"),
            }
        }
    }
}

fn ask(found: &Found, request: &Value) -> Result<Value> {
    let mut reply = crate::control::call_at(&found.socket.to_string_lossy(), request)?;
    match reply["error"].as_str() {
        Some(error) => bail!("{error}"),
        None => Ok(reply["ok"].take()),
    }
}

/// Runs `po ctl`; prints the reply as JSON. The exit code is the caller's to set from `Err`.
pub fn run(args: &[String]) -> Result<()> {
    let task = parse(args)?;
    let all = windows();
    let cwd = std::env::current_dir().unwrap_or_default();
    match task {
        Task::Windows => {
            let list: Vec<&Value> = all.iter().map(|found| &found.info).collect();
            println!("{}", serde_json::to_string_pretty(&list)?);
        }
        Task::Send { window, request } => {
            let found = pick(&all, window, &cwd)?;
            println!("{}", serde_json::to_string_pretty(&ask(found, &request)?)?);
        }
        Task::Watch { window } => {
            let found = pick(&all, window, &cwd)?;
            crate::control::watch_at(&found.socket.to_string_lossy(), &json!({ "command": "events.subscribe" }), |line| {
                if let Some(error) = line["error"].as_str() {
                    eprintln!("po ctl: {error}");
                    return false;
                }
                // The first line only says it has begun.
                if line["ok"]["event"].is_string() {
                    println!("{}", line["ok"]);
                }
                true
            })?;
        }
        Task::Wait { window, thread, until, timeout } => {
            let found = pick(&all, window, &cwd)?;
            let end = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
            loop {
                let info = match ask(found, &json!({ "command": "thread.get", "thread": thread })) {
                    Ok(info) => info,
                    // A closed thread is gone, which is what waiting for it to close is for.
                    Err(error) if error.to_string().contains("no such thread") && matches!(until.as_str(), "closed" | "done") => {
                        println!("{}", json!({ "thread": thread, "state": "closed" }));
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                };
                if reached(info["state"].as_str().unwrap_or_default(), &until) {
                    println!("{}", serde_json::to_string_pretty(&info)?);
                    return Ok(());
                }
                if std::time::Instant::now() >= end {
                    bail!("timed out waiting for thread {thread} ({})", info["state"]);
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }
    Ok(())
}

/// Whether a thread in `state` has reached `until`: a state's name, or `done` for any state a
/// turn ends in or a thread stops in.
fn reached(state: &str, until: &str) -> bool {
    match until {
        "done" => matches!(state, "ready" | "asking" | "approval" | "failed" | "asleep" | "closed"),
        until => state == until,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn arguments_become_a_request() {
        let Task::Send { window, request } = parse(&args(&["thread.send", "--window", "42", "thread=7", "text=go on", "later=true"])).unwrap() else { panic!() };
        assert_eq!(window, Some(42));
        assert_eq!(request, json!({ "command": "thread.send", "thread": 7, "text": "go on", "later": true }));
        let Task::Send { request, .. } = parse(&args(&["thread.create", "--json", r#"{"prompt":"x","options":{"model":"fast"}}"#])).unwrap() else { panic!() };
        assert_eq!(request["options"]["model"], "fast");
        assert_eq!(parse(&args(&["windows"])).unwrap(), Task::Windows);
        assert_eq!(parse(&args(&["watch", "--window", "3"])).unwrap(), Task::Watch { window: Some(3) });
        assert_eq!(parse(&args(&["wait", "thread=3"])).unwrap(), Task::Wait { window: None, thread: 3, until: "done".into(), timeout: 600 });
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["thread.get", "seven"])).is_err());
        assert!(parse(&args(&["wait"])).is_err());
        assert!(parse(&args(&["thread.get", "--json", "[1]"])).is_err());
    }

    #[test]
    fn a_window_is_picked_by_pid_cwd_or_being_alone() {
        let window = |pid: u64, root: &str| Found { socket: PathBuf::from(format!("/s/{pid}.sock")), info: json!({ "pid": pid, "projects": [{ "root": root }] }) };
        let two = [window(1, "/work/a"), window(2, "/work/b")];
        assert_eq!(pick(&two, Some(2), std::path::Path::new("/")).unwrap().info["pid"], 2);
        assert!(pick(&two, Some(9), std::path::Path::new("/")).is_err());
        assert_eq!(pick(&two, None, std::path::Path::new("/work/b/src")).unwrap().info["pid"], 2);
        assert!(pick(&two, None, std::path::Path::new("/elsewhere")).is_err());
        assert_eq!(pick(&two[..1], None, std::path::Path::new("/elsewhere")).unwrap().info["pid"], 1);
        assert!(pick(&[], None, std::path::Path::new("/")).is_err());
    }

    #[test]
    fn done_is_any_state_a_turn_ends_in() {
        assert!(reached("ready", "done") && reached("failed", "done") && reached("asking", "done"));
        assert!(!reached("running", "done") && !reached("starting", "done"));
        assert!(reached("running", "running") && !reached("ready", "running"));
    }
}
