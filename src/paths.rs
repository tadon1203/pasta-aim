use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn executable_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn settings_path() -> PathBuf {
    executable_dir().join("settings.json")
}

pub fn models_dir() -> PathBuf {
    executable_dir().join("models")
}

pub fn logs_dir() -> PathBuf {
    executable_dir().join("logs")
}

pub fn session_log_path() -> PathBuf {
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    session_log_path_in(&logs_dir(), unix_ms, std::process::id())
}

pub(crate) fn session_log_path_in(directory: &std::path::Path, unix_ms: u128, pid: u32) -> PathBuf {
    directory.join(format!("session-{unix_ms}-{pid}.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_log_name_contains_timestamp_and_pid() {
        assert_eq!(
            session_log_path_in(std::path::Path::new("data/logs"), 1234, 42),
            PathBuf::from("data/logs/session-1234-42.log")
        );
    }
}
