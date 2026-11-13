//! A small file logger and a panic hook. Release builds have no console, so without these
//! `log` output and panics on background threads (e.g. the PTY reader) would vanish.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use log::{LevelFilter, Log, Metadata, Record};

const MAX_LOG_BYTES: u64 = 1024 * 1024;

pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join("logs").join("komando.log")
}

struct FileLogger {
    file: Mutex<Option<File>>,
    level: LevelFilter,
    others: LevelFilter,
}

fn format_line(timestamp: &str, record: &Record) -> String {
    format!("{timestamp} {:<5} {} {}\n", record.level(), record.target(), record.args())
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        // Other crates (gpui, russh) only log warnings and errors unless KOMANDO_LOG asks for more,
        // so their chatter can't grow the file during a long session.
        let level = if metadata.target().starts_with("komando") { self.level } else { self.others };
        metadata.level() <= level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format_line(&chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(), record);
        if let Ok(mut guard) = self.file.lock()
            && let Some(file) = guard.as_mut()
        {
            let _ = file.write_all(line.as_bytes());
        }
        if cfg!(debug_assertions) {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock()
            && let Some(file) = guard.as_mut()
        {
            let _ = file.flush();
        }
    }
}

/// Moves `path` to `komando.old.log` next to it when it exceeds `max` bytes.
fn rotate_if_large(path: &Path, max: u64) {
    if fs::metadata(path).is_ok_and(|m| m.len() > max) {
        let old = path.with_file_name("komando.old.log");
        let _ = fs::remove_file(&old);
        let _ = fs::rename(path, old);
    }
}

fn open_log(path: &Path) -> Option<File> {
    fs::create_dir_all(path.parent()?).ok()?;
    rotate_if_large(path, MAX_LOG_BYTES);
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Komando's own level and other crates' level: Info and Warn, or both as `KOMANDO_LOG` says.
fn levels_from_env() -> (LevelFilter, LevelFilter) {
    match std::env::var("KOMANDO_LOG").ok().and_then(|v| v.trim().parse().ok()) {
        Some(level) => (level, level),
        None => (LevelFilter::Info, LevelFilter::Warn),
    }
}

/// Installs the file logger and the panic hook. Safe to call once; later calls are no-ops.
pub fn init(data_dir: &Path) {
    let (level, others) = levels_from_env();
    let logger = FileLogger { file: Mutex::new(open_log(&log_path(data_dir))), level, others };
    if log::set_boxed_logger(Box::new(logger)).is_err() {
        return;
    }
    log::set_max_level(level.max(others));

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!(
            "panic in thread '{}' at {}: {}\n{backtrace}",
            thread.name().unwrap_or("<unnamed>"),
            info.location().map_or_else(|| "<unknown>".to_string(), |l| l.to_string()),
            panic_message(info.payload()),
        );
        log::logger().flush();
        previous(info);
    }));

    log::info!("Komando {} starting on {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS);
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string payload>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("komando-logging-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_large_log_is_rotated_and_replaces_the_old_one() {
        let dir = temp_dir("rotate");
        let path = dir.join("komando.log");
        fs::write(dir.join("komando.old.log"), "previous").unwrap();
        fs::write(&path, vec![b'x'; 100]).unwrap();
        rotate_if_large(&path, 50);
        assert!(!path.exists());
        assert_eq!(fs::read(dir.join("komando.old.log")).unwrap().len(), 100);

        fs::write(&path, "small").unwrap();
        rotate_if_large(&path, 50);
        assert!(path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lines_carry_timestamp_level_target_and_message() {
        let line = format_line(
            "T",
            &Record::builder().level(log::Level::Warn).target("komando::session").args(format_args!("lost {}", 3)).build(),
        );
        assert_eq!(line, "T WARN  komando::session lost 3\n");
    }
}
