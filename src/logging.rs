use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt;

use crate::{config::DiagnosticsConfig, paths};

static FRAME_TRACE: AtomicBool = AtomicBool::new(false);
static MAK_COMMAND_TRACE: AtomicBool = AtomicBool::new(false);

pub fn set_diagnostics(value: DiagnosticsConfig) {
    FRAME_TRACE.store(value.frame_trace, Ordering::Release);
    MAK_COMMAND_TRACE.store(value.mak_command_trace, Ordering::Release);
    makxd::set_command_trace(value.mak_command_trace);
}

pub fn frame_trace_enabled() -> bool {
    FRAME_TRACE.load(Ordering::Acquire)
}

pub fn mak_command_trace_enabled() -> bool {
    MAK_COMMAND_TRACE.load(Ordering::Acquire)
}

/// Keeps the asynchronous file writer alive until application shutdown.
pub struct Logging {
    _guard: WorkerGuard,
}

pub fn init() -> Option<Logging> {
    let path = paths::log_file_path();
    let file = match open_append_file(&path) {
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
    let subscriber = fmt::Subscriber::builder()
        // Trace call sites are additionally guarded by Diagnostics toggles.
        .with_max_level(Level::TRACE)
        .with_ansi(false)
        .with_thread_names(true)
        .with_thread_ids(true)
        .with_target(true)
        .with_writer(writer)
        .finish();
    if let Err(error) = tracing::subscriber::set_global_default(subscriber) {
        eprintln!("nice-aimassist: failed to install file logger: {error}");
        return None;
    }

    Some(Logging { _guard: guard })
}

fn open_append_file(path: &Path) -> io::Result<File> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_file_creates_parent_and_preserves_existing_contents() {
        let temp =
            std::env::temp_dir().join(format!("nice-aimassist-log-test-{}", std::process::id()));
        let path = temp.join("nested").join("nice-aimassist.log");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "before\n").unwrap();
        use std::io::Write;
        writeln!(open_append_file(&path).unwrap(), "after").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "before\nafter\n");
        let _ = fs::remove_dir_all(temp);
    }
}
