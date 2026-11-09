use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use nice_aimassist::settings::logs_dir;
use tracing_appender::non_blocking::{ErrorCounter, WorkerGuard};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt};

/// Keeps the asynchronous file writer alive until application shutdown.
#[derive(Debug)]
pub struct Logging {
    _guard: WorkerGuard,
    error_counter: ErrorCounter,
}

impl Drop for Logging {
    fn drop(&mut self) {
        tracing::info!(
            dropped_log_lines = self.error_counter.dropped_lines(),
            "session_ended"
        );
    }
}

pub fn init() -> Result<Option<Logging>> {
    let path = session_log_path()?;
    let file = match create_session_file(&path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!(
                "nice-aimassist: failed to initialize {}: {error}",
                path.display()
            );
            return Ok(None);
        }
    };

    let (writer, guard) = tracing_appender::non_blocking(file);
    let error_counter = writer.error_counter();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let subscriber = tracing_subscriber::registry().with(filter).with(
        fmt::layer()
            .with_ansi(false)
            .with_thread_names(true)
            .with_thread_ids(true)
            .with_target(true)
            .with_writer(writer),
    );
    if let Err(error) = tracing::subscriber::set_global_default(subscriber) {
        eprintln!("nice-aimassist: failed to install file logger: {error}");
        return Ok(None);
    }

    tracing::info!(
        pid = std::process::id(),
        log_path = %path.display(),
        build = if cfg!(debug_assertions) { "debug" } else { "release" },
        "session_started"
    );

    Ok(Some(Logging {
        _guard: guard,
        error_counter,
    }))
}

fn session_log_path() -> Result<PathBuf> {
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Ok(logs_dir()?.join(format!("session-{unix_ms}-{}.log", std::process::id())))
}

fn create_session_file(path: &Path) -> io::Result<File> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    File::create(path)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn session_file_creates_parent_and_truncates_existing_contents() {
        let temp =
            std::env::temp_dir().join(format!("nice-aimassist-log-test-{}", std::process::id()));
        let path = temp.join("nested").join("nice-aimassist.log");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "before\n").unwrap();
        writeln!(create_session_file(&path).unwrap(), "after").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "after\n");
        let _ = fs::remove_dir_all(temp);
    }
}
