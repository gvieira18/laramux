use ratatui::{
    prelude::*,
    widgets::{List, ListItem, Padding},
};

use crate::app::{App, LogsPaneFocus};
use crate::ui::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let is_focused = app.logs_tab.focus == LogsPaneFocus::FileTree;
    let block = if is_focused {
        Theme::focused_block(" Log Files ")
    } else {
        Theme::default_block(" Log Files ")
    }
    .padding(Padding::horizontal(1));

    let active_file = app.logs_tab.active_file.as_deref();
    let file_tree = &app.logs_tab.file_tree;

    let items: Vec<ListItem> = file_tree
        .files
        .iter()
        .enumerate()
        .map(|(idx, filename)| {
            let is_selected = is_focused && idx == file_tree.selected_index;
            let is_active = active_file == Some(filename.as_str());
            let is_live = filename == "laravel.log";

            let mut spans = Vec::new();

            if is_selected {
                spans.push(Span::styled("▌ ", Style::default().fg(Theme::ACCENT)));
            } else {
                spans.push(Span::raw("  "));
            }

            let name_style = if is_active {
                Style::default()
                    .fg(Theme::TEXT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Theme::TEXT_DIM)
            };
            spans.push(Span::styled(filename.clone(), name_style));

            if is_live {
                spans.push(Span::raw(" "));
                spans.push(Span::styled("◉", Style::default().fg(Theme::SUCCESS)));
            }

            let line = Line::from(spans);
            if is_selected {
                ListItem::new(line).style(Style::default().bg(Theme::SELECTION_BG))
            } else {
                ListItem::new(line)
            }
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}
