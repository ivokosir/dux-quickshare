use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("qsbar.sock")
}

pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("qsbar")
}

/// Where rqs_lib writes files while they arrive: RAM.
pub fn staging_dir() -> PathBuf {
    runtime_dir().join("qsbar").join("incoming")
}

/// Arrival dir for transfers too big for RAM.
pub fn big_staging_dir() -> PathBuf {
    cache_dir().join("incoming")
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Copied (not saved) files live in RAM (tmpfs): never written to your
/// disk folders, gone at logout/reboot, re-copyable from the bar until then.
pub fn files_dir() -> PathBuf {
    runtime_dir().join("qsbar").join("files")
}

/// Fallback for copied files too big for RAM.
pub fn big_files_dir() -> PathBuf {
    cache_dir().join("big")
}

/// Older versions kept copied files here.
pub fn legacy_files_dir() -> PathBuf {
    cache_dir().join("files")
}

pub fn state_file() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("qsbar").join("state.json")
}

/// XDG_DOWNLOAD_DIR from user-dirs.dirs, else ~/Downloads.
pub fn downloads_dir() -> PathBuf {
    let cfg = xdg("XDG_CONFIG_HOME", ".config").join("user-dirs.dirs");
    if let Ok(text) = std::fs::read_to_string(cfg) {
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("XDG_DOWNLOAD_DIR=") {
                let value = rest.trim_matches('"').replace("$HOME", &home().to_string_lossy());
                if !value.is_empty() {
                    return PathBuf::from(value);
                }
            }
        }
    }
    home().join("Downloads")
}

pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "this computer".into())
}
