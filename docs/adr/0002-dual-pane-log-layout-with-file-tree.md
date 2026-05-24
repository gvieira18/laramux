# Dual-pane log layout with File Tree sidebar

The Logs tab uses a two-pane layout: a File Tree sidebar on the left listing all `.log` files from `storage/logs/`, and a LogEntry viewer on the right. This replaces the previous single-pane layout with a cyclic file filter (`F` key).

The cyclic filter required the user to press `F` repeatedly to reach a specific file, with no visibility into what files existed. With Laravel projects commonly having 20-40+ log files across channels (auth, http, llm, whatsapp) and daily rotation, a visible file list with direct navigation is necessary. The File Tree is flat (no grouping/collapsing by channel) — `laravel.log` is always first, remaining files sorted by date descending, then alphabetically by channel name within the same date.

## Consequences

- The sidebar content changes based on active tab: process list for Output tab, File Tree for Logs tab.
- `h/l` keys switch focus between File Tree and log viewer panes.
- The `F` keybinding (cycle file filter) is removed — the File Tree replaces its function.
- File discovery scans `storage/logs/*.log` without parsing the Laravel `logging.php` config (which may contain dynamic PHP expressions).
