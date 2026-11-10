//! The receiver: runs rqs_lib with no window and serves state to the bar.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rqs_lib::channel::{ChannelAction, ChannelDirection, ChannelMessage, TransferType};
use rqs_lib::{State, Visibility, RQS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, watch};

use crate::{clip, notify, paths};

const HISTORY_LIMIT: usize = 40;
const NEARBY_SECS: u64 = 30;
/// Copied transfers above this size go to disk cache instead of RAM.
const RAM_LIMIT: u64 = 300 * 1024 * 1024;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Copy,
    Save,
}

impl Action {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "copy" => Some(Action::Copy),
            "save" => Some(Action::Save),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    /// image | file | url | text | wifi
    pub kind: String,
    pub name: String,
    pub path: Option<String>,
    pub text: Option<String>,
    pub size: u64,
    pub device: String,
    pub time: u64,
    pub saved: bool,
}

#[derive(Serialize, Deserialize)]
struct Persist {
    #[serde(default = "yes")]
    visible: bool,
    #[serde(default)]
    trusted: Vec<String>,
    #[serde(default)]
    history: Vec<Item>,
}

fn yes() -> bool {
    true
}

impl Default for Persist {
    fn default() -> Self {
        Persist { visible: true, trusted: vec![], history: vec![] }
    }
}

#[derive(Clone, Debug, Serialize)]
struct Incoming {
    id: String,
    device: String,
    pin: Option<String>,
    /// waiting | receiving | finishing
    state: String,
    /// images | files | text
    kind: String,
    files: Vec<String>,
    text: Option<String>,
    total_bytes: u64,
    ack_bytes: u64,
    action: Option<Action>,
    auto: bool,
    started: u64,
    #[serde(skip)]
    notif: Option<u32>,
}

struct Inner {
    persist: Persist,
    visible_until: Option<u64>,
    temp_gen: u64,
    nearby_until: Option<u64>,
    last_nearby_note: u64,
    incoming: Vec<Incoming>,
    status: Option<String>,
    /// The status is a problem, not a confirmation (the bar shows it in red).
    status_bad: bool,
    /// When progress was last sent to the bar (it gets at most ~5 a second).
    last_progress: u64,
}

pub struct Core {
    lib_tx: broadcast::Sender<ChannelMessage>,
    visibility: Arc<Mutex<watch::Sender<Visibility>>>,
    inner: Mutex<Inner>,
    /// One publish at a time; holds what was last written to state.json.
    saved: Mutex<String>,
    snapshot: watch::Sender<String>,
    device_name: String,
}

fn kind_of(files: &[String]) -> &'static str {
    if files.is_empty() {
        "text"
    } else if files.iter().all(|f| clip::is_image(f)) {
        "images"
    } else {
        "files"
    }
}

fn default_action(kind: &str) -> Action {
    if kind == "files" {
        Action::Save
    } else {
        Action::Copy
    }
}

/// Picks a name in `dir` that does not exist yet: a.jpg, a (1).jpg, ...
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| name.into());
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .unwrap()
}

/// rename, falling back to copy+delete across filesystems.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // Copy next to the destination first: a copy that fails halfway (disk full)
    // must not leave a cut-off file under the real name.
    let name = to.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let part = to.with_file_name(format!(".{name}.part"));
    let done = std::fs::copy(from, &part).and_then(|_| std::fs::rename(&part, to));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&part);
        return Err(e).with_context(|| format!("copy {from:?} -> {to:?}"));
    }
    let _ = std::fs::remove_file(from);
    Ok(())
}

/// rqs_lib names a clash "1_name"; find the newest candidate in staging.
fn find_staged(name: &str) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for dir in [paths::staging_dir(), paths::big_staging_dir()] {
        find_staged_in(&dir, name, &mut best);
    }
    best.map(|(_, p)| p)
}

/// Deletes what has arrived so far of a transfer that will not finish.
fn remove_staged(files: &[String]) {
    for name in files {
        if let Some(p) = find_staged(name) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// A received file name must be a plain name: no folders, no "..", not absolute.
/// The library joins it onto the staging folder as it is.
fn is_plain_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!((parts.next(), parts.next()), (Some(std::path::Component::Normal(_)), None))
        && !name.contains(['/', '\\', '\0'])
}

fn find_staged_in(dir: &Path, name: &str, best: &mut Option<(SystemTime, PathBuf)>) {
    let mut consider = |p: PathBuf| {
        if let Ok(meta) = std::fs::metadata(&p) {
            let t = meta.modified().unwrap_or(UNIX_EPOCH);
            if best.as_ref().map(|(bt, _)| t >= *bt).unwrap_or(true) {
                *best = Some((t, p));
            }
        }
    };
    consider(dir.join(name));
    for n in 1..50 {
        let p = dir.join(format!("{n}_{name}"));
        if !p.exists() && n > 3 {
            break;
        }
        consider(p);
    }
}

impl Core {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn snapshot_of(&self, inner: &Inner) -> String {
        let now = now_ms();
        let nearby = inner.nearby_until.map(|t| t > now).unwrap_or(false);
        let status = inner.status.clone();
        json!({
            "daemon": true,
            "device_name": self.device_name,
            "visible": inner.persist.visible,
            "visible_until": inner.visible_until,
            "nearby": nearby,
            "trusted": inner.persist.trusted,
            "incoming": inner.incoming,
            "history": inner.persist.history,
            "status": status,
            "status_bad": inner.status_bad,
            "downloads": paths::downloads_dir(),
        })
        .to_string()
    }

    /// Publishes the current state to every watcher and saves it.
    fn publish(&self) {
        // One at a time, so snapshots go out in order and state.json.tmp has one writer.
        let mut saved = self.saved.lock().unwrap_or_else(|e| e.into_inner());
        let (snap, persist) = {
            let inner = self.lock();
            let mut persist = serde_json::to_value(&inner.persist).unwrap_or(Value::Null);
            // "Visible for 10 minutes" must not survive a restart as "visible".
            if inner.visible_until.is_some() {
                persist["visible"] = json!(false);
            }
            (self.snapshot_of(&inner), serde_json::to_string_pretty(&persist).unwrap_or_default())
        };
        self.snapshot.send_replace(snap);
        // Progress changes many times a second; the saved state does not.
        if !persist.is_empty() && *saved != persist {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let file = paths::state_file();
            let tmp = file.with_extension("json.tmp");
            // 0600: history can hold shared text and Wi-Fi passwords.
            let written = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .and_then(|mut f| f.write_all(persist.as_bytes()));
            if written.is_ok() && std::fs::rename(&tmp, &file).is_ok() {
                *saved = persist;
            }
        }
    }

    fn set_status(self: &Arc<Self>, text: impl Into<String>) {
        self.show_status(text.into(), false);
    }

    /// A status the bar shows in red.
    fn set_problem(self: &Arc<Self>, text: impl Into<String>) {
        self.show_status(text.into(), true);
    }

    fn show_status(self: &Arc<Self>, text: String, bad: bool) {
        {
            let mut inner = self.lock();
            inner.status = Some(text.clone());
            inner.status_bad = bad;
        }
        self.publish();
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(if bad { 8 } else { 4 })).await;
            let mut inner = me.lock();
            if inner.status.as_deref() == Some(text.as_str()) {
                inner.status = None;
                drop(inner);
                me.publish();
            }
        });
    }

    fn apply_visibility(&self, visible: bool) {
        let v = if visible { Visibility::Visible } else { Visibility::Invisible };
        self.visibility.lock().unwrap_or_else(|e| e.into_inner()).send_modify(|s| *s = v);
    }

    fn set_visible(self: &Arc<Self>, visible: bool) {
        {
            let mut inner = self.lock();
            inner.persist.visible = visible;
            inner.visible_until = None;
            inner.temp_gen += 1;
            if visible {
                inner.nearby_until = None;
            }
        }
        self.apply_visibility(visible);
        self.publish();
    }

    fn set_visible_for(self: &Arc<Self>, minutes: u64) {
        let gen = {
            let mut inner = self.lock();
            // Only "visible for a while": publish() saves it as hidden, so a restart
            // comes back hidden.
            inner.persist.visible = true;
            inner.visible_until = Some(now_ms() + minutes * 60_000);
            inner.temp_gen += 1;
            inner.nearby_until = None;
            inner.temp_gen
        };
        self.apply_visibility(true);
        self.publish();
        let me = self.clone();
        let until = now_ms() + minutes * 60_000;
        tokio::spawn(async move {
            // Short steps against the wall clock: one long sleep would not count
            // the time the laptop was suspended.
            loop {
                let left = until.saturating_sub(now_ms());
                if left == 0 || me.lock().temp_gen != gen {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(left.min(15_000))).await;
            }
            let still = me.lock().temp_gen == gen;
            if still {
                me.set_visible(false);
            }
        });
    }

    fn send_lib(&self, id: &str, action: ChannelAction) {
        let _ = self.lib_tx.send(ChannelMessage {
            id: id.to_string(),
            direction: ChannelDirection::FrontToLib,
            action: Some(action),
            ..Default::default()
        });
    }

    /// Deletes what arrived of a transfer that will not finish, except names that
    /// another transfer in the list is using right now (staging is one folder).
    fn remove_partial(&self, files: &[String]) {
        let in_use: Vec<String> = self.lock().incoming.iter().flat_map(|i| i.files.clone()).collect();
        let mine: Vec<String> = files.iter().filter(|f| !in_use.contains(f)).cloned().collect();
        remove_staged(&mine);
    }

    fn accept(self: &Arc<Self>, id: &str, action: Option<Action>) -> Result<()> {
        let notif = {
            let mut inner = self.lock();
            let inc = inner
                .incoming
                .iter_mut()
                .find(|i| i.id == id)
                .ok_or_else(|| anyhow!("no such transfer"))?;
            if inc.state != "waiting" {
                return Ok(());
            }
            inc.action = Some(action.unwrap_or_else(|| default_action(&inc.kind)));
            inc.state = "receiving".into();
            inc.notif.take()
        };
        if let Some(n) = notif {
            notify::close(n);
        }
        self.send_lib(id, ChannelAction::AcceptTransfer);
        self.publish();
        Ok(())
    }

    fn reject(self: &Arc<Self>, id: &str) -> Result<()> {
        let inc = {
            let mut inner = self.lock();
            let pos = inner.incoming.iter().position(|i| i.id == id).ok_or_else(|| anyhow!("no such transfer"))?;
            inner.incoming.remove(pos)
        };
        let waiting = inc.state == "waiting";
        if let Some(n) = inc.notif {
            notify::close(n);
        }
        self.send_lib(id, if waiting { ChannelAction::RejectTransfer } else { ChannelAction::CancelTransfer });
        // The library's own "cancelled" message finds nothing to clean up (the
        // transfer is already gone from the list), so drop the partial files here.
        // And once more a moment later: a cancel right after accepting can arrive
        // before the library has created the files.
        if !waiting {
            self.remove_partial(&inc.files);
            let me = self.clone();
            let files = inc.files.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(3)).await;
                me.remove_partial(&files);
            });
        }
        self.publish();
        Ok(())
    }

    async fn on_message(self: &Arc<Self>, msg: ChannelMessage) {
        if msg.direction != ChannelDirection::LibToFront {
            return;
        }
        if matches!(msg.rtype, Some(TransferType::Outbound)) {
            return;
        }
        let meta: Value = msg.meta.as_ref().and_then(|m| serde_json::to_value(m).ok()).unwrap_or(Value::Null);
        match msg.state {
            Some(State::WaitingForUserConsent) => self.on_request(&msg.id, &meta).await,
            Some(State::ReceivingFiles) => {
                let tell = {
                    let mut inner = self.lock();
                    let mut started = false;
                    if let Some(inc) = inner.incoming.iter_mut().find(|i| i.id == msg.id) {
                        started = inc.state != "receiving";
                        inc.state = "receiving".into();
                        inc.total_bytes = meta["total_bytes"].as_u64().unwrap_or(inc.total_bytes);
                        inc.ack_bytes = meta["ack_bytes"].as_u64().unwrap_or(inc.ack_bytes);
                    }
                    // The library reports every chunk; the bar only needs a few a second.
                    let now = now_ms();
                    let due = started || now.saturating_sub(inner.last_progress) >= 200;
                    if due {
                        inner.last_progress = now;
                    }
                    due
                };
                if tell {
                    self.publish();
                }
            }
            Some(State::Finished) => {
                let me = self.clone();
                let id = msg.id.clone();
                tokio::spawn(async move {
                    if let Err(e) = me.on_finished(&id, &meta).await {
                        error!("finishing {id}: {e:#}");
                        me.set_problem(format!("Something went wrong: {e}"));
                    }
                });
            }
            Some(State::Rejected) | Some(State::Cancelled) | Some(State::Disconnected) => {
                let removed = {
                    let mut inner = self.lock();
                    match inner.incoming.iter().position(|i| i.id == msg.id && i.state != "finishing") {
                        Some(pos) => Some(inner.incoming.remove(pos)),
                        None => None,
                    }
                };
                if let Some(inc) = removed {
                    if let Some(n) = inc.notif {
                        notify::close(n);
                    }
                    // A transfer still waiting for an answer has no file yet; a same-named
                    // file in staging would belong to another transfer.
                    if inc.state == "receiving" {
                        self.remove_partial(&inc.files);
                        self.set_problem(format!("Transfer from {} stopped", inc.device));
                    }
                    self.publish();
                }
            }
            _ => {}
        }
    }

    async fn on_request(self: &Arc<Self>, id: &str, meta: &Value) {
        let device = meta["source"]["name"].as_str().unwrap_or("Unknown device").to_string();
        let files: Vec<String> = meta["files"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if let Some(bad) = files.iter().find(|f| !is_plain_name(f)) {
            warn!("declined {id} from {device}: unsafe file name {bad:?}");
            self.send_lib(id, ChannelAction::RejectTransfer);
            self.set_problem(format!("Declined {device}: unsafe file name"));
            return;
        }
        let kind = kind_of(&files).to_string();
        let text = meta["text_description"].as_str().map(String::from);
        let trusted = {
            let mut inner = self.lock();
            if inner.incoming.iter().any(|i| i.id == id) {
                return;
            }
            inner.incoming.push(Incoming {
                id: id.to_string(),
                device: device.clone(),
                pin: meta["pin_code"].as_str().map(String::from),
                state: "waiting".into(),
                kind: kind.clone(),
                files: files.clone(),
                text: text.clone(),
                total_bytes: meta["total_bytes"].as_u64().unwrap_or(0),
                ack_bytes: 0,
                action: None,
                auto: false,
                started: now_ms(),
                notif: None,
            });
            inner.persist.trusted.iter().any(|t| t == &device)
        };
        self.publish();

        if trusted {
            if let Some(inc) = self.lock().incoming.iter_mut().find(|i| i.id == id) {
                inc.auto = true;
            }
            let _ = self.accept(id, None);
            return;
        }

        let what = match kind.as_str() {
            "text" => text.clone().map(|t| format!("“{t}”")).unwrap_or_else(|| "some text".into()),
            _ if files.len() == 1 => files[0].clone(),
            "images" => format!("{} photos", files.len()),
            _ => format!("{} files", files.len()),
        };
        let pin = meta["pin_code"].as_str().map(|p| format!(" · PIN {p}")).unwrap_or_default();
        let body = format!("{what}{pin}");
        let summary = format!("{device} wants to share");
        let actions: Vec<(&str, &str)> = if kind == "files" {
            vec![("save", "Save"), ("copy", "Copy"), ("reject", "Decline")]
        } else {
            vec![("copy", "Copy"), ("save", "Save"), ("reject", "Decline")]
        };
        let me = self.clone();
        let tid = id.to_string();
        let nid = notify::show(
            notify::Note { summary: &summary, body: &body, image: None, actions: &actions, critical: true, timeout_ms: None },
            move |key| {
                {
                    let mut inner = me.lock();
                    if let Some(inc) = inner.incoming.iter_mut().find(|i| i.id == tid) {
                        inc.notif = None;
                    }
                }
                let _ = match key.as_str() {
                    "copy" => me.accept(&tid, Some(Action::Copy)),
                    "save" => me.accept(&tid, Some(Action::Save)),
                    "reject" => me.reject(&tid),
                    _ => Ok(()),
                };
            },
        )
        .await;
        let mut inner = self.lock();
        match inner.incoming.iter_mut().find(|i| i.id == id && i.state == "waiting") {
            Some(inc) => inc.notif = nid,
            None => {
                if let Some(n) = nid {
                    notify::close(n);
                }
            }
        }
    }

    async fn on_finished(self: &Arc<Self>, id: &str, meta: &Value) -> Result<()> {
        let inc = {
            let mut inner = self.lock();
            let Some(inc) = inner.incoming.iter_mut().find(|i| i.id == id) else {
                return Ok(());
            };
            inc.state = "finishing".into();
            inc.ack_bytes = inc.total_bytes;
            inc.clone()
        };
        self.publish();
        let action = inc.action.unwrap_or_else(|| default_action(&inc.kind));
        let result = if inc.files.is_empty() {
            self.finish_text(&inc, meta, action).await
        } else {
            self.finish_files(&inc, action).await
        };
        self.lock().incoming.retain(|i| i.id != id);
        self.publish();
        result
    }

    fn push_history(&self, items: Vec<Item>) {
        let mut dropped = vec![];
        // What just arrived is never trimmed away (its files are about to be copied).
        let limit = HISTORY_LIMIT.max(items.len());
        {
            let mut inner = self.lock();
            for item in items {
                inner.persist.history.insert(0, item);
            }
            while inner.persist.history.len() > limit {
                if let Some(old) = inner.persist.history.pop() {
                    dropped.push(old);
                }
            }
        }
        for old in dropped {
            forget_file(&old);
        }
    }

    async fn finish_text(self: &Arc<Self>, inc: &Incoming, meta: &Value, action: Action) -> Result<()> {
        let payload = meta["text_payload"].as_str().unwrap_or("").to_string();
        let kind = match meta["text_type"].as_str() {
            Some("Url") => "url",
            Some("Wifi") => "wifi",
            _ => "text",
        };
        // Wi-Fi arrives as "ssid: password"; the password is what you paste.
        let (name, to_copy) = if kind == "wifi" {
            match payload.split_once(": ") {
                Some((ssid, pass)) => (ssid.to_string(), pass.to_string()),
                None => (payload.clone(), payload.clone()),
            }
        } else {
            (inc.text.clone().unwrap_or_else(|| payload.clone()), payload.clone())
        };
        let item_id = uuid::Uuid::new_v4().to_string();
        self.push_history(vec![Item {
            id: item_id.clone(),
            kind: kind.into(),
            name,
            path: None,
            text: Some(payload.clone()),
            size: payload.len() as u64,
            device: inc.device.clone(),
            time: now_ms(),
            saved: false,
        }]);
        // You chose Save: it becomes a text file in Downloads instead of a clipboard entry.
        if action == Action::Save {
            self.save_item(&item_id)?;
            let shown: String = payload.chars().take(120).collect();
            notify::show(
                notify::Note { summary: "Saved to Downloads", body: &shown, image: None, actions: &[], critical: false, timeout_ms: Some(5000) },
                |_| {},
            )
            .await;
            return Ok(());
        }
        // It is in the bar's history now, so a clipboard failure does not lose it.
        clip::copy_text(&to_copy).await.context("couldn't copy; the text is kept in the bar")?;
        let (summary, actions): (&str, Vec<(&str, &str)>) = match kind {
            "url" => ("Link copied", vec![("open", "Open")]),
            "wifi" => ("Wi-Fi password copied", vec![]),
            _ => ("Text copied", vec![]),
        };
        let shown: String = payload.chars().take(120).collect();
        let url = payload.clone();
        notify::show(
            notify::Note { summary, body: &shown, image: None, actions: &actions, critical: false, timeout_ms: Some(5000) },
            move |key| {
                if key == "open" {
                    spawn_detached("xdg-open", &[url.as_str()]);
                }
            },
        )
        .await;
        self.set_status(summary);
        Ok(())
    }

    async fn finish_files(self: &Arc<Self>, inc: &Incoming, action: Action) -> Result<()> {
        let staged_total: u64 = inc
            .files
            .iter()
            .filter_map(|n| find_staged(n))
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        let base = if staged_total > RAM_LIMIT { paths::big_files_dir() } else { paths::files_dir() };
        let bucket = base.join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&bucket)?;
        let downloads = paths::downloads_dir();
        let mut items = vec![];
        let mut save_failed = false;
        for name in &inc.files {
            let Some(staged) = find_staged(name) else {
                warn!("received file {name} not found in staging");
                continue;
            };
            let size = std::fs::metadata(&staged).map(|m| m.len()).unwrap_or(0);
            let to_downloads = || -> Result<PathBuf> {
                std::fs::create_dir_all(&downloads)?;
                let dest = unique_path(&downloads, name);
                move_file(&staged, &dest)?;
                Ok(dest)
            };
            let (dest, saved) = match if action == Action::Save { to_downloads().map(Some) } else { Ok(None) } {
                Ok(Some(dest)) => (dest, true),
                other => {
                    // Copy, or Downloads could not take it: keep it for the bar.
                    if let Err(e) = other {
                        warn!("saving {name}: {e:#}");
                        save_failed = true;
                    }
                    let dest = bucket.join(name);
                    if let Err(e) = move_file(&staged, &dest) {
                        warn!("keeping {name}: {e:#}");
                        continue;
                    }
                    (dest, false)
                }
            };
            items.push(Item {
                id: uuid::Uuid::new_v4().to_string(),
                kind: if clip::is_image(name) { "image".into() } else { "file".into() },
                name: name.clone(),
                path: Some(dest.to_string_lossy().to_string()),
                text: None,
                size,
                device: inc.device.clone(),
                time: now_ms(),
                saved,
            });
        }
        if std::fs::read_dir(&bucket).map(|mut d| d.next().is_none()).unwrap_or(false) {
            let _ = std::fs::remove_dir(&bucket);
        }
        if items.is_empty() {
            return Err(anyhow!("no files arrived"));
        }

        let count = items.len();
        let first = items[0].clone();
        let last_path = PathBuf::from(items.last().unwrap().path.clone().unwrap_or_default());
        let all_paths: Vec<PathBuf> = items.iter().filter_map(|i| i.path.clone().map(PathBuf::from)).collect();
        let image_path = items.iter().rev().find(|i| i.kind == "image").and_then(|i| i.path.clone()).map(PathBuf::from);
        // For the notification's Save button: these items, whatever arrives meanwhile.
        let item_ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
        // Newest first in history, keeping the phone's order within a transfer.
        self.push_history(items.into_iter().rev().collect());
        if save_failed {
            return Err(anyhow!("couldn't save to Downloads; the files are kept in the bar"));
        }

        let (summary, body, actions): (String, String, Vec<(&str, &str)>) = match action {
            Action::Copy => {
                if first.kind == "image" && count == 1 {
                    clip::copy_image(&last_path).await?;
                    ("Photo copied".into(), format!("{} · from {}", first.name, inc.device), vec![("save", "Save")])
                } else if image_path.is_some() && all_paths.iter().all(|p| clip::is_image(&p.to_string_lossy())) {
                    // A clipboard holds one image: copy the last, the rest wait in the bar.
                    clip::copy_image(image_path.as_ref().unwrap()).await?;
                    (
                        format!("{count} photos received"),
                        "Last one is on the clipboard · the rest are in the bar".into(),
                        vec![("save", "Save all")],
                    )
                } else {
                    clip::copy_files(&all_paths).await?;
                    (
                        if count == 1 { "File copied".into() } else { format!("{count} files copied") },
                        format!("Paste into a folder or chat · from {}", inc.device),
                        vec![("save", "Save")],
                    )
                }
            }
            Action::Save => (
                if count == 1 { "Saved to Downloads".into() } else { format!("{count} files saved") },
                if count == 1 { first.name.clone() } else { downloads.display().to_string() },
                vec![("open", "Open"), ("reveal", "Show in folder")],
            ),
        };
        self.set_status(summary.clone());

        let me = self.clone();
        let reveal_path = last_path.clone();
        notify::show(
            notify::Note {
                summary: &summary,
                body: &body,
                image: image_path.as_deref(),
                actions: &actions,
                critical: false,
                timeout_ms: Some(6000),
            },
            move |key| match key.as_str() {
                "save" => {
                    for id in &item_ids {
                        let _ = me.save_item(id);
                    }
                }
                "open" => spawn_detached("xdg-open", &[&reveal_path.to_string_lossy()]),
                "reveal" => reveal(&reveal_path),
                _ => {}
            },
        )
        .await;
        Ok(())
    }

    fn item(&self, id: &str) -> Result<Item> {
        self.lock()
            .persist
            .history
            .iter()
            .find(|i| i.id == id)
            .cloned()
            .ok_or_else(|| anyhow!("no such item"))
    }

    async fn copy_item(self: &Arc<Self>, id: &str) -> Result<()> {
        let item = self.item(id)?;
        match (&item.path, &item.text) {
            (Some(p), _) => {
                let p = PathBuf::from(p);
                if !p.exists() {
                    return Err(anyhow!("file is gone"));
                }
                if item.kind == "image" {
                    clip::copy_image(&p).await?;
                } else {
                    clip::copy_files(&[p]).await?;
                }
            }
            (None, Some(t)) => {
                let t = if item.kind == "wifi" {
                    t.split_once(": ").map(|(_, p)| p.to_string()).unwrap_or(t.clone())
                } else {
                    t.clone()
                };
                clip::copy_text(&t).await?;
            }
            _ => return Err(anyhow!("nothing to copy")),
        }
        self.set_status("Copied");
        Ok(())
    }

    fn save_item(self: &Arc<Self>, id: &str) -> Result<()> {
        let item = self.item(id)?;
        if item.saved {
            return Ok(());
        }
        let downloads = paths::downloads_dir();
        std::fs::create_dir_all(&downloads)?;
        let dest = match (&item.path, &item.text) {
            (Some(p), _) => {
                let dest = unique_path(&downloads, &item.name);
                move_file(Path::new(p), &dest)?;
                if let Some(parent) = Path::new(p).parent() {
                    let _ = std::fs::remove_dir(parent);
                }
                // If the clipboard holds this file (Copy, then Save), a paste would
                // now point at nothing: point it at the saved file.
                let (old, new) = (PathBuf::from(p), dest.clone());
                tokio::spawn(async move {
                    let _ = clip::follow_moved_file(&old, &new).await;
                });
                dest
            }
            (None, Some(t)) => {
                let dest = unique_path(&downloads, "shared-text.txt");
                std::fs::write(&dest, t)?;
                dest
            }
            _ => return Err(anyhow!("nothing to save")),
        };
        {
            let mut inner = self.lock();
            if let Some(i) = inner.persist.history.iter_mut().find(|i| i.id == id) {
                i.saved = true;
                i.path = Some(dest.to_string_lossy().to_string());
                if i.kind != "image" && i.kind != "file" {
                    i.kind = "file".into();
                    i.name = dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                }
            }
        }
        self.set_status("Saved to Downloads");
        Ok(())
    }

    fn remove_item(self: &Arc<Self>, id: &str) -> Result<()> {
        let removed = {
            let mut inner = self.lock();
            let pos = inner.persist.history.iter().position(|i| i.id == id).ok_or_else(|| anyhow!("no such item"))?;
            inner.persist.history.remove(pos)
        };
        forget_file(&removed);
        self.publish();
        Ok(())
    }

    fn clear_history(self: &Arc<Self>) {
        let all = std::mem::take(&mut self.lock().persist.history);
        for item in all {
            forget_file(&item);
        }
        self.publish();
    }

    fn set_trusted(self: &Arc<Self>, device: &str, trusted: bool) {
        {
            let mut inner = self.lock();
            inner.persist.trusted.retain(|d| d != device);
            if trusted {
                inner.persist.trusted.push(device.to_string());
            }
        }
        self.publish();
    }

    fn on_nearby(self: &Arc<Self>) {
        let now = now_ms();
        let should_note = {
            let mut inner = self.lock();
            if inner.persist.visible {
                return;
            }
            inner.nearby_until = Some(now + NEARBY_SECS * 1000);
            let due = now.saturating_sub(inner.last_nearby_note) > 120_000;
            if due {
                inner.last_nearby_note = now;
            }
            due
        };
        self.publish();
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(NEARBY_SECS + 1)).await;
            me.publish();
        });
        if should_note {
            let me = self.clone();
            tokio::spawn(async move {
                notify::show(
                    notify::Note {
                        summary: "A phone nearby is sharing",
                        body: "You are hidden. Become visible for 5 minutes?",
                        image: None,
                        actions: &[("show", "Become visible")],
                        critical: false,
                        timeout_ms: Some(15000),
                    },
                    move |key| {
                        if key == "show" {
                            me.set_visible_for(5);
                        }
                    },
                )
                .await;
            });
        }
    }

    async fn handle(self: &Arc<Self>, req: &Value) -> Result<Value> {
        let s = |k: &str| req[k].as_str().unwrap_or("").to_string();
        match req["cmd"].as_str().unwrap_or("") {
            "status" => return Ok(serde_json::from_str(&self.snapshot.borrow().clone())?),
            "visible" => self.set_visible(true),
            "hidden" => self.set_visible(false),
            "toggle" => {
                let v = self.lock().persist.visible;
                self.set_visible(!v);
            }
            "temp" => self.set_visible_for(req["minutes"].as_u64().unwrap_or(5).clamp(1, 240)),
            "accept" => self.accept(&s("id"), Action::parse(&s("action")))?,
            "reject" => self.reject(&s("id"))?,
            "trust" => self.set_trusted(&s("device"), true),
            "untrust" => self.set_trusted(&s("device"), false),
            "copy" => self.copy_item(&s("item")).await?,
            "save" => self.save_item(&s("item"))?,
            "remove" => self.remove_item(&s("item"))?,
            "clear" => self.clear_history(),
            "open" => {
                let item = self.item(&s("item"))?;
                match (item.path, item.text) {
                    (Some(p), _) if !Path::new(&p).exists() => return Err(anyhow!("file is gone")),
                    (Some(p), _) => spawn_detached("xdg-open", &[&p]),
                    (None, Some(t)) if item.kind == "url" => spawn_detached("xdg-open", &[&t]),
                    _ => return Err(anyhow!("nothing to open")),
                }
            }
            "reveal" => {
                let item = self.item(&s("item"))?;
                match item.path {
                    Some(p) if !Path::new(&p).exists() => return Err(anyhow!("file is gone")),
                    Some(p) => reveal(Path::new(&p)),
                    None => return Err(anyhow!("not a file")),
                }
            }
            "downloads" => spawn_detached("xdg-open", &[&paths::downloads_dir().to_string_lossy()]),
            other => return Err(anyhow!("unknown command: {other}")),
        }
        Ok(json!({"ok": true}))
    }
}

fn forget_file(item: &Item) {
    if item.saved {
        return;
    }
    if let Some(p) = &item.path {
        let p = Path::new(p);
        if p.starts_with(paths::files_dir()) || p.starts_with(paths::big_files_dir()) {
            let _ = std::fs::remove_file(p);
            if let Some(parent) = p.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

fn spawn_detached(program: &str, args: &[&str]) {
    let _ = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|mut c| {
            std::thread::spawn(move || {
                let _ = c.wait();
            })
        });
}

/// Opens the file manager with the file selected.
fn reveal(path: &Path) {
    let uri = clip::encode_uri(path);
    let status = std::process::Command::new("busctl")
        .args([
            "--user",
            "call",
            "org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1",
            "ShowItems",
            "ass",
            "1",
            &uri,
            "",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        if let Some(parent) = path.parent() {
            spawn_detached("xdg-open", &[&parent.to_string_lossy()]);
        }
    }
}

async fn serve_client(core: Arc<Core>, stream: UnixStream) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let Some(line) = lines.next_line().await? else { return Ok(()) };
    let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);

    if req["cmd"] == "watch" {
        let mut rx = core.snapshot.subscribe();
        loop {
            let snap = rx.borrow_and_update().clone();
            write.write_all(snap.as_bytes()).await?;
            write.write_all(b"\n").await?;
            if rx.changed().await.is_err() {
                return Ok(());
            }
        }
    }

    let reply = match core.handle(&req).await {
        Ok(v) => v,
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    };
    write.write_all(reply.to_string().as_bytes()).await?;
    write.write_all(b"\n").await?;
    Ok(())
}

pub async fn run() -> Result<()> {
    let sock = paths::socket_path();
    if UnixStream::connect(&sock).await.is_ok() {
        return Err(anyhow!("qsbar daemon already running"));
    }
    let _ = std::fs::remove_file(&sock);

    let staging = paths::staging_dir();
    // Leftovers here are partial files from an interrupted transfer.
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_dir_all(paths::big_staging_dir());
    std::fs::create_dir_all(&staging)?;
    std::fs::create_dir_all(paths::big_staging_dir())?;
    // Big transfers pass through the disk cache: nobody else gets to read them.
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(paths::cache_dir(), std::fs::Permissions::from_mode(0o700));
    }
    rqs_lib::set_large_download(Some(paths::big_staging_dir()), RAM_LIMIT);
    std::fs::create_dir_all(paths::files_dir())?;
    // Copied files never persist on disk: drop old disk copies on start.
    let _ = std::fs::remove_dir_all(paths::legacy_files_dir());
    let _ = std::fs::remove_dir_all(paths::big_files_dir());
    if let Some(parent) = paths::state_file().parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut persist: Persist = std::fs::read_to_string(paths::state_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    persist.history.retain(|i| i.path.as_ref().map(|p| Path::new(p).exists()).unwrap_or(true));

    let start_visible = persist.visible;
    let mut rqs = RQS::new(
        if start_visible { Visibility::Visible } else { Visibility::Invisible },
        None,
        Some(staging.clone()),
    );
    let (_send_tx, mut ble_rx) = rqs.run().await?;

    let core = Arc::new(Core {
        lib_tx: rqs.message_sender.clone(),
        visibility: rqs.visibility_sender.clone(),
        inner: Mutex::new(Inner {
            persist,
            visible_until: None,
            temp_gen: 0,
            nearby_until: None,
            last_nearby_note: 0,
            incoming: vec![],
            status: None,
            status_bad: false,
            last_progress: 0,
        }),
        saved: Mutex::new(String::new()),
        snapshot: watch::channel(String::new()).0,
        device_name: paths::hostname(),
    });
    core.publish();

    // Library events
    {
        let core = core.clone();
        let mut rx = rqs.message_sender.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => core.on_message(msg).await,
                    Err(broadcast::error::RecvError::Lagged(n)) => warn!("lagged {n} messages"),
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // "A device nearby is sharing" over Bluetooth
    {
        let core = core.clone();
        tokio::spawn(async move {
            while let Ok(()) = ble_rx.recv().await {
                core.on_nearby();
            }
        });
    }

    let listener = UnixListener::bind(&sock)?;
    info!("qsbar listening on {sock:?}, visible={start_visible}");
    let accept_loop = {
        let core = core.clone();
        async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let core = core.clone();
                        tokio::spawn(async move {
                            let _ = serve_client(core, stream).await;
                        });
                    }
                    Err(e) => error!("accept: {e}"),
                }
            }
        }
    };

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = accept_loop => {}
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    info!("stopping");
    let _ = std::fs::remove_file(&sock);
    rqs.stop().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_plain_name;

    #[test]
    fn plain_names_pass() {
        for name in ["photo.jpg", "My file (1).pdf", "a..b", ".hidden", "čaj.txt"] {
            assert!(is_plain_name(name), "{name}");
        }
    }

    #[test]
    fn names_that_leave_the_folder_are_refused() {
        for name in ["", ".", "..", "../x", "a/b", "/etc/passwd", "a\\b", "x/../../y", "dir/"] {
            assert!(!is_plain_name(name), "{name}");
        }
    }
}
