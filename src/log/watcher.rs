use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::Result;
use crate::event::Event;
use crate::log::entry::LogEntryParser;

/// A log line with its source file
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub content: String,
    pub file: String,
}

/// Watch Laravel log directory for changes
pub struct LogWatcher {
    log_dir: PathBuf,
    additional_files: Vec<PathBuf>,
    event_tx: mpsc::Sender<Event>,
    cancel_token: CancellationToken,
}

impl LogWatcher {
    pub fn new(
        log_dir: PathBuf,
        event_tx: mpsc::Sender<Event>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            log_dir,
            additional_files: Vec::new(),
            event_tx,
            cancel_token,
        }
    }

    /// Add additional log files to watch (paths relative to project root)
    pub fn with_additional_files(mut self, files: Vec<PathBuf>) -> Self {
        self.additional_files = files;
        self
    }

    /// Start watching the log directory.
    ///
    /// On startup:
    /// - Scans `storage/logs/` for all `.log` files and emits `LogFilesChanged`.
    /// - Only sets up tail-follow (incremental reading) for `laravel.log` and
    ///   any `additional_files` — NOT for every `.log` file.
    ///
    /// At runtime:
    /// - When a new `.log` file is created, re-scans and emits `LogFilesChanged`.
    /// - Continues to tail `laravel.log` and `additional_files` for `LogUpdate`.
    pub async fn watch(self) -> Result<()> {
        let log_dir = self.log_dir.clone();
        let additional_files = self.additional_files.clone();
        let event_tx = self.event_tx.clone();
        let cancel_token = self.cancel_token.clone();

        // Create channel for file system events
        let (fs_tx, mut fs_rx) = mpsc::channel::<notify::Result<notify::Event>>(100);

        // Create watcher
        let mut watcher = RecommendedWatcher::new(
            move |res| {
                let _ = fs_tx.blocking_send(res);
            },
            Config::default(),
        )?;

        // Watch the log directory
        watcher.watch(&log_dir, RecursiveMode::NonRecursive)?;

        // Watch parent directories of additional files
        let mut watched_dirs: HashSet<PathBuf> = HashSet::new();
        watched_dirs.insert(log_dir.clone());

        // Track which specific files we're watching (for additional files)
        let watched_files: HashSet<PathBuf> = additional_files.iter().cloned().collect();

        for file in &additional_files {
            if let Some(parent) = file.parent() {
                if parent.exists()
                    && !watched_dirs.contains(parent)
                    && watcher.watch(parent, RecursiveMode::NonRecursive).is_ok()
                {
                    watched_dirs.insert(parent.to_path_buf());
                }
            }
        }

        // Track file positions for incremental reading (keyed by file path)
        let mut file_positions: std::collections::HashMap<PathBuf, u64> =
            std::collections::HashMap::new();

        // Scan for all .log files and emit LogFilesChanged on startup
        let mut known_log_files: HashSet<String> = HashSet::new();
        if let Ok(dir_entries) = std::fs::read_dir(&log_dir) {
            for dir_entry in dir_entries.filter_map(|e| e.ok()) {
                let path = dir_entry.path();
                if path.extension().map(|ext| ext == "log").unwrap_or(false) {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        known_log_files.insert(name.to_string());
                    }
                }
            }
        }
        {
            let mut filenames: Vec<String> = known_log_files.iter().cloned().collect();
            filenames.sort();
            let _ = event_tx.send(Event::LogFilesChanged(filenames)).await;
        }

        // Only tail-follow laravel.log (not all .log files)
        let laravel_log = log_dir.join("laravel.log");
        if laravel_log.exists() {
            let file_name = "laravel.log".to_string();

            // Read last 5 lines as initial history
            if let Ok(recent_lines) = read_last_n_lines(&laravel_log, 5) {
                if !recent_lines.is_empty() {
                    let entries: Vec<LogEntry> = recent_lines
                        .into_iter()
                        .map(|content| LogEntry {
                            content,
                            file: file_name.clone(),
                        })
                        .collect();
                    let _ = event_tx.send(Event::LogUpdate(entries)).await;
                }
            }
            // Set position to end of file for future reads
            let pos = std::fs::metadata(&laravel_log)
                .map(|m| m.len())
                .unwrap_or(0);
            file_positions.insert(laravel_log.clone(), pos);
        }

        // Initialize positions for additional files
        for file in &additional_files {
            if file.exists() {
                let file_name = file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();

                // Read last 5 lines as initial history
                if let Ok(recent_lines) = read_last_n_lines(file, 5) {
                    if !recent_lines.is_empty() {
                        let entries: Vec<LogEntry> = recent_lines
                            .into_iter()
                            .map(|content| LogEntry {
                                content,
                                file: file_name.clone(),
                            })
                            .collect();
                        let _ = event_tx.send(Event::LogUpdate(entries)).await;
                    }
                }
                // Set position to end of file for future reads
                let pos = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
                file_positions.insert(file.clone(), pos);
            }
        }

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    break;
                }
                Some(event) = fs_rx.recv() => {
                    if let Ok(event) = event {
                        for path in event.paths.iter() {
                            let is_log_file = path.extension().map(|ext| ext == "log").unwrap_or(false);
                            let in_log_dir = path.parent() == Some(log_dir.as_path());
                            let is_watched_file = watched_files.contains(path);

                            // When a new .log file is created in the log dir, re-scan and emit LogFilesChanged
                            if is_log_file
                                && in_log_dir
                                && matches!(event.kind, notify::EventKind::Create(_))
                            {
                                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                                    if known_log_files.insert(name.to_string()) {
                                        // New file appeared — re-emit file list
                                        let mut filenames: Vec<String> =
                                            known_log_files.iter().cloned().collect();
                                        filenames.sort();
                                        let _ = event_tx
                                            .send(Event::LogFilesChanged(filenames))
                                            .await;
                                    }
                                }
                            }

                            // Only tail-follow laravel.log (in log_dir) or additional watched files
                            let is_laravel_log = in_log_dir
                                && path.file_name().and_then(|n| n.to_str()) == Some("laravel.log");
                            let should_tail = is_laravel_log || is_watched_file;

                            if should_tail
                                && matches!(
                                    event.kind,
                                    notify::EventKind::Modify(_) | notify::EventKind::Create(_)
                                )
                            {
                                let file_name = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("unknown")
                                    .to_string();

                                // Get or create position tracker for this file
                                let last_pos = file_positions.entry(path.clone()).or_insert(0);

                                // Read new content
                                if let Ok(new_lines) = read_new_lines(path, last_pos) {
                                    if !new_lines.is_empty() {
                                        let entries: Vec<LogEntry> = new_lines
                                            .into_iter()
                                            .map(|content| LogEntry {
                                                content,
                                                file: file_name.clone(),
                                            })
                                            .collect();
                                        let _ = event_tx.send(Event::LogUpdate(entries)).await;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Read the last N non-empty lines from a file
fn read_last_n_lines(path: &PathBuf, n: usize) -> std::io::Result<Vec<String>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    // Read all lines, keeping only non-empty ones
    let lines: Vec<String> = reader
        .lines()
        .map_while(|l| l.ok())
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    // Return last N lines
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].to_vec())
}

/// Read new lines from file starting at given position
fn read_new_lines(path: &PathBuf, last_pos: &mut u64) -> std::io::Result<Vec<String>> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let current_size = metadata.len();

    // File was truncated (rotated), start from beginning
    if current_size < *last_pos {
        *last_pos = 0;
    }

    // No new content
    if current_size == *last_pos {
        return Ok(Vec::new());
    }

    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(*last_pos))?;

    let mut lines = Vec::new();
    let mut line = String::new();

    while reader.read_line(&mut line)? > 0 {
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            lines.push(trimmed.to_string());
        }
        line.clear();
    }

    *last_pos = current_size;
    Ok(lines)
}

/// Find Laravel log directory in project
pub fn find_log_dir(working_dir: &std::path::Path) -> Option<PathBuf> {
    let logs_dir = working_dir.join("storage/logs");
    if logs_dir.exists() && logs_dir.is_dir() {
        Some(logs_dir)
    } else {
        None
    }
}

/// Read the last `max_entries` parsed log entries from a file (static mode).
///
/// Returns `(entries, skipped)` where `skipped` is the number of entries
/// that were parsed but dropped because they exceeded `max_entries`.
/// This is called synchronously (intended to run inside `spawn_blocking`).
#[allow(dead_code)] // Will be used by Task 6 (event loop wiring)
pub fn read_static_file(
    path: &Path,
    max_entries: usize,
) -> std::io::Result<(Vec<crate::log::entry::LogEntry>, usize)> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let mut parser = LogEntryParser::new();
    for line in reader.lines() {
        let line = line?;
        parser.feed(&line);
    }
    let all_entries = parser.flush();

    let total = all_entries.len();
    let skipped = total.saturating_sub(max_entries);
    let kept = all_entries.into_iter().skip(skipped).collect();

    Ok((kept, skipped))
}
