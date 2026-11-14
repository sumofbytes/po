#![recursion_limit = "1024"]
mod agent;
mod config;
mod dev;
mod files;
mod host;
mod ipc;
mod issues;
mod model;
mod reader;
mod rune_ui;
mod session;
mod system;
mod vcs;

use std::path::{Path, PathBuf};

use files::watch::Prefetch;

/// Relaunch ourselves in its own process group with output going to a log file,
/// so the shell gets its prompt back (like `code .`).
fn detach(root: &Path, command: &Option<String>, pr: &Option<vcs::pr::Pr>) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let log_dir = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
            } else {
                config::home().map(|home| home.join(".cache"))
            }
        })
        .unwrap_or_else(std::env::temp_dir)
        .join("po");
    std::fs::create_dir_all(&log_dir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("po.log"))?;
    let mut child = Command::new(std::env::current_exe()?);
    child.arg("--foreground");
    if let Some(command) = command {
        child.arg("--agent").arg(command);
    }
    if let Some(pr) = pr {
        // The parent did the checkout so its errors reach the terminal; the child owns the cleanup.
        child
            .arg("--pr-worktree")
            .arg(&pr.worktree)
            .arg("--pr-base")
            .arg(&pr.base)
            .arg("--pr-number")
            .arg(pr.number.to_string())
            .arg("--pr-title")
            .arg(&pr.title);
        if let Some(source) = pr.source() {
            child.arg("--pr-source").arg(source);
        }
    }
    child
        .arg(root)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut child, 0);
    #[cfg(windows)]
    {
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
        std::os::windows::process::CommandExt::creation_flags(
            &mut child,
            0x0000_0008 | 0x0000_0200,
        );
    }
    child.spawn()?;
    Ok(())
}

fn main() {
    // `po update` is a terminal command, not a window: it runs to completion and
    // exits before any GUI is set up (like `dex update`).
    let cli: Vec<String> = std::env::args().skip(1).collect();
    if cli.len() == 1 && cli[0] == "mcp" {
        if let Err(error) = ipc::mcp::serve() {
            eprintln!("po mcp: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    if cli.first().is_some_and(|arg| arg == "ctl") {
        if let Err(error) = ipc::ctl::run(&cli[1..]) {
            eprintln!("po ctl: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    if cli.len() == 1 && cli[0] == "update" {
        if std::path::Path::new("update").exists() {
            eprintln!(
                "po-update: running self-update (use `po ./update` to open the local path 'update')"
            );
        }
        match system::update::update() {
            Ok(message) => println!("{message}"),
            Err(message) => {
                eprintln!("po-update: {message}");
                std::process::exit(1);
            }
        }
        return;
    }
    dev::trace::init();
    // What Rune did not take (`--snap`, `--script` and its other flags are its own).
    let mut args = rune::extra_args().to_vec().into_iter();
    let mut path = None;
    let mut foreground = false;
    let mut command = std::env::var("PO_AGENT").ok();
    let mut spec = None;
    // Set only by a parent that already checked the PR out.
    let (mut worktree, mut base, mut number, mut title, mut source) =
        (None, None, 0, String::new(), None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--foreground" | "-f" => foreground = true,
            "--agent" => command = Some(args.next().expect("--agent needs a command")),
            "--pr" => spec = Some(args.next().expect("--pr needs a PR number or URL")),
            "--pr-worktree" => worktree = args.next().map(PathBuf::from),
            "--pr-base" => base = args.next(),
            "--pr-number" => number = args.next().and_then(|n| n.parse().ok()).unwrap_or(0),
            "--pr-title" => title = args.next().unwrap_or_default(),
            "--pr-source" => source = args.next().map(PathBuf::from),
            _ => path = Some(PathBuf::from(arg)),
        }
    }
    let root = path.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let root = root.canonicalize().expect("project path");
    let pr = match (worktree, base, spec) {
        (Some(worktree), Some(base), _) => {
            Some(vcs::pr::Pr::adopt(worktree, base, number, title, source))
        }
        (_, _, Some(spec)) => {
            eprintln!("po: checking out PR {spec}…");
            match vcs::pr::Pr::open(&root, &spec) {
                Ok(pr) => Some(pr),
                Err(err) => {
                    eprintln!("po: could not open PR {spec}: {err}");
                    std::process::exit(1);
                }
            }
        }
        _ => None,
    };
    if !foreground {
        match detach(&root, &command, &pr) {
            Ok(()) => {
                // The detached process owns the worktree now.
                std::mem::forget(pr);
                return;
            }
            Err(err) => eprintln!("po: could not detach ({err}), running in foreground"),
        }
    }
    dev::profile::start();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,po=info"))
        .format_timestamp_millis()
        .init();
    dev::trace::mark("args parsed");
    let prefetch = Prefetch::start(
        pr.as_ref()
            .map_or_else(|| root.clone(), |pr| pr.worktree.clone()),
    );
    host::app::run(root, pr, command, prefetch);
    // Where the event loop returns rather than ending the process; the app's quit already did it.
    dev::profile::finish();
}
