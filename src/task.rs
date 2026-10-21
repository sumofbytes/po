//! Running a project's own checks (tests, build, lint) for an agent and giving back what failed,
//! not a log. Only commands Po recognises from the project's files are run: a caller names a
//! kind of check, never a command line.
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long a check may run before it is stopped.
pub const LIMIT: Duration = Duration::from_secs(15 * 60);

/// Lines of output kept from the end of a run.
const TAIL_LINES: usize = 60;
const TAIL_BYTES: usize = 8 * 1024;

/// A check Po knows how to run in a project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub ecosystem: &'static str,
    pub program: &'static str,
    pub args: &'static [&'static str],
}

impl Spec {
    /// The command as a person would type it.
    pub fn label(&self) -> String {
        std::iter::once(self.program).chain(self.args.iter().copied()).collect::<Vec<_>>().join(" ")
    }
}

/// The kinds of check a caller may ask for.
pub const KINDS: [&str; 3] = ["test", "build", "lint"];

/// A project file that identifies an ecosystem, its name, and its test, build and lint commands.
type Ecosystem = (&'static str, &'static str, [(&'static str, &'static str, &'static [&'static str]); 3]);

/// The check `kind` for the project at `root`, from the files that say what it is.
pub fn detect(root: &Path, kind: &str) -> Result<Spec, String> {
    let table: [Ecosystem; 3] = [
        ("Cargo.toml", "cargo", [("test", "cargo", &["test"]), ("build", "cargo", &["build"]), ("lint", "cargo", &["clippy"])]),
        ("package.json", "npm", [("test", "npm", &["test"]), ("build", "npm", &["run", "build"]), ("lint", "npm", &["run", "lint"])]),
        ("go.mod", "go", [("test", "go", &["test", "./..."]), ("build", "go", &["build", "./..."]), ("lint", "go", &["vet", "./..."])]),
    ];
    if !KINDS.contains(&kind) {
        return Err(format!("no check `{kind}`: {}", KINDS.join(", ")));
    }
    for (file, ecosystem, checks) in table {
        if root.join(file).is_file() {
            let (_, program, args) = checks.iter().find(|(name, ..)| *name == kind).expect("every kind is listed");
            return Ok(Spec { ecosystem, program, args });
        }
    }
    Err("no Cargo.toml, package.json or go.mod here, so Po does not know how to run a check".into())
}

/// One thing that failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub name: String,
    pub location: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub command: String,
    /// None if it was stopped or could not start.
    pub exit: Option<i32>,
    pub seconds: f64,
    pub failures: Vec<Failure>,
    pub summary: Option<String>,
    pub tail: String,
    /// Why there is no exit status: it did not start, or ran out of time.
    pub problem: Option<String>,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.exit == Some(0)
    }

    pub fn to_json(&self, commit: &str) -> Value {
        json!({
            "command": self.command,
            "passed": self.passed(),
            "exit": self.exit,
            "seconds": (self.seconds * 10.0).round() / 10.0,
            "commit": commit,
            "failures": self.failures.iter().map(|f| json!({ "name": f.name, "location": f.location, "message": f.message })).collect::<Vec<_>>(),
            "summary": self.summary,
            "problem": self.problem,
            "output_tail": self.tail,
        })
    }

    /// The result as a document for the viewer.
    pub fn to_markdown(&self, commit: &str) -> String {
        let verdict = match (&self.problem, self.exit) {
            (Some(_), _) => "did not finish".to_string(),
            (None, Some(0)) => "passed".to_string(),
            (None, _) if self.failures.is_empty() => "failed".to_string(),
            (None, _) => format!("failed ({} found)", self.failures.len()),
        };
        let mut out = format!("# {} — {verdict}\n\n", self.command);
        out.push_str(&format!("Ran in Po in {:.1}s", self.seconds));
        if let Some(code) = self.exit {
            out.push_str(&format!(", exit {code}"));
        }
        out.push_str(&format!(", on {commit}.\n\n"));
        if let Some(problem) = &self.problem {
            out.push_str(&format!("{problem}\n\n"));
        }
        if let Some(summary) = &self.summary {
            out.push_str(&format!("{summary}\n\n"));
        }
        for failure in &self.failures {
            out.push_str(&format!("- `{}`", failure.name));
            if let Some(location) = &failure.location {
                out.push_str(&format!(" at `{location}`"));
            }
            if !failure.message.is_empty() {
                out.push_str(&format!(": {}", failure.message));
            }
            out.push('\n');
        }
        if !self.passed() && !self.tail.is_empty() {
            out.push_str(&format!("\n## End of the output\n\n```\n{}\n```\n", self.tail.trim_end()));
        }
        out
    }
}

/// Runs `spec` in `root` and waits for it, up to `limit`.
pub fn run(root: &Path, spec: &Spec, limit: Duration) -> Outcome {
    let started = Instant::now();
    let command = spec.label();
    let mut outcome = Outcome { command, exit: None, seconds: 0.0, failures: vec![], summary: None, tail: String::new(), problem: None };
    let child = Command::new(spec.program)
        .args(spec.args)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .env("CARGO_TERM_COLOR", "never")
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let child = match child {
        Ok(child) => child,
        Err(error) => {
            outcome.problem = Some(format!("could not start `{}`: {error}", spec.program));
            return outcome;
        }
    };
    let pid = child.id();
    let (done, wait) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        done.send(child.wait_with_output()).ok();
    });
    let output = match wait.recv_timeout(limit) {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            outcome.problem = Some(format!("could not read the output: {error}"));
            return outcome;
        }
        Err(_) => {
            stop(pid);
            outcome.seconds = started.elapsed().as_secs_f64();
            outcome.problem = Some(format!("stopped after {} minutes", limit.as_secs() / 60));
            return outcome;
        }
    };
    outcome.seconds = started.elapsed().as_secs_f64();
    outcome.exit = output.status.code();
    if outcome.exit.is_none() {
        outcome.problem = Some("ended by a signal".into());
    }
    // The compiler's messages and the test runner's results come on different streams.
    let text = format!("{}\n{}", String::from_utf8_lossy(&output.stderr), String::from_utf8_lossy(&output.stdout));
    outcome.failures = match spec.ecosystem {
        "cargo" => cargo_failures(&text),
        "go" => go_failures(&text),
        _ => vec![],
    };
    outcome.summary = (spec.ecosystem == "cargo").then(|| cargo_summary(&text)).flatten();
    outcome.tail = tail(&text);
    outcome
}

#[cfg(unix)]
fn stop(pid: u32) {
    // SAFETY: signalling a process we started and still hold the pid of.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn stop(_: u32) {}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    let mut tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    if tail.len() > TAIL_BYTES {
        let mut start = tail.len() - TAIL_BYTES;
        while !tail.is_char_boundary(start) {
            start += 1;
        }
        tail = tail[start..].to_string();
    }
    tail
}

/// Failed tests and compile errors from `cargo test`, `cargo build` or `cargo clippy`.
fn cargo_failures(text: &str) -> Vec<Failure> {
    let lines: Vec<&str> = text.lines().collect();
    let mut failures: Vec<Failure> = vec![];
    for (ix, line) in lines.iter().enumerate() {
        if let Some(name) = line.strip_prefix("test ").and_then(|rest| rest.strip_suffix(" ... FAILED")) {
            let (location, message) = panic_of(&lines, name);
            failures.push(Failure { name: name.to_string(), location, message });
        } else if (line.starts_with("error[") || line.starts_with("error:")) && !line.starts_with("error: could not compile") && !line.starts_with("error: test failed") {
            let location = lines.get(ix + 1).and_then(|next| next.trim_start().strip_prefix("--> ")).map(String::from);
            failures.push(Failure { name: "compile error".into(), location, message: line.trim_start_matches("error").trim_start_matches(|c| c != ':').trim_start_matches(':').trim().to_string() });
        }
    }
    failures
}

/// Where test `name` panicked and what it said, from the `---- name stdout ----` section.
fn panic_of(lines: &[&str], name: &str) -> (Option<String>, String) {
    let header = format!("---- {name} stdout ----");
    let Some(at) = lines.iter().position(|line| *line == header) else { return (None, String::new()) };
    for (ix, line) in lines.iter().enumerate().skip(at + 1) {
        if line.starts_with("---- ") {
            break;
        }
        if let Some((_, rest)) = line.split_once("panicked at ") {
            let location = rest.trim_end_matches(':').to_string();
            let message = lines.get(ix + 1).map(|next| next.trim().to_string()).unwrap_or_default();
            return (Some(location), message);
        }
    }
    (None, String::new())
}

fn cargo_summary(text: &str) -> Option<String> {
    let results: Vec<&str> = text.lines().filter(|line| line.starts_with("test result:")).collect();
    (!results.is_empty()).then(|| results.join("\n\n"))
}

/// Failed tests from `go test`: a `--- FAIL:` line and the `file.go:12: message` under it.
fn go_failures(text: &str) -> Vec<Failure> {
    let lines: Vec<&str> = text.lines().collect();
    let mut failures = vec![];
    for (ix, line) in lines.iter().enumerate() {
        let Some(rest) = line.trim_start().strip_prefix("--- FAIL: ") else { continue };
        let name = rest.split_whitespace().next().unwrap_or(rest).to_string();
        let detail = lines[ix + 1..]
            .iter()
            .take_while(|next| next.starts_with(' ') || next.starts_with('\t'))
            .find_map(|next| {
                let next = next.trim();
                let (location, message) = next.split_once(": ")?;
                let (file, line) = location.rsplit_once(':')?;
                (file.ends_with(".go") && line.chars().all(|c| c.is_ascii_digit())).then(|| (location.to_string(), message.to_string()))
            });
        failures.push(match detail {
            Some((location, message)) => Failure { name, location: Some(location), message },
            None => Failure { name, location: None, message: String::new() },
        });
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO_FAIL: &str = "\
running 2 tests
test tests::adds ... ok
test tests::splits ... FAILED

failures:

---- tests::splits stdout ----

thread 'tests::splits' panicked at src/lib.rs:12:9:
assertion `left == right` failed
  left: 1
 right: 2

failures:
    tests::splits

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
";

    #[test]
    fn cargo_test_failures_come_with_where_and_why() {
        let failures = cargo_failures(CARGO_FAIL);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].name, "tests::splits");
        assert_eq!(failures[0].location.as_deref(), Some("src/lib.rs:12:9"));
        assert_eq!(failures[0].message, "assertion `left == right` failed");
        assert!(cargo_summary(CARGO_FAIL).unwrap().contains("1 failed"));
    }

    #[test]
    fn cargo_compile_errors_are_failures() {
        let text = "error[E0425]: cannot find value `x` in this scope\n --> src/main.rs:3:5\n  |\n3 |     x\n\nerror: could not compile `demo` (bin \"demo\") due to 1 previous error\n";
        let failures = cargo_failures(text);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].location.as_deref(), Some("src/main.rs:3:5"));
        assert!(failures[0].message.contains("cannot find value"));
    }

    #[test]
    fn go_test_failures_come_with_where_and_why() {
        let text = "=== RUN   TestSplit\n--- FAIL: TestSplit (0.00s)\n    split_test.go:14: got 1, want 2\nFAIL\nFAIL\tdemo\t0.002s\n";
        assert_eq!(
            go_failures(text),
            vec![Failure { name: "TestSplit".into(), location: Some("split_test.go:14".into()), message: "got 1, want 2".into() }]
        );
    }

    #[test]
    fn a_check_is_found_from_the_project_files() {
        let dir = std::env::temp_dir().join(format!("po-task-detect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(detect(&dir, "test").is_err());
        std::fs::write(dir.join("go.mod"), "module demo\n").unwrap();
        assert_eq!(detect(&dir, "test").unwrap().label(), "go test ./...");
        assert_eq!(detect(&dir, "lint").unwrap().label(), "go vet ./...");
        assert!(detect(&dir, "deploy").is_err());
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        assert_eq!(detect(&dir, "build").unwrap().label(), "cargo build");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_run_reports_the_exit_status_and_the_end_of_the_output() {
        let spec = Spec { ecosystem: "other", program: "sh", args: &["-c", "echo out; echo err >&2; exit 3"] };
        let outcome = run(Path::new("."), &spec, Duration::from_secs(10));
        assert_eq!(outcome.exit, Some(3));
        assert!(!outcome.passed());
        assert!(outcome.tail.contains("out") && outcome.tail.contains("err"));
        assert!(outcome.to_markdown("abc123").contains("exit 3"));
        let ok = run(Path::new("."), &Spec { ecosystem: "other", program: "true", args: &[] }, Duration::from_secs(10));
        assert!(ok.passed());
    }

    #[test]
    fn a_run_that_takes_too_long_is_stopped() {
        let spec = Spec { ecosystem: "other", program: "sleep", args: &["30"] };
        let outcome = run(Path::new("."), &spec, Duration::from_millis(200));
        assert_eq!(outcome.exit, None);
        assert!(outcome.problem.is_some());
        assert!(!outcome.passed());
    }

    #[test]
    fn a_missing_program_is_a_problem_not_a_crash() {
        let spec = Spec { ecosystem: "other", program: "po-no-such-program", args: &[] };
        let outcome = run(Path::new("."), &spec, Duration::from_secs(1));
        assert!(outcome.problem.unwrap().contains("could not start"));
    }
}
