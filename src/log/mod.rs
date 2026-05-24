pub mod entry;
pub mod watcher;

pub use entry::{LogEntry as ParsedLogEntry, LogEntryParser};
pub use watcher::{find_log_dir, read_static_file, LogEntry as RawLogEntry, LogWatcher};
