use ratatui::{
    prelude::*,
    widgets::{Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};

use crate::app::{App, LogLevel, LogViewMode, LogsPaneFocus};
use crate::log::entry::LogEntry as ParsedLogEntry;
use crate::ui::tabs::log_file_tree;
use crate::ui::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(area);

    log_file_tree::render(frame, horizontal[0], app);
    render_entries(frame, horizontal[1], app);
}

fn level_color(level: &LogLevel) -> Color {
    match level {
        LogLevel::Emergency | LogLevel::Alert | LogLevel::Critical => Theme::LOG_CRITICAL,
        LogLevel::Error => Theme::LOG_ERROR,
        LogLevel::Warning => Theme::LOG_WARNING,
        LogLevel::Notice => Theme::LOG_NOTICE,
        LogLevel::Info => Theme::LOG_INFO,
        LogLevel::Debug => Theme::LOG_DEBUG,
        LogLevel::Unknown => Theme::TEXT_MUTED,
    }
}

fn level_text(level: &LogLevel) -> &'static str {
    match level {
        LogLevel::Emergency => "EMERGENCY",
        LogLevel::Alert => "    ALERT",
        LogLevel::Critical => " CRITICAL",
        LogLevel::Error => "    ERROR",
        LogLevel::Warning => "     WARN",
        LogLevel::Notice => "   NOTICE",
        LogLevel::Info => "     INFO",
        LogLevel::Debug => "    DEBUG",
        LogLevel::Unknown => "  UNKNOWN",
    }
}

fn extract_time(timestamp: &str) -> &str {
    if timestamp.len() >= 19 {
        &timestamp[11..19]
    } else if timestamp.len() >= 8 {
        &timestamp[..8]
    } else {
        timestamp
    }
}

fn build_collapsed_line(
    entry: &ParsedLogEntry,
    is_selected: bool,
    available_width: u16,
) -> Line<'static> {
    let color = level_color(&entry.level);
    let time = extract_time(&entry.timestamp);
    let level = level_text(&entry.level);

    let mut spans = Vec::new();

    if is_selected {
        spans.push(Span::styled("▌ ", Style::default().fg(Theme::ACCENT)));
    } else {
        spans.push(Span::raw("  "));
    }

    spans.push(Span::styled(
        time.to_string(),
        Style::default().fg(Theme::TEXT_MUTED),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        level.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));

    // Right-side indicators
    let mut right_indicators = String::new();
    if entry.payload.is_some() || entry.context.is_some() {
        right_indicators.push_str(" {…}");
    }
    let frame_count = entry.frame_count();
    if frame_count > 0 {
        if !right_indicators.is_empty() {
            right_indicators.push(' ');
        }
        right_indicators.push_str(&format!(" ▶ {} frames", frame_count));
    }

    // Calculate how much space is left for the message
    // 2 (selector) + 8 (time) + 1 (space) + 9 (level) + 1 (space) = 21 prefix chars
    let prefix_len: u16 = 21;
    let right_len = right_indicators.len() as u16;
    let msg_width = (available_width)
        .saturating_sub(prefix_len)
        .saturating_sub(right_len) as usize;

    let message: String = if entry.message.len() > msg_width && msg_width > 3 {
        format!("{}...", &entry.message[..msg_width.saturating_sub(3)])
    } else {
        entry.message.clone()
    };

    // Pad message to fill available space
    let padded_message = format!("{:<width$}", message, width = msg_width);
    spans.push(Span::styled(
        padded_message,
        Style::default().fg(Theme::TEXT),
    ));

    if !right_indicators.is_empty() {
        spans.push(Span::styled(
            right_indicators,
            Style::default().fg(Theme::TEXT_MUTED),
        ));
    }

    Line::from(spans)
}

fn build_expanded_lines(entry: &ParsedLogEntry, available_width: u16) -> Vec<Line<'static>> {
    let color = level_color(&entry.level);
    let mut lines = Vec::new();

    let has_payload = entry.payload.is_some();
    let has_context = entry.context.is_some();
    let has_both = has_payload && has_context;

    if let Some(ref payload_str) = entry.payload {
        if has_both {
            lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(color)),
                Span::styled(
                    build_section_divider("Payload", available_width.saturating_sub(4) as usize),
                    Style::default().fg(Theme::TEXT_MUTED),
                ),
            ]));
        }
        render_json_lines(&mut lines, payload_str, color, available_width);
    }

    if let Some(ref context_str) = entry.context {
        if has_both {
            lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(color)),
                Span::styled(
                    build_section_divider("Context", available_width.saturating_sub(4) as usize),
                    Style::default().fg(Theme::TEXT_MUTED),
                ),
            ]));
        }
        render_json_lines(&mut lines, context_str, color, available_width);
    }

    if let Some(ref stacktrace) = entry.stacktrace {
        if !stacktrace.exception_summary.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("│   ", Style::default().fg(color)),
                Span::styled(
                    stacktrace.exception_summary.clone(),
                    Style::default().fg(Theme::TEXT),
                ),
            ]));
        }

        if !stacktrace.frames.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                "│",
                Style::default().fg(color),
            )]));
            for frame_line in &stacktrace.frames {
                lines.push(Line::from(vec![
                    Span::styled("│   ", Style::default().fg(color)),
                    Span::styled(frame_line.clone(), Style::default().fg(Theme::TEXT_DIM)),
                ]));
            }
        }
    }

    lines
}

fn build_section_divider(label: &str, width: usize) -> String {
    let prefix = format!("┄ {} ┄", label);
    let remaining = width.saturating_sub(prefix.len());
    format!("{}{}", prefix, "┄".repeat(remaining))
}

fn render_json_lines(
    lines: &mut Vec<Line<'static>>,
    json_str: &str,
    connector_color: Color,
    _available_width: u16,
) {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(json_str)
    {
        for (key, value) in &map {
            let value_span = format_json_value(value);
            lines.push(Line::from(vec![
                Span::styled("│   ", Style::default().fg(connector_color)),
                Span::styled(format!("{}: ", key), Style::default().fg(Theme::ACCENT)),
                value_span,
            ]));
        }
    } else {
        lines.push(Line::from(vec![
            Span::styled("│   ", Style::default().fg(connector_color)),
            Span::styled(json_str.to_string(), Style::default().fg(Theme::TEXT)),
        ]));
    }
}

fn format_json_value(value: &serde_json::Value) -> Span<'static> {
    match value {
        serde_json::Value::String(s) => {
            Span::styled(format!("\"{}\"", s), Style::default().fg(Theme::TEXT))
        }
        serde_json::Value::Number(n) => {
            Span::styled(n.to_string(), Style::default().fg(Theme::LOG_NOTICE))
        }
        serde_json::Value::Bool(b) => {
            Span::styled(b.to_string(), Style::default().fg(Theme::LOG_NOTICE))
        }
        serde_json::Value::Null => {
            Span::styled("null".to_string(), Style::default().fg(Theme::TEXT_MUTED))
        }
        other => Span::styled(other.to_string(), Style::default().fg(Theme::TEXT)),
    }
}

fn render_entries(frame: &mut Frame, area: Rect, app: &App) {
    let is_focused = app.logs_tab.focus == LogsPaneFocus::Entries;

    let filtered_indices = app.logs_tab.filtered_entry_indices();
    let entry_count = filtered_indices.len();

    let mode_indicator = app
        .logs_tab
        .view_mode
        .unwrap_or(LogViewMode::Live)
        .indicator();
    let filter_name = app.logs_tab.filter_name();
    let title = format!(
        " Logs [{}] {} | Level: {} ",
        entry_count, mode_indicator, filter_name
    );

    let block = if is_focused {
        Theme::focused_block(&title)
    } else {
        Theme::default_block(&title)
    }
    .padding(Padding::horizontal(1));

    // Account for borders (2) + padding (2) + search bar (1) + footer (1) = 6
    let inner_height = area.height.saturating_sub(6) as usize;
    let content_width = area.width.saturating_sub(6); // borders + padding + scrollbar

    // Build all visible lines with entry-to-line mapping
    let mut all_lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line_start: usize = 0;
    let mut selected_line_count: usize = 1;

    for &entry_idx in &filtered_indices {
        if let Some(entry) = app.logs_tab.entries.get(entry_idx) {
            let is_selected = entry_idx == app.logs_tab.selected_entry;
            let is_expanded = app.logs_tab.expanded_entries.contains(&entry_idx);

            if is_selected {
                selected_line_start = all_lines.len();
            }

            let mut header = build_collapsed_line(entry, is_selected, content_width);
            if is_selected {
                header = header.style(Style::default().bg(Theme::SELECTION_BG));
            }
            all_lines.push(header);

            if is_expanded {
                let expanded = build_expanded_lines(entry, content_width);
                let expanded_count = expanded.len();
                for mut line in expanded {
                    if is_selected {
                        line = line.style(Style::default().bg(Theme::SELECTION_BG));
                    }
                    all_lines.push(line);
                }
                if is_selected {
                    selected_line_count = 1 + expanded_count;
                }
            } else if is_selected {
                selected_line_count = 1;
            }
        }
    }

    let total_lines = all_lines.len();

    // Calculate scroll so the selected entry is visible
    let scroll = if total_lines <= inner_height {
        0u16
    } else {
        let selected_end = selected_line_start + selected_line_count;
        let current_scroll = app.logs_tab.scroll_offset;

        if selected_line_start < current_scroll {
            selected_line_start as u16
        } else if selected_end > current_scroll + inner_height {
            (selected_end.saturating_sub(inner_height)) as u16
        } else {
            current_scroll as u16
        }
    };

    let paragraph = Paragraph::new(all_lines).block(block).scroll((scroll, 0));

    frame.render_widget(paragraph, area);

    // Render scrollbar if content overflows
    if total_lines > inner_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_style(Style::default().fg(Theme::SCROLLBAR_THUMB))
            .track_style(Style::default().fg(Theme::SCROLLBAR_TRACK));

        let max_scroll = total_lines.saturating_sub(inner_height);
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll as usize);

        let scrollbar_area = Rect {
            x: area.x + area.width.saturating_sub(2),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(4),
        };

        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }

    // Search bar
    let search_area = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: 1,
    };

    let search_line = if app.logs_tab.input_mode {
        Line::from(vec![
            Span::styled("Search: ", Style::default().fg(Theme::ACCENT)),
            Span::styled(
                app.logs_tab.search_query.clone(),
                Style::default().fg(Theme::TEXT),
            ),
            Span::styled("█", Style::default().fg(Theme::ACCENT)),
        ])
    } else if !app.logs_tab.search_query.is_empty() {
        Line::from(vec![
            Span::styled("Search: ", Style::default().fg(Theme::TEXT_MUTED)),
            Span::styled(
                format!("\"{}\"", app.logs_tab.search_query),
                Style::default().fg(Theme::TEXT_DIM),
            ),
            Span::styled(" (press / to edit)", Style::default().fg(Theme::TEXT_MUTED)),
        ])
    } else {
        Line::from(vec![])
    };

    if !search_line.spans.is_empty() {
        frame.render_widget(Paragraph::new(search_line), search_area);
    }

    // Footer
    let footer_area = Rect {
        x: area.x + 2,
        y: area.y + area.height.saturating_sub(2),
        width: area.width.saturating_sub(4),
        height: 1,
    };

    let footer = if app.logs_tab.input_mode {
        Line::from(vec![
            Span::styled("[Esc] ", Style::default().fg(Theme::ACCENT)),
            Span::styled("Cancel", Style::default().fg(Theme::TEXT_DIM)),
            Span::raw("  "),
            Span::styled("[Enter] ", Style::default().fg(Theme::ACCENT)),
            Span::styled("Confirm", Style::default().fg(Theme::TEXT_DIM)),
        ])
    } else if is_focused {
        Line::from(vec![
            Span::styled("[j/k] ", Style::default().fg(Theme::ACCENT)),
            Span::styled("Navigate", Style::default().fg(Theme::TEXT_DIM)),
            Span::raw("  "),
            Span::styled("[Enter] ", Style::default().fg(Theme::ACCENT)),
            Span::styled("Expand", Style::default().fg(Theme::TEXT_DIM)),
            Span::raw("  "),
            Span::styled("[e/E] ", Style::default().fg(Theme::ACCENT)),
            Span::styled("Expand/Collapse All", Style::default().fg(Theme::TEXT_DIM)),
            Span::raw("  "),
            Span::styled("[/] ", Style::default().fg(Theme::ACCENT)),
            Span::styled("Search", Style::default().fg(Theme::TEXT_DIM)),
            Span::raw("  "),
            Span::styled("[f] ", Style::default().fg(Theme::ACCENT)),
            Span::styled("Level", Style::default().fg(Theme::TEXT_DIM)),
            Span::raw("  "),
            Span::styled("[g/G] ", Style::default().fg(Theme::ACCENT)),
            Span::styled("Top/Bottom", Style::default().fg(Theme::TEXT_DIM)),
        ])
    } else {
        Line::from(vec![])
    };

    if !footer.spans.is_empty() {
        frame.render_widget(Paragraph::new(footer), footer_area);
    }
}
