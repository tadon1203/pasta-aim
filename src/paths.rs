use std::path::PathBuf;

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
