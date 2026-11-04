//! Desktop notifications through notify-send, so actions come back as plain text.

use std::path::Path;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub struct Note<'a> {
    pub summary: &'a str,
    pub body: &'a str,
    pub image: Option<&'a Path>,
    pub actions: &'a [(&'a str, &'a str)],
    pub critical: bool,
    pub timeout_ms: Option<u32>,
}

const ICON: &str = "qsbar";

/// Shows a notification. Returns its id once the server assigned one;
/// `on_action` runs with the action key if the user clicks a button.
pub async fn show<F>(note: Note<'_>, on_action: F) -> Option<u32>
where
    F: FnOnce(String) + Send + 'static,
{
    let mut cmd = Command::new("notify-send");
    cmd.arg("--app-name=Quick Share").arg(format!("--icon={ICON}")).arg("--print-id");
    if note.critical {
        cmd.arg("--urgency=critical");
    }
    if let Some(ms) = note.timeout_ms {
        cmd.arg(format!("--expire-time={ms}"));
    }
    if let Some(img) = note.image {
        cmd.arg(format!("--hint=string:image-path:{}", img.display()));
    }
    for (key, label) in note.actions {
        cmd.arg(format!("--action={key}={label}"));
    }
    if !note.actions.is_empty() {
        cmd.arg("--wait");
    }
    cmd.arg(note.summary).arg(note.body);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());

    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let mut lines = BufReader::new(stdout).lines();
    let id = lines.next_line().await.ok().flatten().and_then(|l| l.trim().parse::<u32>().ok());

    tokio::spawn(async move {
        let mut on_action = Some(on_action);
        while let Ok(Some(line)) = lines.next_line().await {
            let key = line.trim().to_string();
            if !key.is_empty() {
                if let Some(f) = on_action.take() {
                    f(key);
                }
            }
        }
        let _ = child.wait().await;
    });
    id
}

pub fn close(id: u32) {
    tokio::spawn(async move {
        let _ = Command::new("busctl")
            .args([
                "--user",
                "call",
                "org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
                "CloseNotification",
                "u",
            ])
            .arg(id.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    });
}
