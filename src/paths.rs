use std::path::PathBuf;

pub const DATA_DIR_ENV: &str = "NICE_AIMASSIST_DATA_DIR";

pub fn executable_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_dir() -> PathBuf {
    std::env::var_os(DATA_DIR_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(executable_dir)
}

pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}
