//! Desktop notifications, for threads that need attention while the window isn't looking at them.
use std::process::{Command, Stdio};

/// Shows a notification, if the desktop has a way to. Failures are ignored: it is a courtesy.
pub fn send(summary: &str, body: &str) {
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("notify-send");
        command.args(["--app-name=po", "--", summary, body]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let escape = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
        let mut command = Command::new("osascript");
        command.args(["-e", &format!("display notification \"{}\" with title \"{}\"", escape(body), escape(summary))]);
        command
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        // Waited on a thread of its own, so the notifier doesn't linger as a zombie.
        std::thread::spawn(move || {
            command.status().ok();
        });
    }
}
