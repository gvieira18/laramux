#![allow(dead_code)]

use crate::app::LogLevel;

#[derive(Debug, Clone)]
pub struct Stacktrace {
    pub exception_summary: String,
    pub frames: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub environment: String,
    pub level: LogLevel,
    pub message: String,
    pub payload: Option<String>,
    pub context: Option<String>,
    pub stacktrace: Option<Stacktrace>,
    pub raw: String,
}

impl LogEntry {
    pub fn has_expandable_content(&self) -> bool {
        self.payload.is_some() || self.context.is_some() || self.stacktrace.is_some()
    }

    pub fn frame_count(&self) -> usize {
        self.stacktrace
            .as_ref()
            .map(|st| st.frames.len())
            .unwrap_or(0)
    }
}

pub struct LogEntryParser {
    buffer: Vec<String>,
    complete: Vec<Vec<String>>,
}

impl LogEntryParser {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            complete: Vec::new(),
        }
    }

    pub fn feed(&mut self, text: &str) {
        for line in text.lines() {
            if is_timestamp_line(line) {
                if !self.buffer.is_empty() {
                    let finished = std::mem::take(&mut self.buffer);
                    self.complete.push(finished);
                }
                self.buffer.push(line.to_string());
            } else {
                self.buffer.push(line.to_string());
            }
        }
    }

    pub fn drain_complete(&mut self) -> Vec<LogEntry> {
        let batches = std::mem::take(&mut self.complete);
        batches
            .into_iter()
            .map(|lines| parse_entry(&lines))
            .collect()
    }

    pub fn flush(&mut self) -> Vec<LogEntry> {
        let mut entries = self.drain_complete();
        if !self.buffer.is_empty() {
            let remaining = std::mem::take(&mut self.buffer);
            entries.push(parse_entry(&remaining));
        }
        entries
    }
}

/// Check if a line starts with the Laravel timestamp pattern `[YYYY-MM-DD HH:MM:SS]`
fn is_timestamp_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() >= 22
        && bytes[0] == b'['
        && bytes[5] == b'-'
        && bytes[8] == b'-'
        && bytes[11] == b' '
        && bytes[14] == b':'
        && bytes[17] == b':'
        && bytes[20] == b']'
}

/// Parse a collected group of lines into a LogEntry
fn parse_entry(lines: &[String]) -> LogEntry {
    debug_assert!(!lines.is_empty(), "parse_entry called with empty lines");
    let raw = lines.join("\n");
    let first_line = &lines[0];

    if !is_timestamp_line(first_line) {
        return LogEntry {
            timestamp: String::new(),
            environment: String::new(),
            level: LogLevel::Unknown,
            message: first_line.clone(),
            payload: None,
            context: None,
            stacktrace: None,
            raw,
        };
    }

    // Extract timestamp: bytes 1..20
    let timestamp = first_line[1..20].to_string();

    // After `] `, parse `environment.LEVEL: message {json} {json}`
    let after_bracket = &first_line[22..]; // skip `[...] `

    let (environment, level, rest) = parse_env_level_message(after_bracket);

    // Split rest into message, payload, context
    let (message, payload, context) = extract_json_blocks(rest);

    // Parse stacktrace from continuation lines
    let stacktrace = parse_stacktrace(lines);

    // Extract exception summary from the first line if it contains `[object]` pattern
    let stacktrace = match stacktrace {
        Some(mut st) => {
            if let Some(summary) = extract_exception_summary(first_line) {
                st.exception_summary = summary;
            }
            Some(st)
        }
        None => None,
    };

    LogEntry {
        timestamp,
        environment,
        level,
        message,
        payload,
        context,
        stacktrace,
        raw,
    }
}

/// Parse `environment.LEVEL: rest_of_line` from the text after `] `
fn parse_env_level_message(text: &str) -> (String, LogLevel, &str) {
    if let Some(colon_pos) = text.find(": ") {
        let env_level = &text[..colon_pos];
        let rest = &text[colon_pos + 2..];

        if let Some(dot_pos) = env_level.rfind('.') {
            let environment = env_level[..dot_pos].to_string();
            let level = LogLevel::from_str(&env_level[dot_pos + 1..]);
            return (environment, level, rest);
        }
    }
    (String::new(), LogLevel::Unknown, text)
}

/// Extract message, optional payload (first JSON block), and optional context (second JSON block).
/// JSON block detection handles nested braces and quoted strings with escapes.
fn extract_json_blocks(text: &str) -> (String, Option<String>, Option<String>) {
    let mut json_ranges: Vec<(usize, usize)> = Vec::with_capacity(2);
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len && json_ranges.len() < 2 {
        if bytes[i] == b'{' {
            if let Some(end) = find_json_end(bytes, i) {
                json_ranges.push((i, end + 1));
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }

    match json_ranges.len() {
        0 => (text.trim_end().to_string(), None, None),
        1 => {
            let message = text[..json_ranges[0].0].trim_end().to_string();
            let payload = text[json_ranges[0].0..json_ranges[0].1].to_string();
            (message, Some(payload), None)
        }
        _ => {
            let message = text[..json_ranges[0].0].trim_end().to_string();
            let payload = text[json_ranges[0].0..json_ranges[0].1].to_string();
            let context = text[json_ranges[1].0..json_ranges[1].1].to_string();
            (message, Some(payload), Some(context))
        }
    }
}

/// Find the matching closing `}` for a `{` at position `start`, handling nested braces and quoted strings.
fn find_json_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = start;
    let len = bytes.len();

    while i < len {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'"' => {
                i += 1;
                while i < len {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        break;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parse stacktrace from continuation lines (lines[1..])
fn parse_stacktrace(lines: &[String]) -> Option<Stacktrace> {
    let mut in_stacktrace = false;
    let mut frames = Vec::new();

    for line in lines.iter().skip(1) {
        let trimmed = line.trim();
        if trimmed == "[stacktrace]" {
            in_stacktrace = true;
            continue;
        }
        if in_stacktrace
            && trimmed.starts_with('#')
            && trimmed
                .as_bytes()
                .get(1)
                .map(|b| b.is_ascii_digit())
                .unwrap_or(false)
        {
            frames.push(trimmed.to_string());
        }
    }

    if in_stacktrace && !frames.is_empty() {
        Some(Stacktrace {
            exception_summary: String::new(),
            frames,
        })
    } else {
        None
    }
}

/// Extract exception summary from `[object] (ClassName(...): message at path:line)` pattern
fn extract_exception_summary(line: &str) -> Option<String> {
    let object_start = line.find("[object] (")?;
    let summary_start = object_start + "[object] (".len();
    let after = &line[summary_start..];
    let paren_pos = after.rfind(')')?;
    Some(after[..paren_pos].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_info_no_json() {
        let mut parser = LogEntryParser::new();
        parser.feed("[2024-01-26 10:30:45] local.INFO: User logged in successfully");
        let entries = parser.flush();

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.timestamp, "2024-01-26 10:30:45");
        assert_eq!(entry.environment, "local");
        assert!(matches!(entry.level, LogLevel::Info));
        assert_eq!(entry.message, "User logged in successfully");
        assert!(entry.payload.is_none());
        assert!(entry.context.is_none());
        assert!(entry.stacktrace.is_none());
        assert!(!entry.has_expandable_content());
        assert_eq!(entry.frame_count(), 0);
    }

    #[test]
    fn test_parse_debug_with_single_json() {
        let mut parser = LogEntryParser::new();
        parser.feed(
            r#"[2024-01-26 10:30:45] local.DEBUG: Query executed {"sql":"SELECT * FROM users","time":3.5}"#,
        );
        let entries = parser.flush();

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.timestamp, "2024-01-26 10:30:45");
        assert_eq!(entry.environment, "local");
        assert!(matches!(entry.level, LogLevel::Debug));
        assert_eq!(entry.message, "Query executed");
        assert_eq!(
            entry.payload.as_deref(),
            Some(r#"{"sql":"SELECT * FROM users","time":3.5}"#)
        );
        assert!(entry.context.is_none());
        assert!(entry.has_expandable_content());
    }

    #[test]
    fn test_parse_info_with_dual_json() {
        let mut parser = LogEntryParser::new();
        parser.feed(
            r#"[2024-01-26 10:30:45] production.INFO: Payment processed {"amount":99.99,"currency":"USD"} {"user_id":42,"ip":"127.0.0.1"}"#,
        );
        let entries = parser.flush();

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert!(matches!(entry.level, LogLevel::Info));
        assert_eq!(entry.message, "Payment processed");
        assert_eq!(
            entry.payload.as_deref(),
            Some(r#"{"amount":99.99,"currency":"USD"}"#)
        );
        assert_eq!(
            entry.context.as_deref(),
            Some(r#"{"user_id":42,"ip":"127.0.0.1"}"#)
        );
        assert!(entry.has_expandable_content());
    }

    #[test]
    fn test_parse_error_with_stacktrace() {
        let mut parser = LogEntryParser::new();
        parser.feed(
            "[2024-01-26 10:30:45] local.ERROR: [object] (RuntimeException(code: 0): Something broke at /app/Http/Controller.php:42)\n\
             [stacktrace]\n\
             #0 /app/Http/Controller.php(42): App\\Service->run()\n\
             #1 /vendor/laravel/framework/src/Pipeline.php(128): call()\n\
             #2 {main}"
        );
        let entries = parser.flush();

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert!(matches!(entry.level, LogLevel::Error));
        assert!(entry.stacktrace.is_some());
        let st = entry.stacktrace.as_ref().expect("stacktrace should exist");
        assert_eq!(
            st.exception_summary,
            "RuntimeException(code: 0): Something broke at /app/Http/Controller.php:42"
        );
        assert_eq!(st.frames.len(), 3);
        assert_eq!(entry.frame_count(), 3);
        assert!(entry.has_expandable_content());
    }

    #[test]
    fn test_parse_multiple_entries() {
        let mut parser = LogEntryParser::new();
        parser.feed(
            "[2024-01-26 10:30:45] local.INFO: First message\n\
             [2024-01-26 10:30:46] local.WARNING: Second message",
        );
        let entries = parser.flush();

        assert_eq!(entries.len(), 2);
        assert!(matches!(entries[0].level, LogLevel::Info));
        assert_eq!(entries[0].message, "First message");
        assert!(matches!(entries[1].level, LogLevel::Warning));
        assert_eq!(entries[1].message, "Second message");
    }

    #[test]
    fn test_parser_incremental_feed() {
        let mut parser = LogEntryParser::new();

        parser.feed("[2024-01-26 10:30:45] local.INFO: First message");
        let drained = parser.drain_complete();
        assert_eq!(
            drained.len(),
            0,
            "single buffered entry should not be drained"
        );

        parser.feed("[2024-01-26 10:30:46] local.DEBUG: Second message");
        let drained = parser.drain_complete();
        assert_eq!(drained.len(), 1, "first entry should now be complete");
        assert!(matches!(drained[0].level, LogLevel::Info));
        assert_eq!(drained[0].message, "First message");

        let flushed = parser.flush();
        assert_eq!(flushed.len(), 1, "second entry should be flushed");
        assert!(matches!(flushed[0].level, LogLevel::Debug));
        assert_eq!(flushed[0].message, "Second message");
    }

    #[test]
    fn test_parse_entry_without_timestamp_prefix() {
        let mut parser = LogEntryParser::new();
        parser.feed("some random non-Laravel log line");
        let entries = parser.flush();

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert!(matches!(entry.level, LogLevel::Unknown));
        assert_eq!(entry.message, "some random non-Laravel log line");
        assert!(entry.timestamp.is_empty());
        assert!(entry.environment.is_empty());
    }
}
