# Multi-line LogEntry with stateful parser

The log parser accumulates lines into a single LogEntry until it encounters the next `[YYYY-MM-DD HH:MM:SS]` timestamp prefix, rather than treating each line independently. This means the parser is stateful — it buffers lines and emits a complete LogEntry only when the next entry begins (or the file ends).

This was chosen because Laravel's Monolog formatter emits exceptions with real newlines (`allowInlineLineBreaks = true`), producing stack traces that span 20-30+ lines. Treating each line as a separate entry made the log viewer unusable for debugging — errors were scattered across dozens of individual lines with no grouping. The multi-line model enables collapsible entries where the summary line shows the message and the stacktrace/JSON are revealed on demand.

## Considered Options

- **Line-by-line parsing (status quo):** Simpler, but impossible to implement expand/collapse or associate a stacktrace with its error message.
- **Regex-based multi-line grouping at render time:** Would avoid changing the data model but pushes complexity into the UI layer and duplicates parsing logic.

## Consequences

- The `LogLine` struct is replaced by `LogEntry` with fields: timestamp, environment, level, message, payload (optional JSON), context (optional JSON), stacktrace (optional Vec of frames).
- The parser must handle partial entries at file boundaries (e.g., the last entry in a file may not be followed by another timestamp).
- Dual JSON blocks on a single entry line are split into Payload (first) and Context (second).
