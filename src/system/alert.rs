//! Desktop notifications, for threads that need attention while the window isn't looking at them.
use std::process::{Command, Stdio};

/// Shows a notification, if the desktop has a way to. Failures are ignored: it is a courtesy.
/// `clicked` runs if the user clicks it, on a thread of its own, with the token that lets the
/// window take focus when the desktop gave one. Only Linux can tell: macOS's notifier can't.
pub fn send(summary: &str, body: &str, clicked: impl FnOnce(Option<String>) + Send + 'static) {
    #[cfg(target_os = "linux")]
    {
        let (summary, body) = (summary.to_string(), body.to_string());
        // Waited on a thread of its own: an action keeps the notifier running until it is answered.
        std::thread::spawn(move || {
            // The token goes to stderr so that it can't run into the action on stdout.
            let asked = Command::new("notify-send")
                .args([
                    "--app-name=po",
                    "--action=default=Open",
                    "--activation-token-fd=2",
                    "--",
                    &summary,
                    &body,
                ])
                .stdin(Stdio::null())
                .output();
            match asked {
                Ok(output) if output.status.success() => {
                    if let Some(token) = click(&output.stdout, &output.stderr) {
                        clicked(token);
                    }
                }
                // A notifier too old for actions refuses them, and has shown nothing.
                _ => {
                    let mut plain = Command::new("notify-send");
                    plain.args(["--app-name=po", "--", &summary, &body]);
                    quietly(plain);
                }
            }
        });
    }
    #[cfg(target_os = "macos")]
    {
        drop(clicked);
        let escape = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
        let mut command = Command::new("osascript");
        command.args([
            "-e",
            &format!(
                "display notification \"{}\" with title \"{}\"",
                escape(body),
                escape(summary)
            ),
        ]);
        // Waited on a thread of its own, so the notifier doesn't linger as a zombie.
        std::thread::spawn(move || quietly(command));
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = (summary, body, clicked);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn quietly(mut command: Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.status().ok();
}

/// What `notify-send` said: `None` unless the notification was clicked, else the activation token
/// if there was one. Its stderr may also hold its own messages, which have spaces in them.
#[cfg(any(target_os = "linux", test))]
fn click(stdout: &[u8], stderr: &[u8]) -> Option<Option<String>> {
    String::from_utf8_lossy(stdout)
        .lines()
        .any(|line| line.trim() == "default")
        .then(|| {
            let stderr = String::from_utf8_lossy(stderr);
            stderr
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.contains(char::is_whitespace))
                .map(str::to_string)
        })
}

#[cfg(test)]
mod tests {
    use super::click;

    #[test]
    fn a_notification_that_expired_was_not_clicked() {
        assert_eq!(click(b"", b"Wait timeout expired\n"), None);
    }

    #[test]
    fn a_click_without_a_token_still_counts() {
        assert_eq!(click(b"default\n", b""), Some(None));
    }

    #[test]
    fn the_token_is_told_from_the_notifiers_own_messages() {
        assert_eq!(
            click(b"default\n", b"Wait timeout expired\nabc-123\n"),
            Some(Some("abc-123".to_string()))
        );
    }
}
