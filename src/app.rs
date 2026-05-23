#![allow(dead_code)]

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::PathBuf;

use crate::config::LaramuxConfig;
use crate::log::entry::LogEntry as ParsedLogEntry;
use crate::process::types::{
    OutputLine, Process, ProcessConfig, ProcessId, ProcessRegistry, ProcessStatus,
};
use crate::process::{FullArtisanCommand, QualityTool};
use crate::ui::tabs::Tab;

/// System resource statistics
#[derive(Debug, Clone, Default)]
pub struct SystemStats {
    /// Overall CPU usage percentage (0-100)
    pub cpu_usage: f32,
    /// Overall memory usage percentage (0-100)
    pub memory_usage: f32,
    /// Total memory in bytes
    pub total_memory: u64,
    /// Used memory in bytes
    pub used_memory: u64,
    /// Per-process stats keyed by PID
    pub process_stats: HashMap<u32, ProcessStats>,
}

/// Per-process resource statistics
#[derive(Debug, Clone, Default)]
pub struct ProcessStats {
    /// CPU usage percentage for this process
    pub cpu_usage: f32,
    /// Memory usage in bytes for this process
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Notice,
    Warning,
    Error,
    Critical,
    Alert,
    Emergency,
    Unknown,
}

impl LogLevel {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "debug" => LogLevel::Debug,
            "info" => LogLevel::Info,
            "notice" => LogLevel::Notice,
            "warning" => LogLevel::Warning,
            "error" => LogLevel::Error,
            "critical" => LogLevel::Critical,
            "alert" => LogLevel::Alert,
            "emergency" => LogLevel::Emergency,
            _ => LogLevel::Unknown,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(
            self,
            LogLevel::Error | LogLevel::Critical | LogLevel::Alert | LogLevel::Emergency
        )
    }

    /// Get display name for the level
    pub fn name(&self) -> &'static str {
        match self {
            LogLevel::Debug => "Debug",
            LogLevel::Info => "Info",
            LogLevel::Notice => "Notice",
            LogLevel::Warning => "Warning",
            LogLevel::Error => "Error",
            LogLevel::Critical => "Critical",
            LogLevel::Alert => "Alert",
            LogLevel::Emergency => "Emergency",
            LogLevel::Unknown => "Unknown",
        }
    }

    /// All log levels for filtering
    pub fn all() -> &'static [LogLevel] {
        &[
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Notice,
            LogLevel::Warning,
            LogLevel::Error,
            LogLevel::Critical,
            LogLevel::Alert,
            LogLevel::Emergency,
        ]
    }

    /// Get next filter level (cycles through)
    pub fn next_filter(&self) -> Option<LogLevel> {
        match self {
            LogLevel::Debug => Some(LogLevel::Info),
            LogLevel::Info => Some(LogLevel::Notice),
            LogLevel::Notice => Some(LogLevel::Warning),
            LogLevel::Warning => Some(LogLevel::Error),
            LogLevel::Error => Some(LogLevel::Critical),
            LogLevel::Critical => None, // Return to "All"
            _ => Some(LogLevel::Debug),
        }
    }
}

// ============================================================================
// Processes Tab State
// ============================================================================

/// View mode for the Processes tab
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcessesView {
    #[default]
    List,
    Output,
}

/// State for the Processes tab
#[derive(Debug, Default)]
pub struct ProcessesTabState {
    pub view: ProcessesView,
    pub selected_index: usize,
    pub output_scroll_offset: usize,
}

impl ProcessesTabState {
    pub fn is_output_view(&self) -> bool {
        self.view == ProcessesView::Output
    }

    pub fn toggle_view(&mut self) {
        self.view = match self.view {
            ProcessesView::List => ProcessesView::Output,
            ProcessesView::Output => ProcessesView::List,
        };
    }
}

// ============================================================================
// Logs Tab State
// ============================================================================

/// Which pane has focus in the logs tab
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogsPaneFocus {
    FileTree,
    #[default]
    Entries,
}

/// View mode for a log file
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogViewMode {
    /// Live-tailing mode (only for laravel.log)
    Live,
    /// Static file viewing mode (all other files)
    Static,
}

impl LogViewMode {
    /// Determine the view mode for a given filename
    pub fn for_file(filename: &str) -> Self {
        if filename == "laravel.log" {
            LogViewMode::Live
        } else {
            LogViewMode::Static
        }
    }

    /// Display indicator for the view mode
    pub fn indicator(&self) -> &'static str {
        match self {
            LogViewMode::Live => "⏺ LIVE",
            LogViewMode::Static => "⏸ STATIC",
        }
    }
}

/// Extract YYYY-MM-DD date from a log filename like `auth-2026-05-22.log`
pub fn extract_date_from_filename(filename: &str) -> Option<&str> {
    let name = filename.strip_suffix(".log").unwrap_or(filename);
    // Look for a YYYY-MM-DD pattern anywhere in the filename
    let bytes = name.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    for i in 0..=bytes.len() - 10 {
        if bytes[i].is_ascii_digit()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
            && bytes[i + 3].is_ascii_digit()
            && bytes[i + 4] == b'-'
            && bytes[i + 5].is_ascii_digit()
            && bytes[i + 6].is_ascii_digit()
            && bytes[i + 7] == b'-'
            && bytes[i + 8].is_ascii_digit()
            && bytes[i + 9].is_ascii_digit()
        {
            return Some(&name[i..i + 10]);
        }
    }
    None
}

/// Flat sorted list of log files
#[derive(Debug, Default)]
pub struct LogFileTree {
    pub files: Vec<String>,
    pub selected_index: usize,
}

impl LogFileTree {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            selected_index: 0,
        }
    }

    /// Update the file list with proper sorting:
    /// 1. `laravel.log` always first
    /// 2. Files with dates sorted descending (most recent first)
    /// 3. Files without dates sorted alphabetically
    pub fn update_files(&mut self, mut files: Vec<String>) {
        files.sort_by(|a, b| {
            let a_is_laravel = a == "laravel.log";
            let b_is_laravel = b == "laravel.log";

            if a_is_laravel && !b_is_laravel {
                return std::cmp::Ordering::Less;
            }
            if !a_is_laravel && b_is_laravel {
                return std::cmp::Ordering::Greater;
            }
            if a_is_laravel && b_is_laravel {
                return std::cmp::Ordering::Equal;
            }

            let date_a = extract_date_from_filename(a);
            let date_b = extract_date_from_filename(b);

            match (date_a, date_b) {
                (Some(da), Some(db)) => {
                    // Descending by date, then alphabetical for same date
                    match db.cmp(da) {
                        std::cmp::Ordering::Equal => a.cmp(b),
                        ord => ord,
                    }
                }
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.cmp(b),
            }
        });

        self.files = files;

        if self.selected_index >= self.files.len() {
            self.selected_index = 0;
        }
    }

    /// Get the currently selected filename
    pub fn selected_file(&self) -> Option<&str> {
        self.files.get(self.selected_index).map(|s| s.as_str())
    }

    /// Move selection to the next file
    pub fn select_next(&mut self) {
        if !self.files.is_empty() && self.selected_index < self.files.len() - 1 {
            self.selected_index += 1;
        }
    }

    /// Move selection to the previous file
    pub fn select_previous(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }
}

/// State for the Logs tab
#[derive(Debug)]
pub struct LogsTabState {
    /// File tree for log file navigation
    pub file_tree: LogFileTree,
    /// Currently active log file
    pub active_file: Option<String>,
    /// View mode for the active file
    pub view_mode: Option<LogViewMode>,
    /// Parsed log entries for the active file
    pub entries: VecDeque<ParsedLogEntry>,
    /// Maximum number of entries to keep
    pub max_entries: usize,
    /// Currently selected entry index
    pub selected_entry: usize,
    /// Set of expanded entry indices
    pub expanded_entries: HashSet<usize>,
    /// Scroll offset for the entries pane
    pub scroll_offset: usize,
    /// Search query text
    pub search_query: String,
    /// Log level filter
    pub filter_level: Option<LogLevel>,
    /// Whether search input mode is active
    pub input_mode: bool,
    /// Which pane has focus
    pub focus: LogsPaneFocus,
}

impl Default for LogsTabState {
    fn default() -> Self {
        Self::new()
    }
}

impl LogsTabState {
    pub fn new() -> Self {
        Self {
            file_tree: LogFileTree::new(),
            active_file: None,
            view_mode: None,
            entries: VecDeque::with_capacity(1000),
            max_entries: 1000,
            selected_entry: 0,
            expanded_entries: HashSet::new(),
            scroll_offset: 0,
            search_query: String::new(),
            filter_level: None,
            input_mode: false,
            focus: LogsPaneFocus::default(),
        }
    }

    /// Add a parsed log entry, respecting max_entries limit.
    /// When an entry is popped from the front, adjusts expanded_entries indices
    /// and selected_entry accordingly.
    pub fn add_entry(&mut self, entry: ParsedLogEntry) {
        if self.entries.len() >= self.max_entries {
            self.entries.pop_front();

            // Shift all expanded indices down by 1, removing index 0
            self.expanded_entries = self
                .expanded_entries
                .iter()
                .filter_map(|&idx| if idx > 0 { Some(idx - 1) } else { None })
                .collect();

            // Adjust selected_entry
            self.selected_entry = self.selected_entry.saturating_sub(1);
        }
        self.entries.push_back(entry);
    }

    /// Toggle expand/collapse for the currently selected entry.
    /// No-op if the entry has no expandable content.
    pub fn toggle_expand(&mut self) {
        if let Some(entry) = self.entries.get(self.selected_entry) {
            if !entry.has_expandable_content() {
                return;
            }
        } else {
            return;
        }

        if self.expanded_entries.contains(&self.selected_entry) {
            self.expanded_entries.remove(&self.selected_entry);
        } else {
            self.expanded_entries.insert(self.selected_entry);
        }
    }

    /// Expand all entries that have expandable content
    pub fn expand_all(&mut self) {
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.has_expandable_content() {
                self.expanded_entries.insert(i);
            }
        }
    }

    /// Collapse all entries
    pub fn collapse_all(&mut self) {
        self.expanded_entries.clear();
    }

    /// Move cursor to the next entry in the filtered list
    pub fn select_next_entry(&mut self) {
        let indices = self.filtered_entry_indices();
        if let Some(pos) = indices.iter().position(|&idx| idx >= self.selected_entry) {
            if indices[pos] == self.selected_entry {
                // Currently on a filtered entry, move to next
                if pos + 1 < indices.len() {
                    self.selected_entry = indices[pos + 1];
                }
            } else {
                // Not on a filtered entry, snap to this one
                self.selected_entry = indices[pos];
            }
        }
    }

    /// Move cursor to the previous entry in the filtered list
    pub fn select_previous_entry(&mut self) {
        let indices = self.filtered_entry_indices();
        if let Some(pos) = indices.iter().rposition(|&idx| idx <= self.selected_entry) {
            if indices[pos] == self.selected_entry {
                // Currently on a filtered entry, move to previous
                if pos > 0 {
                    self.selected_entry = indices[pos - 1];
                }
            } else {
                // Not on a filtered entry, snap to this one
                self.selected_entry = indices[pos];
            }
        }
    }

    /// Jump cursor to the first filtered entry
    pub fn jump_to_top(&mut self) {
        let indices = self.filtered_entry_indices();
        if let Some(&first) = indices.first() {
            self.selected_entry = first;
        }
    }

    /// Jump cursor to the last filtered entry
    pub fn jump_to_bottom(&mut self) {
        let indices = self.filtered_entry_indices();
        if let Some(&last) = indices.last() {
            self.selected_entry = last;
        }
    }

    /// Reset all filters and selection state
    pub fn reset_filters(&mut self) {
        self.search_query.clear();
        self.filter_level = None;
        self.selected_entry = 0;
        self.expanded_entries.clear();
        self.scroll_offset = 0;
    }

    /// Cycle through log level filters
    pub fn cycle_filter(&mut self) {
        self.filter_level = match self.filter_level {
            None => Some(LogLevel::Debug),
            Some(level) => level.next_filter(),
        };
    }

    /// Get display name for the current filter level
    pub fn filter_name(&self) -> &'static str {
        match self.filter_level {
            None => "All",
            Some(level) => level.name(),
        }
    }

    /// Select a file, setting active_file and view_mode, and resetting entries/filters
    pub fn select_file(&mut self, filename: &str) {
        self.active_file = Some(filename.to_string());
        self.view_mode = Some(LogViewMode::for_file(filename));
        self.entries.clear();
        self.reset_filters();
    }

    /// Return indices of entries matching the current level filter and search query.
    /// Search matches on `entry.message` only (not stacktrace).
    /// Unknown level always passes the level filter.
    pub fn filtered_entry_indices(&self) -> Vec<usize> {
        let query_lower = self.search_query.to_lowercase();

        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                // Level filter: Unknown always passes
                if let Some(min_level) = self.filter_level {
                    if entry.level != LogLevel::Unknown {
                        let level_order = |l: &LogLevel| -> u8 {
                            match l {
                                LogLevel::Debug => 0,
                                LogLevel::Info => 1,
                                LogLevel::Notice => 2,
                                LogLevel::Warning => 3,
                                LogLevel::Error => 4,
                                LogLevel::Critical => 5,
                                LogLevel::Alert => 6,
                                LogLevel::Emergency => 7,
                                LogLevel::Unknown => 0,
                            }
                        };
                        if level_order(&entry.level) < level_order(&min_level) {
                            return false;
                        }
                    }
                }

                // Search filter: match on message only
                if !query_lower.is_empty() && !entry.message.to_lowercase().contains(&query_lower) {
                    return false;
                }

                true
            })
            .map(|(i, _)| i)
            .collect()
    }
}

// ============================================================================
// Artisan Tab State
// ============================================================================

/// A resolved command ready to execute
#[derive(Debug, Clone)]
pub struct ResolvedCommand {
    pub display_name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// State for the Artisan tab
#[derive(Debug, Default)]
pub struct ArtisanTabState {
    pub selected_command: usize,
    pub input_buffer: String,
    pub input_mode: bool,
    pub command_output: VecDeque<OutputLine>,
    pub output_scroll_offset: usize,
    pub running_command: Option<String>,
    pub artisan_commands: Vec<FullArtisanCommand>,
    pub search_query: String,
    pub search_mode: bool,
    pub details_scroll_offset: usize,
}

impl ArtisanTabState {
    fn filtered_commands_with_favorites<'a>(
        &'a self,
        favorites: &[String],
    ) -> Vec<(&'a FullArtisanCommand, bool)> {
        let mut commands: Vec<_> = if self.search_query.is_empty() {
            self.artisan_commands.iter().collect()
        } else {
            let query = self.search_query.to_lowercase();
            self.artisan_commands
                .iter()
                .filter(|cmd| {
                    cmd.name.to_lowercase().contains(&query)
                        || cmd.description.to_lowercase().contains(&query)
                })
                .collect()
        };

        // Sort: favorites first, then alphabetically
        commands.sort_by(|a, b| {
            let a_fav = favorites.contains(&a.name);
            let b_fav = favorites.contains(&b.name);
            match (a_fav, b_fav) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.cmp(&b.name),
            }
        });

        commands
            .into_iter()
            .map(|cmd| (cmd, favorites.contains(&cmd.name)))
            .collect()
    }

    pub fn command_count(&self, favorites: &[String]) -> usize {
        self.filtered_commands_with_favorites(favorites).len()
    }

    pub fn selected_artisan_command(&self, favorites: &[String]) -> Option<&FullArtisanCommand> {
        let filtered = self.filtered_commands_with_favorites(favorites);
        filtered.get(self.selected_command).map(|(cmd, _)| *cmd)
    }

    /// Returns (name, description, is_favorite)
    pub fn current_command_display(&self, favorites: &[String]) -> Vec<(String, String, bool)> {
        self.filtered_commands_with_favorites(favorites)
            .iter()
            .map(|(cmd, is_fav)| (cmd.name.clone(), cmd.description.clone(), *is_fav))
            .collect()
    }

    pub fn selected_command_resolved(
        &self,
        user_args: &str,
        favorites: &[String],
        is_sail: bool,
    ) -> Option<ResolvedCommand> {
        let filtered = self.filtered_commands_with_favorites(favorites);
        let (cmd, _) = filtered.get(self.selected_command)?;

        let (command, mut args) = if is_sail {
            (
                "./vendor/bin/sail".to_string(),
                vec!["artisan".to_string(), cmd.name.clone()],
            )
        } else {
            (
                "php".to_string(),
                vec!["artisan".to_string(), cmd.name.clone()],
            )
        };

        if !user_args.is_empty() {
            for arg in user_args.split_whitespace() {
                args.push(arg.to_string());
            }
        }

        args.push("--ansi".to_string());

        Some(ResolvedCommand {
            display_name: format!("artisan {}", cmd.name),
            command,
            args,
        })
    }

    /// Get the command name for the currently selected command (for toggling favorites)
    pub fn selected_command_name(&self, favorites: &[String]) -> Option<String> {
        let filtered = self.filtered_commands_with_favorites(favorites);
        filtered
            .get(self.selected_command)
            .map(|(cmd, _)| cmd.name.clone())
    }

    pub fn add_output(&mut self, line: OutputLine) {
        if self.command_output.len() >= 1000 {
            self.command_output.pop_front();
        }
        self.command_output.push_back(line);
    }

    pub fn clear_output(&mut self) {
        self.command_output.clear();
        self.output_scroll_offset = 0;
    }
}

// ============================================================================
// Make Tab State
// ============================================================================

/// State for the Make tab
#[derive(Debug, Default)]
pub struct MakeTabState {
    pub selected_command: usize,
    pub input_buffer: String,
    pub input_mode: bool,
    pub command_output: VecDeque<OutputLine>,
    pub output_scroll_offset: usize,
    pub running_command: Option<String>,
    pub make_commands: Vec<FullArtisanCommand>,
    pub search_query: String,
    pub search_mode: bool,
    pub details_scroll_offset: usize,
}

impl MakeTabState {
    fn filtered_commands_with_favorites<'a>(
        &'a self,
        favorites: &[String],
    ) -> Vec<(&'a FullArtisanCommand, bool)> {
        let mut commands: Vec<_> = if self.search_query.is_empty() {
            self.make_commands.iter().collect()
        } else {
            let query = self.search_query.to_lowercase();
            self.make_commands
                .iter()
                .filter(|cmd| {
                    cmd.name.to_lowercase().contains(&query)
                        || cmd.description.to_lowercase().contains(&query)
                })
                .collect()
        };

        // Sort: favorites first, then alphabetically
        commands.sort_by(|a, b| {
            let a_fav = favorites.contains(&a.name);
            let b_fav = favorites.contains(&b.name);
            match (a_fav, b_fav) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.cmp(&b.name),
            }
        });

        commands
            .into_iter()
            .map(|cmd| (cmd, favorites.contains(&cmd.name)))
            .collect()
    }

    pub fn command_count(&self, favorites: &[String]) -> usize {
        self.filtered_commands_with_favorites(favorites).len()
    }

    pub fn selected_make_command(&self, favorites: &[String]) -> Option<&FullArtisanCommand> {
        let filtered = self.filtered_commands_with_favorites(favorites);
        filtered.get(self.selected_command).map(|(cmd, _)| *cmd)
    }

    /// Returns (display_name, full_command, is_favorite)
    pub fn current_command_display(
        &self,
        favorites: &[String],
        is_sail: bool,
    ) -> Vec<(String, String, bool)> {
        self.filtered_commands_with_favorites(favorites)
            .iter()
            .map(|(cmd, is_fav)| {
                let display_name = cmd
                    .name
                    .strip_prefix("make:")
                    .map(|s| {
                        let mut chars = s.chars();
                        match chars.next() {
                            None => String::new(),
                            Some(first) => {
                                first.to_uppercase().collect::<String>() + chars.as_str()
                            }
                        }
                    })
                    .unwrap_or_else(|| cmd.name.clone());
                let full_command = if is_sail {
                    format!("sail artisan {}", cmd.name)
                } else {
                    format!("php artisan {}", cmd.name)
                };
                (display_name, full_command, *is_fav)
            })
            .collect()
    }

    pub fn selected_command_resolved(
        &self,
        user_args: &str,
        favorites: &[String],
        is_sail: bool,
    ) -> Option<ResolvedCommand> {
        let filtered = self.filtered_commands_with_favorites(favorites);
        let (cmd, _) = filtered.get(self.selected_command)?;

        let (command, mut args) = if is_sail {
            (
                "./vendor/bin/sail".to_string(),
                vec!["artisan".to_string(), cmd.name.clone()],
            )
        } else {
            (
                "php".to_string(),
                vec!["artisan".to_string(), cmd.name.clone()],
            )
        };

        if !user_args.is_empty() {
            for arg in user_args.split_whitespace() {
                args.push(arg.to_string());
            }
        }

        args.push("--ansi".to_string());

        let display_name = cmd
            .name
            .strip_prefix("make:")
            .map(|s| {
                let mut chars = s.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                }
            })
            .unwrap_or_else(|| cmd.name.clone());

        Some(ResolvedCommand {
            display_name,
            command,
            args,
        })
    }

    /// Get the command name for the currently selected command (for toggling favorites)
    pub fn selected_command_name(&self, favorites: &[String]) -> Option<String> {
        let filtered = self.filtered_commands_with_favorites(favorites);
        filtered
            .get(self.selected_command)
            .map(|(cmd, _)| cmd.name.clone())
    }

    pub fn add_output(&mut self, line: OutputLine) {
        if self.command_output.len() >= 1000 {
            self.command_output.pop_front();
        }
        self.command_output.push_back(line);
    }

    pub fn clear_output(&mut self) {
        self.command_output.clear();
        self.output_scroll_offset = 0;
    }
}

// ============================================================================
// Quality Tab State
// ============================================================================

/// Category within the Quality tab
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityCategory {
    #[default]
    QualityTools,
    Testing,
}

impl QualityCategory {
    pub fn all() -> &'static [QualityCategory] {
        &[QualityCategory::QualityTools, QualityCategory::Testing]
    }

    pub fn name(&self) -> &'static str {
        match self {
            QualityCategory::QualityTools => "Quality Tools",
            QualityCategory::Testing => "Testing",
        }
    }

    pub fn next(&self) -> QualityCategory {
        match self {
            QualityCategory::QualityTools => QualityCategory::Testing,
            QualityCategory::Testing => QualityCategory::QualityTools,
        }
    }

    pub fn previous(&self) -> QualityCategory {
        self.next()
    }
}

/// State for the Quality tab (quality tools + testing)
#[derive(Debug, Default)]
pub struct QualityTabState {
    pub selected_category: QualityCategory,
    pub selected_tool: usize,
    pub input_buffer: String,
    pub input_mode: bool,
    pub command_output: VecDeque<OutputLine>,
    pub output_scroll_offset: usize,
    pub running_command: Option<String>,
    pub quality_tools: Vec<QualityTool>,
    pub testing_tools: Vec<QualityTool>,
    pub details_scroll_offset: usize,
}

impl QualityTabState {
    pub fn current_tools(&self) -> &[QualityTool] {
        match self.selected_category {
            QualityCategory::QualityTools => &self.quality_tools,
            QualityCategory::Testing => &self.testing_tools,
        }
    }

    pub fn tool_count(&self) -> usize {
        self.current_tools().len()
    }

    pub fn selected_tool_item(&self) -> Option<&QualityTool> {
        self.current_tools().get(self.selected_tool)
    }

    pub fn selected_command_resolved(&self, user_args: &str) -> Option<ResolvedCommand> {
        let tool = self.selected_tool_item()?;

        let (non_flags, flags): (Vec<_>, Vec<_>) =
            tool.args.iter().partition(|arg| !arg.starts_with('-'));

        let mut args: Vec<String> = non_flags.into_iter().cloned().collect();

        if !user_args.is_empty() {
            for arg in user_args.split_whitespace() {
                args.push(arg.to_string());
            }
        }

        args.extend(flags.into_iter().cloned());

        Some(ResolvedCommand {
            display_name: tool.display_name.clone(),
            command: tool.command.clone(),
            args,
        })
    }

    pub fn add_output(&mut self, line: OutputLine) {
        if self.command_output.len() >= 1000 {
            self.command_output.pop_front();
        }
        self.command_output.push_back(line);
    }

    pub fn clear_output(&mut self) {
        self.command_output.clear();
        self.output_scroll_offset = 0;
    }
}

// ============================================================================
// Config Tab State
// ============================================================================

use crate::config::{CustomProcess, CustomTool, OverrideConfig, RestartPolicy};

/// Available configuration sections
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigSection {
    #[default]
    Disabled,
    Overrides,
    Custom,
    Sail,
    Logs,
    QualityDisabledTools,
    QualityCustomTools,
    QualityDefaultArgs,
    ArtisanFavorites,
    MakeFavorites,
}

impl ConfigSection {
    pub fn all() -> &'static [ConfigSection] {
        &[
            ConfigSection::Disabled,
            ConfigSection::Overrides,
            ConfigSection::Custom,
            ConfigSection::Sail,
            ConfigSection::Logs,
            ConfigSection::QualityDisabledTools,
            ConfigSection::QualityCustomTools,
            ConfigSection::QualityDefaultArgs,
            ConfigSection::ArtisanFavorites,
            ConfigSection::MakeFavorites,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            ConfigSection::Disabled => "Disabled",
            ConfigSection::Overrides => "Overrides",
            ConfigSection::Custom => "Custom",
            ConfigSection::Sail => "Sail",
            ConfigSection::Logs => "Logs",
            ConfigSection::QualityDisabledTools => "Disabled Tools",
            ConfigSection::QualityCustomTools => "Custom Tools",
            ConfigSection::QualityDefaultArgs => "Default Args",
            ConfigSection::ArtisanFavorites => "Artisan Favs",
            ConfigSection::MakeFavorites => "Make Favs",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            ConfigSection::Disabled => 0,
            ConfigSection::Overrides => 1,
            ConfigSection::Custom => 2,
            ConfigSection::Sail => 3,
            ConfigSection::Logs => 4,
            ConfigSection::QualityDisabledTools => 5,
            ConfigSection::QualityCustomTools => 6,
            ConfigSection::QualityDefaultArgs => 7,
            ConfigSection::ArtisanFavorites => 8,
            ConfigSection::MakeFavorites => 9,
        }
    }

    pub fn from_index(index: usize) -> Self {
        match index {
            0 => ConfigSection::Disabled,
            1 => ConfigSection::Overrides,
            2 => ConfigSection::Custom,
            3 => ConfigSection::Sail,
            4 => ConfigSection::Logs,
            5 => ConfigSection::QualityDisabledTools,
            6 => ConfigSection::QualityCustomTools,
            7 => ConfigSection::QualityDefaultArgs,
            8 => ConfigSection::ArtisanFavorites,
            9 => ConfigSection::MakeFavorites,
            _ => ConfigSection::Disabled,
        }
    }
}

/// Which panel has focus in the Config tab
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigFocus {
    #[default]
    Sections,
    Details,
}

/// Edit mode for Config tab
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigEditMode {
    #[default]
    Browse,
    EditText,
    SelectOption,
    Confirm,
}

/// Detail view mode - whether viewing item list or item fields
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigDetailView {
    #[default]
    ItemList, // Navigating the list of items
    ItemFields, // Navigating fields within a selected item
}

/// State for the Config tab
#[derive(Debug, Default)]
pub struct ConfigTabState {
    pub config_draft: Option<ConfigDraft>,
    pub section: ConfigSection,
    pub focus: ConfigFocus,
    pub selected_item: usize,
    pub scroll_offset: usize,
    pub edit_mode: ConfigEditMode,
    pub edit_buffer: String,
    pub edit_field: usize,
    pub has_changes: bool,
    pub error: Option<String>,
    pub confirm_delete: Option<usize>,
    pub detail_view: ConfigDetailView,
    /// For enum selection mode - the currently selected option index
    pub enum_selection: usize,
}

impl ConfigTabState {
    pub fn is_editing(&self) -> bool {
        matches!(self.edit_mode, ConfigEditMode::EditText)
    }

    pub fn is_selecting(&self) -> bool {
        matches!(self.edit_mode, ConfigEditMode::SelectOption)
    }

    pub fn is_field_view(&self) -> bool {
        self.detail_view == ConfigDetailView::ItemFields
    }
}

/// Editable copy of disabled flags
#[derive(Debug, Clone, Default)]
pub struct DisabledDraft {
    pub serve: bool,
    pub vite: bool,
    pub queue: bool,
    pub horizon: bool,
    pub reverb: bool,
}

impl DisabledDraft {
    pub fn items(&self) -> [(&'static str, bool); 5] {
        [
            ("Serve", self.serve),
            ("Vite", self.vite),
            ("Queue", self.queue),
            ("Horizon", self.horizon),
            ("Reverb", self.reverb),
        ]
    }

    pub fn toggle(&mut self, index: usize) {
        match index {
            0 => self.serve = !self.serve,
            1 => self.vite = !self.vite,
            2 => self.queue = !self.queue,
            3 => self.horizon = !self.horizon,
            4 => self.reverb = !self.reverb,
            _ => {}
        }
    }
}

/// Editable copy of an override config
#[derive(Debug, Clone, Default)]
pub struct OverrideDraft {
    pub command: String,
    pub args: String,
    pub working_dir: String,
    pub env: Vec<(String, String)>,
    pub restart_policy: RestartPolicy,
}

impl OverrideDraft {
    pub fn from_override(cfg: &OverrideConfig) -> Self {
        Self {
            command: cfg.command.clone().unwrap_or_default(),
            args: cfg.args.clone().map(|a| a.join(" ")).unwrap_or_default(),
            working_dir: cfg.working_dir.clone().unwrap_or_default(),
            env: cfg
                .env
                .clone()
                .map(|e| e.into_iter().collect())
                .unwrap_or_default(),
            restart_policy: cfg.restart_policy.unwrap_or_default(),
        }
    }

    pub fn to_override(&self) -> Option<OverrideConfig> {
        // Only create override if something is actually set
        if self.command.is_empty()
            && self.args.is_empty()
            && self.working_dir.is_empty()
            && self.env.is_empty()
            && self.restart_policy == RestartPolicy::Never
        {
            return None;
        }

        Some(OverrideConfig {
            command: if self.command.is_empty() {
                None
            } else {
                Some(self.command.clone())
            },
            args: if self.args.is_empty() {
                None
            } else {
                Some(self.args.split_whitespace().map(String::from).collect())
            },
            working_dir: if self.working_dir.is_empty() {
                None
            } else {
                Some(self.working_dir.clone())
            },
            env: if self.env.is_empty() {
                None
            } else {
                Some(self.env.iter().cloned().collect())
            },
            restart_policy: if self.restart_policy == RestartPolicy::Never {
                None
            } else {
                Some(self.restart_policy)
            },
        })
    }

    pub fn is_empty(&self) -> bool {
        self.command.is_empty()
            && self.args.is_empty()
            && self.working_dir.is_empty()
            && self.env.is_empty()
            && self.restart_policy == RestartPolicy::Never
    }
}

/// Editable copy of a custom process
#[derive(Debug, Clone, Default)]
pub struct CustomProcessDraft {
    pub name: String,
    pub display_name: String,
    pub command: String,
    pub args: String,
    pub hotkey: String,
    pub enabled: bool,
    pub working_dir: String,
    pub env: Vec<(String, String)>,
    pub restart_policy: RestartPolicy,
}

impl CustomProcessDraft {
    pub fn from_custom(cp: &CustomProcess) -> Self {
        Self {
            name: cp.name.clone(),
            display_name: cp.display_name.clone(),
            command: cp.command.clone(),
            args: cp.args.join(" "),
            hotkey: cp.hotkey.map(|c| c.to_string()).unwrap_or_default(),
            enabled: cp.enabled,
            working_dir: cp.working_dir.clone().unwrap_or_default(),
            env: cp
                .env
                .clone()
                .map(|e| e.into_iter().collect())
                .unwrap_or_default(),
            restart_policy: cp.restart_policy.unwrap_or_default(),
        }
    }

    pub fn to_custom(&self) -> CustomProcess {
        CustomProcess {
            name: self.name.clone(),
            display_name: self.display_name.clone(),
            command: self.command.clone(),
            args: if self.args.is_empty() {
                vec![]
            } else {
                self.args.split_whitespace().map(String::from).collect()
            },
            hotkey: self.hotkey.chars().next(),
            enabled: self.enabled,
            working_dir: if self.working_dir.is_empty() {
                None
            } else {
                Some(self.working_dir.clone())
            },
            env: if self.env.is_empty() {
                None
            } else {
                Some(self.env.iter().cloned().collect())
            },
            restart_policy: if self.restart_policy == RestartPolicy::Never {
                None
            } else {
                Some(self.restart_policy)
            },
        }
    }

    pub fn new() -> Self {
        Self {
            enabled: true,
            ..Default::default()
        }
    }
}

/// Editable copy of a custom tool
#[derive(Debug, Clone, Default)]
pub struct CustomToolDraft {
    pub name: String,
    pub display_name: String,
    pub command: String,
    pub args: String,
    pub category: String,
}

impl CustomToolDraft {
    pub fn from_tool(tool: &CustomTool) -> Self {
        Self {
            name: tool.name.clone(),
            display_name: tool.display_name.clone(),
            command: tool.command.clone(),
            args: tool.args.join(" "),
            category: tool.category.clone(),
        }
    }

    pub fn to_tool(&self) -> CustomTool {
        CustomTool {
            name: self.name.clone(),
            display_name: self.display_name.clone(),
            command: self.command.clone(),
            args: if self.args.is_empty() {
                vec![]
            } else {
                self.args.split_whitespace().map(String::from).collect()
            },
            category: self.category.clone(),
        }
    }

    pub fn new_quality() -> Self {
        Self {
            category: "quality".to_string(),
            ..Default::default()
        }
    }
}

/// Editable copy of logs config
#[derive(Debug, Clone, Default)]
pub struct LogsDraft {
    pub max_lines: String,
    pub files: Vec<String>,
    pub default_filter: String,
}

/// Editable copy of quality config
#[derive(Debug, Clone, Default)]
pub struct QualityDraft {
    pub disabled_tools: Vec<String>,
    pub custom_tools: Vec<CustomToolDraft>,
    pub default_args: Vec<(String, String)>,
}

/// Complete editable copy of the configuration
#[derive(Debug, Clone, Default)]
pub struct ConfigDraft {
    pub sail: Option<bool>,
    pub disabled: DisabledDraft,
    pub overrides: HashMap<String, OverrideDraft>,
    pub custom: Vec<CustomProcessDraft>,
    pub quality: QualityDraft,
    pub logs: LogsDraft,
    pub artisan_favorites: Vec<String>,
    pub make_favorites: Vec<String>,
}

impl ConfigDraft {
    pub fn from_config(config: Option<&LaramuxConfig>) -> Self {
        match config {
            Some(cfg) => Self {
                sail: cfg.sail,
                disabled: DisabledDraft {
                    serve: cfg.disabled.serve,
                    vite: cfg.disabled.vite,
                    queue: cfg.disabled.queue,
                    horizon: cfg.disabled.horizon,
                    reverb: cfg.disabled.reverb,
                },
                overrides: cfg
                    .overrides
                    .iter()
                    .map(|(k, v)| (k.clone(), OverrideDraft::from_override(v)))
                    .collect(),
                custom: cfg
                    .custom
                    .iter()
                    .map(CustomProcessDraft::from_custom)
                    .collect(),
                quality: QualityDraft {
                    disabled_tools: cfg.quality.disabled_tools.clone(),
                    custom_tools: cfg
                        .quality
                        .custom_tools
                        .iter()
                        .map(CustomToolDraft::from_tool)
                        .collect(),
                    default_args: cfg
                        .quality
                        .default_args
                        .iter()
                        .map(|(k, v)| (k.clone(), v.join(" ")))
                        .collect(),
                },
                logs: LogsDraft {
                    max_lines: cfg
                        .logs
                        .max_lines
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                    files: cfg.logs.files.clone().unwrap_or_default(),
                    default_filter: cfg.logs.default_filter.clone().unwrap_or_default(),
                },
                artisan_favorites: cfg.artisan.favorites.clone(),
                make_favorites: cfg.make.favorites.clone(),
            },
            None => Self::default(),
        }
    }

    pub fn to_config(&self) -> LaramuxConfig {
        use crate::config::{ArtisanConfig, DisabledConfig, LogConfig, MakeConfig, QualityConfig};

        LaramuxConfig {
            sail: self.sail,
            disabled: DisabledConfig {
                serve: self.disabled.serve,
                vite: self.disabled.vite,
                queue: self.disabled.queue,
                horizon: self.disabled.horizon,
                reverb: self.disabled.reverb,
            },
            overrides: self
                .overrides
                .iter()
                .filter_map(|(k, v)| v.to_override().map(|o| (k.clone(), o)))
                .collect(),
            custom: self.custom.iter().map(|c| c.to_custom()).collect(),
            quality: QualityConfig {
                disabled_tools: self.quality.disabled_tools.clone(),
                custom_tools: self
                    .quality
                    .custom_tools
                    .iter()
                    .map(|t| t.to_tool())
                    .collect(),
                default_args: self
                    .quality
                    .default_args
                    .iter()
                    .map(|(k, v)| (k.clone(), v.split_whitespace().map(String::from).collect()))
                    .collect(),
            },
            logs: LogConfig {
                max_lines: self.logs.max_lines.parse().ok(),
                files: if self.logs.files.is_empty() {
                    None
                } else {
                    Some(self.logs.files.clone())
                },
                default_filter: if self.logs.default_filter.is_empty() {
                    None
                } else {
                    Some(self.logs.default_filter.clone())
                },
            },
            artisan: ArtisanConfig {
                favorites: self.artisan_favorites.clone(),
            },
            make: MakeConfig {
                favorites: self.make_favorites.clone(),
            },
        }
    }

    // Backward compatibility methods for existing code
    pub fn process_items(&self) -> [(&'static str, bool); 5] {
        self.disabled.items()
    }

    pub fn toggle_item(&mut self, index: usize) {
        self.disabled.toggle(index);
    }

    /// Get override for a process, creating default if none exists
    pub fn get_or_create_override(&mut self, name: &str) -> &mut OverrideDraft {
        if !self.overrides.contains_key(name) {
            self.overrides
                .insert(name.to_string(), OverrideDraft::default());
        }
        self.overrides.get_mut(name).unwrap()
    }

    /// Count of custom processes
    pub fn custom_count(&self) -> usize {
        self.custom.len()
    }
}

// ============================================================================
// Main App State
// ============================================================================

/// The main application state
pub struct App {
    /// Whether Laravel Sail is detected (commands run through Docker)
    pub is_sail: bool,

    /// Currently active tab
    pub active_tab: Tab,

    /// Processes tab state
    pub processes_tab: ProcessesTabState,

    /// Logs tab state
    pub logs_tab: LogsTabState,

    /// Artisan tab state
    pub artisan_tab: ArtisanTabState,

    /// Make tab state
    pub make_tab: MakeTabState,

    /// Quality tab state
    pub quality_tab: QualityTabState,

    /// Config tab state
    pub config_tab: ConfigTabState,

    /// All managed processes
    pub processes: HashMap<ProcessId, Process>,

    /// Order of processes for display
    pub process_order: Vec<ProcessId>,

    /// Working directory (Laravel project root)
    pub working_dir: PathBuf,

    /// Whether the app should quit
    pub should_quit: bool,

    /// Status message to display
    pub status_message: Option<String>,

    /// Process registry for metadata lookup
    pub registry: ProcessRegistry,

    /// Current configuration (if loaded)
    pub config: Option<LaramuxConfig>,

    /// Configuration loading error (if any)
    pub config_error: Option<String>,

    /// System resource statistics
    pub system_stats: SystemStats,
}

impl App {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            is_sail: false,
            active_tab: Tab::default(),
            processes_tab: ProcessesTabState::default(),
            logs_tab: LogsTabState::new(),
            artisan_tab: ArtisanTabState::default(),
            make_tab: MakeTabState::default(),
            quality_tab: QualityTabState::default(),
            config_tab: ConfigTabState::default(),
            processes: HashMap::new(),
            process_order: Vec::new(),
            working_dir,
            should_quit: false,
            status_message: None,
            registry: ProcessRegistry::new(),
            config: None,
            config_error: None,
            system_stats: SystemStats::default(),
        }
    }

    /// Set configuration loading error
    pub fn set_config_error(&mut self, error: String) {
        self.config_error = Some(error);
    }

    /// Set the configuration
    pub fn set_config(&mut self, config: Option<LaramuxConfig>) {
        self.config_tab.config_draft = Some(ConfigDraft::from_config(config.as_ref()));

        // Apply log config
        if let Some(ref cfg) = config {
            // Set max log entries
            self.logs_tab.max_entries = cfg.log_max_lines();

            // Apply default log filter
            if let Some(filter) = cfg.default_log_filter() {
                self.logs_tab.filter_level = Some(LogLevel::from_str(filter));
            }
        }

        self.config = config;
    }

    /// Set the process registry
    pub fn set_registry(&mut self, registry: ProcessRegistry) {
        self.registry = registry;
    }

    /// Set the discovered artisan commands
    pub fn set_artisan_commands(&mut self, commands: Vec<FullArtisanCommand>) {
        self.artisan_tab.artisan_commands = commands;
    }

    /// Set the discovered artisan make commands
    pub fn set_artisan_make_commands(&mut self, commands: Vec<FullArtisanCommand>) {
        self.make_tab.make_commands = commands;
    }

    /// Set the discovered quality tools
    pub fn set_quality_tools(&mut self, tools: Vec<QualityTool>) {
        self.quality_tab.quality_tools = tools;
    }

    /// Set the discovered testing tools
    pub fn set_testing_tools(&mut self, tools: Vec<QualityTool>) {
        self.quality_tab.testing_tools = tools;
    }

    /// Register a process configuration
    pub fn register_process(&mut self, config: ProcessConfig) {
        let id = config.id.clone();
        if !self.process_order.contains(&id) {
            self.process_order.push(id.clone());
        }
        self.processes.insert(id, Process::new(config));
    }

    /// Get the currently selected process (uses processes_tab.selected_index)
    pub fn selected_process(&self) -> Option<&Process> {
        self.process_order
            .get(self.processes_tab.selected_index)
            .and_then(|id| self.processes.get(id))
    }

    /// Get the currently selected process mutably
    pub fn selected_process_mut(&mut self) -> Option<&mut Process> {
        self.process_order
            .get(self.processes_tab.selected_index)
            .and_then(|id| self.processes.get_mut(id))
    }

    /// Get the currently selected process id
    pub fn selected_id(&self) -> Option<&ProcessId> {
        self.process_order.get(self.processes_tab.selected_index)
    }

    /// Move selection up
    pub fn select_previous(&mut self) {
        if !self.process_order.is_empty() && self.processes_tab.selected_index > 0 {
            self.processes_tab.selected_index -= 1;
        }
    }

    /// Move selection down
    pub fn select_next(&mut self) {
        if !self.process_order.is_empty()
            && self.processes_tab.selected_index < self.process_order.len() - 1
        {
            self.processes_tab.selected_index += 1;
        }
    }

    /// Add output to a process
    pub fn add_process_output(&mut self, id: &ProcessId, line: String, is_stderr: bool) {
        if let Some(process) = self.processes.get_mut(id) {
            let output_line = if is_stderr {
                OutputLine::stderr(line)
            } else {
                OutputLine::stdout(line)
            };
            process.add_output(output_line);
        }
    }

    /// Update process status
    pub fn set_process_status(&mut self, id: &ProcessId, status: ProcessStatus) {
        if let Some(process) = self.processes.get_mut(id) {
            process.status = status;
        }
    }

    /// Set process PID
    pub fn set_process_pid(&mut self, id: &ProcessId, pid: Option<u32>) {
        if let Some(process) = self.processes.get_mut(id) {
            process.pid = pid;
        }
    }

    /// Clear all log entries
    pub fn clear_logs(&mut self) {
        self.logs_tab.entries.clear();
        self.logs_tab.expanded_entries.clear();
        self.logs_tab.selected_entry = 0;
        self.logs_tab.scroll_offset = 0;
    }

    /// Clear output for the selected process
    pub fn clear_selected_output(&mut self) {
        if let Some(process) = self.selected_process_mut() {
            process.clear_output();
        }
    }

    /// Set a status message
    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status_message = Some(message.into());
    }

    /// Clear the status message
    pub fn clear_status(&mut self) {
        self.status_message = None;
    }

    /// Request app quit
    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Scroll selected process output up
    pub fn scroll_output_up(&mut self, amount: usize) {
        if let Some(process) = self.selected_process_mut() {
            process.scroll_offset = process.scroll_offset.saturating_add(amount);
        }
    }

    /// Scroll selected process output down
    pub fn scroll_output_down(&mut self, amount: usize) {
        if let Some(process) = self.selected_process_mut() {
            process.scroll_offset = process.scroll_offset.saturating_sub(amount);
        }
    }

    // Tab navigation
    pub fn next_tab(&mut self) {
        self.active_tab = self.active_tab.next();
    }

    pub fn previous_tab(&mut self) {
        self.active_tab = self.active_tab.previous();
    }

    pub fn go_to_tab(&mut self, tab: Tab) {
        self.active_tab = tab;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a ParsedLogEntry with minimal fields
    fn make_entry(level: LogLevel, message: &str) -> ParsedLogEntry {
        ParsedLogEntry {
            timestamp: "2026-05-23 10:00:00".to_string(),
            environment: "local".to_string(),
            level,
            message: message.to_string(),
            payload: None,
            context: None,
            stacktrace: None,
            raw: String::new(),
        }
    }

    /// Helper to create a ParsedLogEntry with expandable content (payload)
    fn make_expandable_entry(level: LogLevel, message: &str) -> ParsedLogEntry {
        ParsedLogEntry {
            timestamp: "2026-05-23 10:00:00".to_string(),
            environment: "local".to_string(),
            level,
            message: message.to_string(),
            payload: Some(r#"{"key":"value"}"#.to_string()),
            context: None,
            stacktrace: None,
            raw: String::new(),
        }
    }

    #[test]
    fn test_file_tree_ordering_laravel_log_first() {
        let mut tree = LogFileTree::new();
        tree.update_files(vec![
            "auth-2026-05-22.log".to_string(),
            "laravel.log".to_string(),
            "queue-2026-05-21.log".to_string(),
        ]);

        assert_eq!(tree.files[0], "laravel.log");
    }

    #[test]
    fn test_file_tree_ordering_by_date_desc() {
        let mut tree = LogFileTree::new();
        tree.update_files(vec![
            "auth-2026-05-20.log".to_string(),
            "laravel.log".to_string(),
            "queue-2026-05-22.log".to_string(),
            "auth-2026-05-22.log".to_string(),
            "queue-2026-05-21.log".to_string(),
            "debug.log".to_string(),
        ]);

        assert_eq!(tree.files[0], "laravel.log");
        // Next: 2026-05-22 files sorted alphabetically
        assert_eq!(tree.files[1], "auth-2026-05-22.log");
        assert_eq!(tree.files[2], "queue-2026-05-22.log");
        // Then 2026-05-21
        assert_eq!(tree.files[3], "queue-2026-05-21.log");
        // Then 2026-05-20
        assert_eq!(tree.files[4], "auth-2026-05-20.log");
        // Files without dates last, alphabetically
        assert_eq!(tree.files[5], "debug.log");
    }

    #[test]
    fn test_entry_expand_collapse() {
        let mut state = LogsTabState::new();

        // Add a non-expandable entry
        state.add_entry(make_entry(LogLevel::Info, "simple message"));
        // Add an expandable entry
        state.add_entry(make_expandable_entry(LogLevel::Error, "error with payload"));

        // Try to toggle non-expandable entry (should be no-op)
        state.selected_entry = 0;
        state.toggle_expand();
        assert!(
            state.expanded_entries.is_empty(),
            "non-expandable entry should not be added"
        );

        // Toggle expandable entry
        state.selected_entry = 1;
        state.toggle_expand();
        assert!(state.expanded_entries.contains(&1));

        // Toggle again to collapse
        state.toggle_expand();
        assert!(!state.expanded_entries.contains(&1));
    }

    #[test]
    fn test_expand_all_collapse_all() {
        let mut state = LogsTabState::new();

        state.add_entry(make_entry(LogLevel::Info, "simple 1"));
        state.add_entry(make_expandable_entry(LogLevel::Error, "expandable 1"));
        state.add_entry(make_entry(LogLevel::Debug, "simple 2"));
        state.add_entry(make_expandable_entry(LogLevel::Warning, "expandable 2"));

        state.expand_all();
        // Only entries 1 and 3 are expandable
        assert_eq!(state.expanded_entries.len(), 2);
        assert!(state.expanded_entries.contains(&1));
        assert!(state.expanded_entries.contains(&3));
        assert!(!state.expanded_entries.contains(&0));
        assert!(!state.expanded_entries.contains(&2));

        state.collapse_all();
        assert!(state.expanded_entries.is_empty());
    }

    #[test]
    fn test_cursor_navigation() {
        let mut state = LogsTabState::new();

        state.add_entry(make_entry(LogLevel::Info, "entry 0"));
        state.add_entry(make_entry(LogLevel::Warning, "entry 1"));
        state.add_entry(make_entry(LogLevel::Error, "entry 2"));

        // Start at 0
        state.selected_entry = 0;

        state.select_next_entry();
        assert_eq!(state.selected_entry, 1);

        state.select_next_entry();
        assert_eq!(state.selected_entry, 2);

        // At the end, should not go further
        state.select_next_entry();
        assert_eq!(state.selected_entry, 2);

        state.select_previous_entry();
        assert_eq!(state.selected_entry, 1);

        state.select_previous_entry();
        assert_eq!(state.selected_entry, 0);

        // At the beginning, should not go further
        state.select_previous_entry();
        assert_eq!(state.selected_entry, 0);

        // Jump to bottom
        state.jump_to_bottom();
        assert_eq!(state.selected_entry, 2);

        // Jump to top
        state.jump_to_top();
        assert_eq!(state.selected_entry, 0);
    }

    #[test]
    fn test_filter_reset_on_file_change() {
        let mut state = LogsTabState::new();

        // Set up some state
        state.search_query = "test".to_string();
        state.filter_level = Some(LogLevel::Error);
        state.selected_entry = 5;
        state.expanded_entries.insert(2);
        state.scroll_offset = 10;
        state.add_entry(make_entry(LogLevel::Info, "old entry"));

        // Select a new file
        state.select_file("auth-2026-05-22.log");

        assert_eq!(state.active_file.as_deref(), Some("auth-2026-05-22.log"));
        assert_eq!(state.view_mode, Some(LogViewMode::Static),);
        assert!(state.entries.is_empty(), "entries should be cleared");
        assert!(state.search_query.is_empty(), "search should be cleared");
        assert!(state.filter_level.is_none(), "filter should be cleared");
        assert_eq!(state.selected_entry, 0);
        assert!(state.expanded_entries.is_empty());
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_live_mode_detection() {
        assert_eq!(LogViewMode::for_file("laravel.log"), LogViewMode::Live);
        assert_eq!(
            LogViewMode::for_file("auth-2026-05-22.log"),
            LogViewMode::Static
        );
        assert_eq!(LogViewMode::for_file("queue.log"), LogViewMode::Static);

        assert_eq!(LogViewMode::Live.indicator(), "⏺ LIVE");
        assert_eq!(LogViewMode::Static.indicator(), "⏸ STATIC");
    }

    #[test]
    fn test_buffer_limit() {
        let mut state = LogsTabState::new();
        state.max_entries = 3;

        state.add_entry(make_entry(LogLevel::Info, "entry 0"));
        state.add_entry(make_expandable_entry(LogLevel::Warning, "entry 1"));
        state.add_entry(make_entry(LogLevel::Error, "entry 2"));

        assert_eq!(state.entries.len(), 3);

        // Expand entry at index 1
        state.selected_entry = 1;
        state.toggle_expand();
        assert!(state.expanded_entries.contains(&1));

        // Set selected_entry to 2
        state.selected_entry = 2;

        // Add one more, should pop front and shift indices
        state.add_entry(make_entry(LogLevel::Debug, "entry 3"));

        assert_eq!(state.entries.len(), 3);
        // Entry 0 was popped. Old entry 1 is now at index 0, old entry 2 at 1, new entry 3 at 2.
        assert_eq!(state.entries[0].message, "entry 1");
        assert_eq!(state.entries[1].message, "entry 2");
        assert_eq!(state.entries[2].message, "entry 3");

        // Expanded index 1 should now be 0
        assert!(state.expanded_entries.contains(&0));
        assert!(!state.expanded_entries.contains(&1));

        // selected_entry was 2, should shift to 1
        assert_eq!(state.selected_entry, 1);
    }

    #[test]
    fn test_extract_date_from_filename() {
        assert_eq!(
            extract_date_from_filename("auth-2026-05-22.log"),
            Some("2026-05-22")
        );
        assert_eq!(
            extract_date_from_filename("queue-worker-2026-01-15.log"),
            Some("2026-01-15")
        );
        assert_eq!(extract_date_from_filename("laravel.log"), None);
        assert_eq!(extract_date_from_filename("debug.log"), None);
    }

    #[test]
    fn test_filtered_entry_indices_level_filter() {
        let mut state = LogsTabState::new();

        state.add_entry(make_entry(LogLevel::Debug, "debug msg"));
        state.add_entry(make_entry(LogLevel::Info, "info msg"));
        state.add_entry(make_entry(LogLevel::Warning, "warning msg"));
        state.add_entry(make_entry(LogLevel::Error, "error msg"));
        state.add_entry(make_entry(LogLevel::Unknown, "unknown msg"));

        // No filter: all pass
        let indices = state.filtered_entry_indices();
        assert_eq!(indices, vec![0, 1, 2, 3, 4]);

        // Filter Warning+: indices 2 (Warning), 3 (Error), 4 (Unknown always passes)
        state.filter_level = Some(LogLevel::Warning);
        let indices = state.filtered_entry_indices();
        assert_eq!(indices, vec![2, 3, 4]);
    }

    #[test]
    fn test_filtered_entry_indices_search() {
        let mut state = LogsTabState::new();

        state.add_entry(make_entry(LogLevel::Info, "User logged in"));
        state.add_entry(make_entry(LogLevel::Info, "Payment processed"));
        state.add_entry(make_entry(LogLevel::Error, "User not found"));

        state.search_query = "user".to_string();
        let indices = state.filtered_entry_indices();
        assert_eq!(indices, vec![0, 2]);
    }

    #[test]
    fn test_file_tree_select_next_previous() {
        let mut tree = LogFileTree::new();
        tree.update_files(vec![
            "laravel.log".to_string(),
            "auth.log".to_string(),
            "queue.log".to_string(),
        ]);

        assert_eq!(tree.selected_index, 0);
        assert_eq!(tree.selected_file(), Some("laravel.log"));

        tree.select_next();
        assert_eq!(tree.selected_index, 1);

        tree.select_next();
        assert_eq!(tree.selected_index, 2);

        // Should not go past the end
        tree.select_next();
        assert_eq!(tree.selected_index, 2);

        tree.select_previous();
        assert_eq!(tree.selected_index, 1);

        tree.select_previous();
        assert_eq!(tree.selected_index, 0);

        // Should not go below 0
        tree.select_previous();
        assert_eq!(tree.selected_index, 0);
    }

    #[test]
    fn test_file_tree_update_resets_out_of_bounds_index() {
        let mut tree = LogFileTree::new();
        tree.update_files(vec![
            "a.log".to_string(),
            "b.log".to_string(),
            "c.log".to_string(),
        ]);
        tree.selected_index = 2;

        // Update with fewer files; index should reset
        tree.update_files(vec!["a.log".to_string()]);
        assert_eq!(tree.selected_index, 0);
    }
}
