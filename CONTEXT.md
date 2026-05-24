# LaraMux

TUI application for managing Laravel development processes in a single terminal. Monitors processes, streams output, and provides structured log viewing.

## Language

### Process Management

**ProcessKind**:
An enumerated type of Laravel development process that LaraMux can spawn and manage (Serve, Vite, Queue, Horizon, Reverb).
_Avoid_: service, daemon, worker (except as a ProcessKind value)

**Discovery**:
The automatic detection of which ProcessKinds are available by inspecting `composer.json` and `package.json`.
_Avoid_: detection, scanning

### Log Viewing

**LogEntry**:
A single logical log event parsed from a Laravel log file. May span multiple lines in the file (message + metadata JSON + stacktrace). Delimited by the `[YYYY-MM-DD HH:MM:SS]` timestamp prefix.
_Avoid_: LogLine, log record, log message (when referring to the full parsed entity)

**Payload**:
The first JSON object in a LogEntry, containing the data explicitly passed by the developer via `Log::info('msg', [...])`.
_Avoid_: context (when referring to the developer-provided data)

**Context**:
The second JSON object in a LogEntry, automatically injected by Laravel middleware (e.g., `user_id`, `tenant_id`). Distinct from Payload.
_Avoid_: extra, metadata (when distinguishing from Payload)

**Live Mode**:
A file viewing mode where new LogEntries are streamed in real-time via tail-follow. Only applies to `laravel.log`. Indicated by `◉ LIVE`.
_Avoid_: streaming mode, watch mode, tail mode

**Static Mode**:
A file viewing mode where a log file is read on-demand with no automatic updates. User presses `r` to reload. Indicated by `◎ STATIC`.
_Avoid_: file mode, read mode, snapshot mode

**File Tree**:
The flat, sorted list of `.log` files in `storage/logs/` displayed in the sidebar. `laravel.log` always appears first, remaining files sorted by date descending.
_Avoid_: file browser, file explorer, directory tree

## Relationships

- A **LogEntry** belongs to exactly one log file
- A **LogEntry** has zero or one **Payload** and zero or one **Context**
- A **LogEntry** may contain a stacktrace (displayed collapsed by default)
- The **File Tree** lists all `.log` files from `storage/logs/`
- **Live Mode** is exclusive to `laravel.log`; all other files use **Static Mode**

## Example dialogue

> **Dev:** "When the user selects a file in the File Tree, does it open in Live Mode?"
> **Domain expert:** "No — only `laravel.log` opens in Live Mode. Everything else opens in Static Mode. The user can reload with `r`."

> **Dev:** "If a LogEntry has two JSON blocks, which one is the Payload?"
> **Domain expert:** "The first one — that's what the developer passed to `Log::info()`. The second is the Context, injected automatically by middleware."

## Flagged ambiguities

- "context" was ambiguous — in Laravel logging, both the developer-provided data and the middleware-injected data could be called "context". Resolved: developer data is **Payload**, middleware data is **Context**.
- "LogLine" vs "LogEntry" — the old codebase used LogLine for individual text lines. Resolved: **LogEntry** is the canonical term for a parsed multi-line log event; LogLine is deprecated.
