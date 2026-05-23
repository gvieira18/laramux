#[allow(unused_imports)]
pub mod entry;
pub mod parser;
pub mod watcher;

#[allow(unused_imports)]
pub use entry::{LogEntry as ParsedLogEntry, LogEntryParser, Stacktrace};
#[allow(unused_imports)] // read_static_file will be used by Task 6
pub use watcher::{find_log_dir, read_static_file, LogEntry as RawLogEntry, LogWatcher};
