//! End-to-end test for `po update` against a local stub release server.
//!
//! The stub speaks just enough of GitHub's release layout: `/releases/latest`
//! redirects to the tag page and `/releases/download/<tag>/<asset>` serves the
//! archive and `SHA256SUMS`. The real `po` binary is copied out of `target/`
//! (update refuses dev builds) and pointed at the stub with `PO_BASE_URL`.
#![cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]

use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
    thread,
};

const TARGET: &str = if cfg!(target_os = "linux") {
    "x86_64-unknown-linux-gnu"
} else {
    "aarch64-apple-darwin"
};

/// Marker the fake "new" binary prints, to prove the exe was replaced.
const NEW_BINARY_OUTPUT: &str = "fake po v99.0.0";

struct Release {
    tag: &'static str,
    archive: Vec<u8>,
    sums: String,
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("po-update-e2e-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(cmd: &mut Command) -> Output {
    let out = cmd.output().expect("spawn");
    assert!(out.status.success(), "{cmd:?} failed: {out:?}");
    out
}

/// Build a release archive laid out like release.yml's: `po-<tag>-<target>/po`.
fn make_release(tag: &'static str, corrupt_sums: bool) -> Release {
    let dir = scratch(&format!("build-{tag}-{corrupt_sums}"));
    let name = format!("po-{tag}-{TARGET}");
    fs::create_dir_all(dir.join(&name)).unwrap();
    let bin = dir.join(&name).join("po");
    fs::write(&bin, format!("#!/bin/sh\necho '{NEW_BINARY_OUTPUT}'\n")).unwrap();
    let asset = format!("{name}.tar.gz");
    run(Command::new("tar").args(["-czf", &asset, &name]).current_dir(&dir));
    let archive = fs::read(dir.join(&asset)).unwrap();
    let mut hash = if cfg!(target_os = "linux") {
        let mut c = Command::new("sha256sum");
        c.arg(&asset);
        c
    } else {
        let mut c = Command::new("shasum");
        c.args(["-a", "256", &asset]);
        c
    };
    let line = String::from_utf8(run(hash.current_dir(&dir)).stdout).unwrap();
    let sums = if corrupt_sums {
        line.replacen(|c: char| c.is_ascii_hexdigit(), "x", 1)
    } else {
        line
    };
    let _ = fs::remove_dir_all(&dir);
    Release { tag, archive, sums }
}

/// Serve the release on 127.0.0.1 until the process exits; returns the base URL.
fn serve(release: Release) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let release = Arc::new(release);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let release = release.clone();
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                let tag = release.tag;
                let archive_path = format!("/releases/download/{tag}/po-{tag}-{TARGET}.tar.gz");
                let sums_path = format!("/releases/download/{tag}/SHA256SUMS");
                let (status, extra, body): (&str, String, Vec<u8>) = if path == "/releases/latest" {
                    ("302 Found", format!("Location: /releases/tag/{tag}\r\n"), vec![])
                } else if path == format!("/releases/tag/{tag}") {
                    ("200 OK", String::new(), b"release page".to_vec())
                } else if path == archive_path {
                    ("200 OK", String::new(), release.archive.clone())
                } else if path == sums_path {
                    ("200 OK", String::new(), release.sums.clone().into_bytes())
                } else {
                    ("404 Not Found", String::new(), vec![])
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            });
        }
    });
    base
}

/// Copy the built `po` to a scratch dir and run `po update` from there.
fn run_update(dir: &Path, base: &str, extra_env: &[(&str, &str)]) -> (PathBuf, Output) {
    let exe = dir.join("po");
    fs::copy(env!("CARGO_BIN_EXE_po"), &exe).unwrap();
    let mut cmd = Command::new(&exe);
    cmd.arg("update")
        .env("PO_BASE_URL", base)
        .env_remove("PO_VERSION")
        .env("CARGO_HOME", dir.join("no-cargo"));
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn po update");
    (exe, out)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn updates_the_running_binary_from_the_latest_release() {
    let dir = scratch("ok");
    let base = serve(make_release("v99.0.0", false));
    let (exe, out) = run_update(&dir, &base, &[]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stdout).contains("-> v99.0.0"), "stdout: {}", text(&out.stdout));

    let replaced = run(&mut Command::new(&exe));
    assert_eq!(text(&replaced.stdout).trim(), NEW_BINARY_OUTPUT);
    // No staged leftovers next to the exe.
    let leftovers: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".po.new"))
        .collect();
    assert!(leftovers.is_empty(), "staged files left behind: {leftovers:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn reports_up_to_date_when_latest_is_not_newer() {
    let dir = scratch("uptodate");
    let base = serve(make_release("v0.0.1", false));
    let (exe, out) = run_update(&dir, &base, &[]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stdout).contains("up to date"), "stdout: {}", text(&out.stdout));
    assert_eq!(fs::read(&exe).unwrap(), fs::read(env!("CARGO_BIN_EXE_po")).unwrap());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn checksum_mismatch_fails_and_leaves_the_binary_untouched() {
    let dir = scratch("badsum");
    let base = serve(make_release("v99.0.0", true));
    let (exe, out) = run_update(&dir, &base, &[]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("checksum mismatch"), "stderr: {}", text(&out.stderr));
    assert_eq!(fs::read(&exe).unwrap(), fs::read(env!("CARGO_BIN_EXE_po")).unwrap());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pinned_version_installs_even_when_not_newer() {
    let dir = scratch("pinned");
    let base = serve(make_release("v0.0.1", false));
    let (exe, out) = run_update(&dir, &base, &[("PO_VERSION", "0.0.1")]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let replaced = run(&mut Command::new(&exe));
    assert_eq!(text(&replaced.stdout).trim(), NEW_BINARY_OUTPUT);
    let _ = fs::remove_dir_all(&dir);
}
