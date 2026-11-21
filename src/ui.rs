use crate::{
    app::{App, Menu, Pane},
    editor::{caret_position, note_rows},
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const SECONDARY: Style = Style::new().fg(Color::DarkGray);
const ACCENT: Style = Style::new().fg(Color::Cyan);
const SELECTED: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const NOTE_SELECTION: Style = Style::new().add_modifier(Modifier::REVERSED);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let screen = frame.area();
    if screen.is_empty() {
        return;
    }
    let horizontal = if screen.width > 8 { 1 } else { 0 };
    let content = Rect::new(
        screen.x + horizontal,
        screen.y,
        screen.width - 2 * horizontal,
        screen.height,
    );
    let mut footer = Rect::new(content.x, content.bottom() - 1, content.width, 1);
    app.note_width = usize::from(content.width);
    let available = content.height.saturating_sub(1);
    let mut panel = match app.panes.last() {
        Some(Pane::Menu(menu)) => menu_lines(menu, content.width, available.saturating_sub(1)),
        Some(Pane::TextInput { kind, text }) => {
            let (title, hint) = kind.labels();
            vec![
                Line::styled(title, ACCENT),
                Line::raw(clip_end(text, content.width as usize).to_owned()),
                Line::styled(
                    format!("{hint} · Ctrl+U clear · Enter save · Esc back"),
                    SECONDARY,
                ),
            ]
        }
        Some(Pane::Help) => vec![
            Line::styled("Keyboard shortcuts", ACCENT),
            Line::raw("Shift+arrows select · Ctrl+A select all"),
            Line::raw("Ctrl+Shift+←/→ select by word"),
            Line::raw("Ctrl+Z undo · Ctrl+Y redo"),
            Line::raw("Ctrl+Space dictate · hold for push-to-talk"),
            Line::raw("Ctrl+C copy · Ctrl+X cut"),
            Line::raw("Esc or Ctrl+/ commands · :h help · :u undo · :q quit"),
            Line::styled("Esc close", SECONDARY),
        ],
        None => Vec::new(),
    };
    panel.truncate(available.saturating_sub(1) as usize);
    let note_height = available.saturating_sub(panel.len() as u16);
    let note_area = Rect::new(content.x, content.y, content.width, note_height);
    if !note_area.is_empty() {
        draw_note(frame, app, note_area);
    }
    if !panel.is_empty() {
        frame.render_widget(
            Paragraph::new(panel),
            Rect::new(
                content.x,
                note_area.bottom(),
                content.width,
                available - note_height,
            ),
        );
        if let Some(Pane::TextInput { text, .. }) = app.panes.last()
            && available - note_height > 1
        {
            let width =
                UnicodeWidthStr::width(clip_end(text, content.width.saturating_sub(1) as usize));
            let line = Rect::new(content.x, note_area.bottom() + 1, content.width, 1);
            frame.render_widget(Paragraph::new(clip_end(text, width)), line);
            frame.set_cursor_position(Position::new(content.x + width as u16, line.y));
        } else if let Some(Pane::Menu(menu)) = app.panes.last()
            && let Some(query) = &menu.query
        {
            let column = query.display().width().min(usize::from(content.width - 1));
            frame.set_cursor_position(Position::new(content.x + column as u16, note_area.bottom()));
        }
    }
    let (status, error) = app.status();
    if let Some(count) = app.copied_characters() {
        let message = format!("Copied {count} chars");
        let message = if message.len() <= usize::from(footer.width) {
            message
        } else {
            format!("{count} chars")
        };
        let message = clip_start(&message, usize::from(footer.width));
        let width = message.len() as u16;
        let right = Rect::new(footer.right() - width, footer.y, width, 1);
        frame.render_widget(
            Paragraph::new(message).style(Style::new().fg(Color::Green)),
            right,
        );
        footer.width = footer.width.saturating_sub(width + 2);
    }
    frame.render_widget(
        Paragraph::new(status).style(if error {
            Style::new().fg(Color::Yellow)
        } else {
            Style::default()
        }),
        footer,
    );
}

fn draw_note(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = note_rows(app.editor.text(), app.note_width);
    let (cursor_row, cursor_column) = caret_position(&rows, app.editor.cursor());
    let height = area.height as usize;
    let top = app.top_row.min(rows.len().saturating_sub(height));
    app.top_row = top
        .min(cursor_row)
        .max(cursor_row.saturating_sub(height - 1));
    if app.editor.text().is_empty() && app.panes.is_empty() {
        frame.render_widget(
            Paragraph::new("Type or dictate a note.")
                .style(SECONDARY)
                .wrap(Wrap { trim: false }),
            area,
        );
    } else {
        let visible = &rows[app.top_row..(app.top_row + height).min(rows.len())];
        let lines: Vec<_> = visible
            .iter()
            .map(|row| Line::raw(row.text.as_str()))
            .collect();
        frame.render_widget(Paragraph::new(lines), area);
        if let Some(selection) = app.editor.selection() {
            for (index, row) in visible.iter().enumerate() {
                let columns = row.selected_columns(&selection);
                let start = columns.start.min(usize::from(area.width)) as u16;
                let end = columns.end.min(usize::from(area.width)) as u16;
                frame.buffer_mut().set_style(
                    Rect::new(area.x + start, area.y + index as u16, end - start, 1),
                    NOTE_SELECTION,
                );
            }
        }
    }
    if app.panes.is_empty() {
        frame.set_cursor_position(Position::new(
            area.x + cursor_column as u16,
            area.y + (cursor_row - app.top_row) as u16,
        ));
    }
}

fn menu_lines(menu: &Menu, width: u16, height: u16) -> Vec<Line<'static>> {
    if height == 0 {
        return Vec::new();
    }
    let visible = menu.visible();
    let mut lines = vec![if let Some(query) = &menu.query {
        Line::styled(query.display(), SELECTED)
    } else {
        Line::styled(menu.title, SELECTED)
    }];
    let narrow = width < 55;
    let context_capacity = (height as usize).saturating_sub(2 + usize::from(narrow));
    lines.extend(
        menu.context
            .iter()
            .take(context_capacity)
            .cloned()
            .map(|line| Line::styled(line, SECONDARY)),
    );
    let available = (height as usize)
        .saturating_sub(lines.len() + usize::from(narrow))
        .max(1);
    let first = menu.selected.saturating_sub(available - 1);
    let name_width = visible
        .iter()
        .map(|row| row.name.width())
        .max()
        .unwrap_or(0)
        .min(24);
    for (index, row) in visible.iter().enumerate().skip(first).take(available) {
        let active = index == menu.selected;
        let mut spans = vec![Span::styled(if active { "› " } else { "  " }, ACCENT)];
        let name = clip_start(
            &row.name,
            if narrow {
                width.saturating_sub(2) as usize
            } else {
                name_width
            },
        );
        spans.push(Span::styled(
            name.to_owned(),
            if active { SELECTED } else { Style::default() },
        ));
        if !narrow && !row.detail.is_empty() {
            spans.push(Span::raw(
                " ".repeat(2 + name_width.saturating_sub(name.width())),
            ));
            spans.push(Span::styled(row.detail.clone(), SECONDARY));
        }
        lines.push(Line::from(spans));
    }
    if visible.is_empty() {
        lines.push(Line::styled("No matching commands", SECONDARY));
    }
    if narrow
        && let Some(row) = visible.get(menu.selected)
        && !row.detail.is_empty()
    {
        lines.push(Line::styled(row.detail.clone(), SECONDARY));
    }
    lines.truncate(height as usize);
    lines
}

fn clip_start(text: &str, cells: usize) -> &str {
    let mut width = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        width += grapheme.width();
        if width > cells {
            return &text[..offset];
        }
    }
    text
}

fn clip_end(text: &str, cells: usize) -> &str {
    let mut width = 0;
    for (offset, grapheme) in text.grapheme_indices(true).rev() {
        width += grapheme.width();
        if width > cells {
            return &text[offset + grapheme.len()..];
        }
    }
    text
}
