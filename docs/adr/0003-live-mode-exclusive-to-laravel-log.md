# Live mode exclusive to laravel.log

Only `laravel.log` supports Live Mode (real-time tail-follow with auto-scroll). All other log files open in Static Mode (read on-demand, manual reload with `r`). There is no toggle to switch arbitrary files into Live Mode.

This was chosen to keep the watcher simple — only one file has an active tail at any time. In practice, `laravel.log` (the `single` driver default) captures all log channels, so developers debugging in real-time look there. Channel-specific daily files (e.g., `auth-2026-05-23.log`) are used for retroactive investigation, not live monitoring.

## Considered Options

- **Any file can toggle Live Mode (`L` key):** More flexible, but adds watcher lifecycle management (start/stop tailing per file), complicates memory model (which file's buffer to keep?), and the use case is rare — if you need live logs, `laravel.log` has everything.

## Consequences

- The watcher only tail-follows `laravel.log`; directory monitoring still runs to detect new files for the File Tree.
- When `laravel.log` doesn't exist (project uses `daily` driver), the fallback opens the most recent `laravel-YYYY-MM-DD.log` in Static Mode.
- Memory model: at most 1000 LogEntries for the live buffer + 1000 for the active static file. Switching files discards the previous static buffer.
