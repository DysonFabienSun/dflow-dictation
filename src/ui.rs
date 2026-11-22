use crate::{
    app::{App, Menu, Pane},
    editor::{caret_position, note_rows},
};
use dflow_scratchpad::locale::Locale;
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
const NOTE_UNDERLINE: Style = Style::new().add_modifier(Modifier::UNDERLINED);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let locale = app.locale;
    let screen = frame.area();
    app.note_area = Rect::ZERO;
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
    let available = content.height.saturating_sub(1);
    let mut panel = match app.panes.last() {
        Some(Pane::Menu(menu)) => {
            menu_lines(menu, content.width, available.saturating_sub(1), locale)
        }
        Some(Pane::TextInput { kind, text }) => {
            let (title, hint) = kind.labels(locale);
            vec![
                Line::styled(title, ACCENT),
                Line::raw(clip_end(text, content.width as usize).to_owned()),
                Line::styled(
                    format!(
                        "{hint} · {}",
                        locale.text(
                            "Ctrl+U clear · Enter save · Esc back",
                            "Ctrl+U 清空 · Enter 保存 · Esc 返回"
                        )
                    ),
                    SECONDARY,
                ),
            ]
        }
        Some(Pane::Help) => vec![
            Line::styled(locale.text("Keyboard shortcuts", "键盘快捷键"), ACCENT),
            Line::raw(locale.text(
                "Shift+arrows select · Ctrl+A select all",
                "Shift+方向键 选择 · Ctrl+A 全选",
            )),
            Line::raw(locale.text("Ctrl+Shift+←/→ select by word", "Ctrl+Shift+←/→ 按词选择")),
            Line::raw(locale.text("Ctrl+Z undo · Ctrl+Y redo", "Ctrl+Z 撤销 · Ctrl+Y 重做")),
            Line::raw(locale.text(
                "Ctrl+Space dictate · hold for push-to-talk",
                "Ctrl+Space 听写 · 按住可持续听写",
            )),
            Line::raw(locale.text("Ctrl+C copy · Ctrl+X cut", "Ctrl+C 复制 · Ctrl+X 剪切")),
            Line::raw(locale.text(
                "Esc or Ctrl+/ commands · :h help · :u undo · :q quit",
                "Esc 或 Ctrl+/ 命令列表 · :h 帮助 · :u 撤销 · :q 退出",
            )),
            Line::styled(locale.text("Esc close", "Esc 关闭"), SECONDARY),
        ],
        None => Vec::new(),
    };
    panel.truncate(available.saturating_sub(1) as usize);
    let note_height = available.saturating_sub(panel.len() as u16);
    let note_area = Rect::new(content.x, content.y, content.width, note_height);
    app.note_area = note_area;
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
        let (message, width) = copy_status(locale, count, footer.width);
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

fn copy_status(locale: Locale, count: usize, available: u16) -> (String, u16) {
    let mut message = match locale {
        Locale::English => format!("Copied {count} chars"),
        Locale::SimplifiedChinese => format!("已复制 {count} 个字符"),
    };
    if message.width() > usize::from(available) {
        message = match locale {
            Locale::English => format!("{count} chars"),
            Locale::SimplifiedChinese => format!("{count} 字符"),
        };
    }
    let message = clip_start(&message, usize::from(available));
    (message.to_owned(), message.width() as u16)
}

fn draw_note(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = note_rows(app.editor.text(), usize::from(area.width));
    let (cursor_row, cursor_column) = caret_position(&rows, app.editor.cursor());
    let height = area.height as usize;
    let top = app.top_row.min(rows.len().saturating_sub(height));
    app.top_row = top
        .min(cursor_row)
        .max(cursor_row.saturating_sub(height - 1));
    if app.editor.text().is_empty() && app.panes.is_empty() {
        frame.render_widget(
            Paragraph::new(app.locale.text("Type or dictate a note.", "输入或听写便笺"))
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
        let highlight = app
            .editor
            .selection()
            .map(|range| (range, NOTE_SELECTION))
            .or_else(|| app.editor.underline().map(|range| (range, NOTE_UNDERLINE)));
        if let Some((range, style)) = highlight {
            for (index, row) in visible.iter().enumerate() {
                let columns = if style == NOTE_SELECTION {
                    row.selected_columns(&range)
                } else {
                    row.underlined_columns(&range)
                };
                let start = columns.start.min(usize::from(area.width)) as u16;
                let end = columns.end.min(usize::from(area.width)) as u16;
                frame.buffer_mut().set_style(
                    Rect::new(area.x + start, area.y + index as u16, end - start, 1),
                    style,
                );
            }
        }
    }
    if app.panes.is_empty() {
        frame.set_cursor_position(Position::new(
            area.x + cursor_column.min(usize::from(area.width - 1)) as u16,
            area.y + (cursor_row - app.top_row) as u16,
        ));
    }
}

fn menu_lines(menu: &Menu, width: u16, height: u16, locale: Locale) -> Vec<Line<'static>> {
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
        lines.push(Line::styled(
            locale.text("No matching commands", "没有匹配的命令"),
            SECONDARY,
        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn chinese_footer_and_copy_message_use_display_cells() {
        let locale = Locale::SimplifiedChinese;
        assert_eq!(copy_status(locale, 8, 20), ("已复制 8 个字符".into(), 15));
        assert_eq!(copy_status(locale, 8, 6), ("8 字符".into(), 6));
        assert_eq!(copy_status(locale, 8, 5), ("8 字".into(), 4));
    }

    #[test]
    fn chinese_help_preserves_the_note_in_a_narrow_terminal() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config::load_for(
            directory.path().join("dflow.toml"),
            Some(Locale::SimplifiedChinese),
        );
        let mut app = App::new(config);
        app.editor.insert("便笺内容");
        app.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        for key in "/help".chars().map(KeyCode::Char).chain([KeyCode::Enter]) {
            app.event(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)));
        }
        let mut terminal = Terminal::new(TestBackend::new(28, 9)).unwrap();
        let frame = terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let rows: Vec<String> = frame
            .buffer
            .content
            .chunks(28)
            .map(|row| {
                row.iter()
                    .filter(|cell| cell.symbol() != " ")
                    .map(|cell| cell.symbol())
                    .collect()
            })
            .collect();
        assert_eq!(rows[0], "便笺内容");
        assert!(rows.iter().any(|row| row == "键盘快捷键"));
        assert!(!app.note_area.is_empty());
    }
}
