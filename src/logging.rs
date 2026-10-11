use std::{
    fs::{self, File},
    io,
    path::Path,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use tracing_appender::non_blocking::{ErrorCounter, WorkerGuard};
use tracing_subscriber::{fmt, layer::SubscriberExt, registry::Registry, reload};

use crate::{config::LogLevel, paths};

static FRAME_TRACE: AtomicBool = AtomicBool::new(false);
static MAKCU_COMMAND_TRACE: AtomicBool = AtomicBool::new(false);
static LEVEL_HANDLE: OnceLock<reload::Handle<tracing_subscriber::filter::LevelFilter, Registry>> =
    OnceLock::new();

pub fn frame_trace_enabled() -> bool {
    FRAME_TRACE.load(Ordering::Acquire)
}

pub fn makcu_command_trace_enabled() -> bool {
    MAKCU_COMMAND_TRACE.load(Ordering::Acquire)
}

pub fn set_level(level: LogLevel) {
    let trace = level == LogLevel::Trace;
    FRAME_TRACE.store(trace, Ordering::Release);
    MAKCU_COMMAND_TRACE.store(trace, Ordering::Release);

    if let Some(handle) = LEVEL_HANDLE.get()
        && let Err(error) = handle.modify(|filter| *filter = level_filter(level))
    {
        eprintln!("nice-aimassist: failed to update log level: {error}");
    }
}

/// Keeps the asynchronous file writer alive until application shutdown.
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

pub fn init() -> Option<Logging> {
    let path = paths::session_log_path();
    let file = match create_session_file(&path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!(
                "nice-aimassist: failed to initialize {}: {error}",
                path.display()
            );
            return None;
        }
    };

    let (writer, guard) = tracing_appender::non_blocking(file);
    let error_counter = writer.error_counter();
    let (filter, handle) = reload::Layer::new(level_filter(LogLevel::Info));
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
        return None;
    }

    let _ = LEVEL_HANDLE.set(handle);

    tracing::info!(
        pid = std::process::id(),
        log_path = %path.display(),
        build = if cfg!(debug_assertions) { "debug" } else { "release" },
        "session_started"
    );

    Some(Logging {
        _guard: guard,
        error_counter,
    })
}

fn level_filter(level: LogLevel) -> tracing_subscriber::filter::LevelFilter {
    match level {
        LogLevel::Off => tracing_subscriber::filter::LevelFilter::OFF,
        LogLevel::Error => tracing_subscriber::filter::LevelFilter::ERROR,
        LogLevel::Warn => tracing_subscriber::filter::LevelFilter::WARN,
        LogLevel::Info => tracing_subscriber::filter::LevelFilter::INFO,
        LogLevel::Debug => tracing_subscriber::filter::LevelFilter::DEBUG,
        LogLevel::Trace => tracing_subscriber::filter::LevelFilter::TRACE,
    }
}

fn create_session_file(path: &Path) -> io::Result<File> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    File::create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_file_creates_parent_and_truncates_existing_contents() {
        let temp =
            std::env::temp_dir().join(format!("nice-aimassist-log-test-{}", std::process::id()));
        let path = temp.join("nested").join("nice-aimassist.log");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "before\n").unwrap();
        use std::io::Write;
        writeln!(create_session_file(&path).unwrap(), "after").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "after\n");
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn log_levels_map_to_minimum_level_filters() {
        assert_eq!(
            level_filter(LogLevel::Off),
            tracing_subscriber::filter::LevelFilter::OFF
        );
        assert_eq!(
            level_filter(LogLevel::Error),
            tracing_subscriber::filter::LevelFilter::ERROR
        );
        assert_eq!(
            level_filter(LogLevel::Warn),
            tracing_subscriber::filter::LevelFilter::WARN
        );
        assert_eq!(
            level_filter(LogLevel::Info),
            tracing_subscriber::filter::LevelFilter::INFO
        );
        assert_eq!(
            level_filter(LogLevel::Debug),
            tracing_subscriber::filter::LevelFilter::DEBUG
        );
        assert_eq!(
            level_filter(LogLevel::Trace),
            tracing_subscriber::filter::LevelFilter::TRACE
        );
    }
}
