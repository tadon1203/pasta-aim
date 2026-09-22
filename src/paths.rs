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

pub fn logs_dir() -> PathBuf {
    executable_dir().join("logs")
}

pub fn log_file_path() -> PathBuf {
    logs_dir().join("nice-aimassist.log")
}

#[cfg(test)]
pub(crate) fn log_file_path_in(directory: &std::path::Path) -> PathBuf {
    directory.join("logs").join("nice-aimassist.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_is_placed_under_logs_directory() {
        assert_eq!(
            log_file_path_in(std::path::Path::new("data")),
            PathBuf::from("data")
                .join("logs")
                .join("nice-aimassist.log")
        );
    }
}
