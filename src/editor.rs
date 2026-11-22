use crate::transcript::needs_space;
use std::{
    ops::Range,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const EDIT_GROUP_DELAY: Duration = Duration::from_millis(750);

#[derive(Default)]
pub struct NoteRow {
    pub text: String,
    text_width: usize,
    carets: Vec<(usize, usize)>,
    newline: Option<usize>,
}

impl NoteRow {
    pub fn underlined_columns(&self, range: &Range<usize>) -> Range<usize> {
        let columns = self.selected_columns(range);
        columns.start.min(self.text_width)..columns.end.min(self.text_width)
    }

    pub fn selected_columns(&self, selection: &Range<usize>) -> Range<usize> {
        let column = |offset| {
            let index = self.carets.partition_point(|&(caret, _)| caret < offset);
            match self.carets.get(index) {
                Some(&(caret, _)) if Some(caret) == self.newline => self.text_width,
                Some(&(_, column)) => column.min(self.text_width),
                None => self.text_width + usize::from(self.newline.is_some()),
            }
        };
        column(selection.start)..column(selection.end)
    }
}

pub fn note_rows(text: &str, width: usize) -> Vec<NoteRow> {
    let width = width.max(1);
    let mut rows = vec![NoteRow::default()];
    let mut column = 0;
    let mut after_word = false;
    let mut wrap_at = None;
    let mut graphemes = text.grapheme_indices(true);
    while let Some((offset, grapheme)) = graphemes.next() {
        if grapheme == "\n" {
            let row = rows.last_mut().unwrap();
            row.carets.push((offset, column.min(width - 1)));
            row.newline = Some(offset);
            rows.push(NoteRow::default());
            column = 0;
            after_word = false;
            continue;
        }
        if wrap_at == Some(offset) {
            rows.push(NoteRow::default());
            column = 0;
            wrap_at = None;
        }
        let space = wrap_space(grapheme);
        if space && after_word {
            wrap_at = wrapped_word_start(grapheme.width(), graphemes.clone(), width, column);
        }
        after_word = !space;
        let grapheme = if grapheme.width() > width {
            "?"
        } else {
            grapheme
        };
        let cells = grapheme.width();
        if column + cells > width {
            if wrap_at.is_some() {
                // Keep offsets for separators that fall beyond the visible end of the row.
                rows.last_mut().unwrap().carets.push((offset, width));
                column = width;
                continue;
            }
            rows.push(NoteRow::default());
            column = 0;
        }
        let row = rows.last_mut().unwrap();
        row.carets.push((offset, column));
        row.text.push_str(grapheme);
        column += cells;
        row.text_width = column;
    }
    if column == width {
        rows.push(NoteRow::default());
        column = 0;
    }
    rows.last_mut().unwrap().carets.push((text.len(), column));
    rows
}

fn wrap_space(grapheme: &str) -> bool {
    grapheme.chars().next().is_some_and(|ch| {
        ch.is_whitespace() && !matches!(ch, '\n' | '\r' | '\u{a0}' | '\u{2007}' | '\u{202f}')
    })
}

fn wrapped_word_start(
    mut gap: usize,
    graphemes: unicode_segmentation::GraphemeIndices<'_>,
    width: usize,
    column: usize,
) -> Option<usize> {
    let mut lookahead = graphemes.peekable();
    while let Some((_, space)) = lookahead.next_if(|&(_, grapheme)| wrap_space(grapheme)) {
        gap += space.width();
    }
    let &(start, first) = lookahead.peek()?;
    if first == "\n" {
        return None;
    }
    let cells: usize = lookahead
        .take_while(|&(_, grapheme)| grapheme != "\n" && !wrap_space(grapheme))
        .map(|(_, grapheme)| grapheme.width())
        .sum();
    (column + gap + cells.min(width) > width).then_some(start)
}

pub fn caret_position(rows: &[NoteRow], cursor: usize) -> (usize, usize) {
    let row = rows.partition_point(|row| row.carets[0].0 <= cursor) - 1;
    let carets = &rows[row].carets;
    let index = carets.partition_point(|&(offset, _)| offset <= cursor) - 1;
    (row, carets[index].1)
}

#[derive(Clone, Default)]
struct Note {
    text: String,
    cursor: usize,
}

struct Underline {
    range: Range<usize>,
    slash_pending: bool,
}

#[derive(Default)]
pub struct Editor {
    note: Note,
    anchor: Option<usize>,
    underline: Option<Underline>,
    undo: Vec<Note>,
    redo: Vec<Note>,
    group: Option<Note>,
    burst: Option<(BurstKind, Instant)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BurstKind {
    Typing,
    Backspace,
    Delete,
}

pub enum Movement {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    Home,
    End,
    NoteStart,
    NoteEnd,
}

impl Editor {
    pub fn from_note(text: String, cursor: usize) -> Self {
        let cursor = cursor.min(text.len());
        let mut editor = Self {
            note: Note { text, cursor },
            ..Self::default()
        };
        editor.align_cursor();
        editor
    }

    pub fn text(&self) -> &str {
        &self.note.text
    }
    pub fn cursor(&self) -> usize {
        self.note.cursor
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        let cursor = self.cursor();
        (anchor != cursor).then(|| anchor.min(cursor)..anchor.max(cursor))
    }

    pub fn underline(&self) -> Option<Range<usize>> {
        self.underline
            .as_ref()
            .map(|underline| underline.range.clone())
    }

    fn set_underline(&mut self, range: Range<usize>) {
        self.underline = (!range.is_empty()).then_some(Underline {
            range,
            slash_pending: true,
        });
    }

    pub fn take_underline_slash(&mut self) -> bool {
        self.underline
            .as_mut()
            .is_some_and(|underline| std::mem::take(&mut underline.slash_pending))
    }

    pub fn select_all(&mut self) {
        self.end_burst();
        self.anchor = Some(0);
        self.note.cursor = self.text().len();
        self.clear_underline_for_selection();
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    fn clear_underline_for_selection(&mut self) {
        if self.selection().is_some() {
            self.underline = None;
        }
    }

    pub fn point_caret(&mut self, row: usize, column: usize, width: usize, extend: bool) {
        self.end_burst();
        if extend {
            self.anchor.get_or_insert(self.note.cursor);
        } else {
            self.clear_selection();
        }
        let rows = note_rows(self.text(), width);
        self.note.cursor = match rows.get(row) {
            Some(row_layout) if column < row_layout.text_width => {
                let index = row_layout.carets.partition_point(|&(offset, cell)| {
                    Some(offset) != row_layout.newline && cell <= column
                }) - 1;
                row_layout.carets[index].0
            }
            Some(row_layout) => row_layout.newline.unwrap_or_else(|| {
                rows.get(row + 1)
                    .map_or(self.text().len(), |next| next.carets[0].0)
            }),
            None => self.text().len(),
        };
        self.clear_underline_for_selection();
    }

    pub fn move_caret(&mut self, movement: Movement, extend: bool, width: usize) {
        self.end_burst();
        if extend {
            self.anchor.get_or_insert(self.note.cursor);
        } else {
            let selection = self.selection();
            self.clear_selection();
            if let Some(range) = selection {
                match movement {
                    Movement::Left => {
                        self.note.cursor = range.start;
                        return;
                    }
                    Movement::Right => {
                        self.note.cursor = range.end;
                        return;
                    }
                    _ => {}
                }
            }
        }
        match movement {
            Movement::Left => self.left(),
            Movement::Right => self.right(),
            Movement::WordLeft => {
                self.note.cursor = self
                    .text()
                    .unicode_word_indices()
                    .take_while(|&(offset, _)| offset < self.cursor())
                    .last()
                    .map_or(0, |(offset, _)| offset);
                self.align_cursor();
            }
            Movement::WordRight => {
                self.note.cursor = self
                    .text()
                    .unicode_word_indices()
                    .find(|&(offset, _)| offset > self.cursor())
                    .map_or(self.text().len(), |(offset, _)| offset);
                self.align_cursor();
            }
            Movement::Up => self.vertical(false, width),
            Movement::Down => self.vertical(true, width),
            Movement::Home => self.home(),
            Movement::End => self.end(),
            Movement::NoteStart => self.note.cursor = 0,
            Movement::NoteEnd => self.note.cursor = self.text().len(),
        }
        self.clear_underline_for_selection();
    }

    pub fn edit(&mut self, change: impl FnOnce(&mut Self) -> bool) -> bool {
        self.begin();
        let changed = change(self);
        self.finish();
        changed
    }

    pub fn edit_burst(
        &mut self,
        kind: BurstKind,
        now: Instant,
        change: impl FnOnce(&mut Self) -> bool,
    ) -> bool {
        let join = self.burst.is_some_and(|(previous_kind, last_edit)| {
            previous_kind == kind && now.duration_since(last_edit) < EDIT_GROUP_DELAY
        });
        let changed = if join {
            change(self)
        } else {
            self.edit(change)
        };
        if changed {
            self.burst = Some((kind, now));
        }
        changed
    }

    pub fn end_burst(&mut self) {
        self.burst = None;
    }

    pub fn begin(&mut self) {
        debug_assert!(self.group.is_none());
        self.end_burst();
        self.group = Some(self.note.clone());
    }

    pub fn finish(&mut self) {
        if let Some(snapshot) = self.group.take()
            && snapshot.text != self.note.text
        {
            if self.undo.len() >= 100 {
                self.undo.remove(0);
            }
            self.undo.push(snapshot);
            self.redo.clear();
        }
    }

    pub fn undo(&mut self) -> bool {
        self.end_burst();
        if let Some(previous) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.note, previous));
            self.clear_selection();
            self.underline = None;
            return true;
        }
        false
    }

    pub fn redo(&mut self) -> bool {
        self.end_burst();
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.note, next));
            self.clear_selection();
            self.underline = None;
            return true;
        }
        false
    }

    pub fn clear(&mut self) -> bool {
        if self.text().is_empty() {
            return false;
        }
        self.begin();
        self.note = Note::default();
        self.clear_selection();
        self.underline = None;
        self.finish();
        true
    }

    pub fn lowercase(&mut self) -> bool {
        let Some(target) = self.selection().or_else(|| self.underline()) else {
            return false;
        };
        let text = self.text();
        let cursor = self.cursor();
        let inside = cursor.clamp(target.start, target.end);
        let mut lower = String::new();
        let mut new_inside = 0;
        let mut start = target.start;
        let mut push = |range: Range<usize>, preserve: bool| {
            let part = &text[range.clone()];
            let prefix = &text[range.start..inside.clamp(range.start, range.end)];
            if preserve {
                lower.push_str(part);
                new_inside += prefix.len();
            } else {
                lower.push_str(&part.to_lowercase());
                new_inside += prefix.to_lowercase().len();
            }
        };
        for (offset, word) in text.unicode_word_indices() {
            let end = offset + word.len();
            if end <= start {
                continue;
            }
            if offset >= target.end {
                break;
            }
            let mut previous_uppercase = false;
            let preserve = word.graphemes(true).any(|grapheme| {
                let upper = grapheme.starts_with(char::is_uppercase);
                std::mem::replace(&mut previous_uppercase, upper) && upper
            });
            if preserve {
                let protected_start = offset.max(start);
                let protected_end = end.min(target.end);
                push(start..protected_start, false);
                push(protected_start..protected_end, true);
                start = protected_end;
            }
        }
        push(start..target.end, false);
        if lower == text[target.clone()] {
            return false;
        }
        let new_cursor = if cursor >= target.start {
            target.start + new_inside + cursor.saturating_sub(target.end)
        } else {
            cursor
        };
        self.begin();
        self.note.text.replace_range(target, &lower);
        self.note.cursor = new_cursor;
        self.clear_selection();
        self.underline = None;
        self.align_cursor();
        self.finish();
        true
    }

    pub fn insert(&mut self, text: &str) -> bool {
        let range = self.selection().unwrap_or(self.cursor()..self.cursor());
        self.insert_in_range(range, text)
    }

    pub fn insert_incoming(&mut self, text: &str) -> bool {
        let range = self.selection().unwrap_or(self.cursor()..self.cursor());
        self.insert_underlined(range, text)
    }

    pub fn insert_incoming_at(&mut self, offset: usize, text: &str) -> bool {
        self.insert_underlined(offset..offset, text)
    }

    fn insert_underlined(&mut self, range: Range<usize>, text: &str) -> bool {
        let start = range.start;
        let changed = self.insert_in_range(range, text);
        if changed {
            self.set_underline(start..start + text.len());
        }
        changed
    }

    pub fn insert_at(&mut self, offset: usize, text: &str) -> bool {
        self.insert_in_range(offset..offset, text)
    }

    fn insert_in_range(&mut self, range: Range<usize>, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let changed = self.text()[range.clone()] != *text;
        self.clear_selection();
        self.note.cursor = range.start + text.len();
        if changed {
            self.note.text.replace_range(range, text);
            self.underline = None;
        }
        self.align_cursor();
        changed
    }

    pub fn append(&mut self, text: &str) -> bool {
        self.insert_at(self.text().len(), text)
    }

    pub fn replace(&mut self, text: String) -> bool {
        if self.note.text == text {
            return false;
        }
        self.note.cursor = text.len();
        self.note.text = text;
        self.clear_selection();
        self.set_underline(0..self.cursor());
        true
    }

    pub fn transcript(&mut self, range: &mut Range<usize>, text: &str) -> bool {
        let text = text.trim();
        let left = self.text()[..range.start].chars().next_back();
        let right = self.text()[range.end..].chars().next();
        let mut insertion = String::new();
        if needs_space(left, text.chars().next()) {
            insertion.push(' ');
        }
        insertion.push_str(text);
        let advance = insertion.len();
        if needs_space(text.chars().next_back(), right) {
            insertion.push(' ');
        }
        if self.text()[range.clone()] == insertion {
            return false;
        }
        self.note.text.replace_range(range.clone(), &insertion);
        range.end = range.start + insertion.len();
        self.set_underline(range.clone());
        self.note.cursor = range.start + advance;
        self.align_cursor();
        true
    }

    fn align_cursor(&mut self) {
        if self.cursor() == self.text().len() {
            return;
        }
        self.note.cursor = self
            .text()
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|&i| i >= self.cursor())
            .unwrap_or(self.text().len());
    }

    fn left(&mut self) {
        self.note.cursor = self.text()[..self.cursor()]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i);
    }

    pub(crate) fn right(&mut self) {
        self.note.cursor += self.text()[self.cursor()..]
            .graphemes(true)
            .next()
            .map_or(0, str::len);
    }

    pub(crate) fn home(&mut self) {
        self.note.cursor = self.line_start();
    }

    fn end(&mut self) {
        self.note.cursor = self.line_end();
    }

    fn vertical(&mut self, down: bool, width: usize) {
        let rows = note_rows(self.text(), width);
        let (row, column) = caret_position(&rows, self.cursor());
        let column = column.min(width.saturating_sub(1));
        let target = if down {
            rows.get(row + 1)
        } else {
            row.checked_sub(1).and_then(|row| rows.get(row))
        };
        if let Some(row) = target {
            let index = row.carets.partition_point(|&(_, cell)| cell <= column) - 1;
            self.note.cursor = row.carets[index].0;
        }
    }

    pub fn backspace(&mut self) -> bool {
        if let Some(range) = self.selection() {
            return self.erase(range);
        }
        let end = self.cursor();
        self.left();
        self.erase(self.cursor()..end)
    }

    pub fn delete(&mut self) -> bool {
        if let Some(range) = self.selection() {
            return self.erase(range);
        }
        let start = self.cursor();
        self.right();
        self.erase(start..self.cursor())
    }

    fn line_start(&self) -> usize {
        self.text()[..self.cursor()]
            .rfind('\n')
            .map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text()[self.cursor()..]
            .find('\n')
            .map_or(self.text().len(), |i| self.cursor() + i)
    }

    pub fn erase(&mut self, range: Range<usize>) -> bool {
        self.clear_selection();
        if range.is_empty() {
            return false;
        }
        self.note.cursor = range.start;
        self.note.text.replace_range(range, "");
        self.underline = None;
        self.align_cursor();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercase_preserves_acronyms_and_maps_unicode_caret() {
        let mut editor = Editor::from_note("Before After".into(), 7);
        let incoming = "İtem NASA OpenAI APIs ";
        assert!(editor.edit(|editor| editor.insert_incoming(incoming)));
        editor.move_caret(Movement::Right, false, 80);
        assert_eq!(editor.underline(), Some(7..7 + incoming.len()));
        assert!(editor.lowercase());
        assert_eq!(editor.text(), "Before i\u{307}tem NASA OpenAI APIs After");
        assert_eq!(&editor.text()[editor.cursor()..], "fter");
        assert_eq!(editor.underline(), None);
        assert!(editor.undo());
        assert_eq!(editor.text(), format!("Before {incoming}After"));
        assert!(!editor.lowercase());
        assert!(editor.redo());
        assert_eq!(editor.underline(), None);
    }

    #[test]
    fn selection_replaces_underline_and_protects_partially_selected_words() {
        let mut editor = Editor::from_note("OpenAI Before ".into(), 14);
        assert!(editor.edit(|editor| editor.insert_incoming("Incoming")));
        editor.move_caret(Movement::WordLeft, false, 80);
        editor.move_caret(Movement::WordLeft, true, 80);
        editor.move_caret(Movement::WordLeft, true, 80);
        editor.move_caret(Movement::Right, true, 80);
        assert_eq!(editor.selection(), Some(1..14));
        assert_eq!(editor.underline(), None);
        assert!(editor.lowercase());
        assert_eq!(editor.text(), "OpenAI before Incoming");

        assert!(editor.edit(|editor| editor.insert_incoming_at(0, "Another ")));
        assert!(
            editor.edit_burst(BurstKind::Typing, Instant::now(), |editor| editor
                .insert("x"))
        );
        assert_eq!(editor.underline(), None);
    }

    #[test]
    fn typing_bursts_split_after_a_pause_or_a_caret_move() {
        let mut editor = Editor::default();
        let start = Instant::now();
        let shortly_after = start + Duration::from_millis(200);
        let after_pause = shortly_after + EDIT_GROUP_DELAY;

        editor.edit_burst(BurstKind::Typing, start, |e| e.insert("a"));
        editor.edit_burst(BurstKind::Typing, shortly_after, |e| e.insert("b"));
        editor.edit_burst(BurstKind::Typing, after_pause, |e| e.insert("c"));
        editor.move_caret(Movement::Left, false, 80);
        editor.move_caret(Movement::Right, false, 80);
        editor.edit_burst(BurstKind::Typing, after_pause, |e| e.insert("d"));

        for previous in ["abc", "ab", ""] {
            assert!(editor.undo());
            assert_eq!(editor.text(), previous);
        }
        for next in ["ab", "abc", "abcd"] {
            assert!(editor.redo());
            assert_eq!(editor.text(), next);
        }
    }

    #[test]
    fn deletions_group_by_key_and_new_edits_invalidate_redo() {
        let mut editor = Editor::from_note("one界two".into(), "one界".len());
        let start = Instant::now();
        editor.edit_burst(BurstKind::Backspace, start, Editor::backspace);
        editor.edit_burst(BurstKind::Backspace, start, Editor::backspace);
        editor.edit_burst(BurstKind::Delete, start, Editor::delete);
        editor.edit_burst(BurstKind::Delete, start, Editor::delete);
        assert_eq!(editor.text(), "ono");
        assert!(editor.undo());
        assert_eq!(editor.text(), "ontwo");
        assert!(editor.undo());
        assert_eq!(editor.text(), "one界two");
        assert!(editor.redo());
        assert_eq!(editor.text(), "ontwo");

        editor.edit_burst(BurstKind::Typing, start, |e| e.insert("x"));
        assert!(!editor.redo());
        assert_eq!(editor.text(), "onxtwo");
    }

    #[test]
    fn word_wrap_preserves_separators_and_keeps_words_together() {
        let text = "hello  world\nhi 界面好";
        let rows = note_rows(text, 7);
        let visible: Vec<_> = rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(visible, ["hello  ", "world", "hi ", "界面好"]);

        let mut editor = Editor::from_note(text.into(), 0);
        // The first row has no room to display the separating spaces at width 5.
        editor.point_caret(0, 4, 5, false);
        assert_eq!(editor.cursor(), 4);
        editor.point_caret(1, 2, 5, true);
        assert_eq!(&editor.text()[editor.selection().unwrap()], "o  wo");

        let rows = note_rows("hi abcdefghij", 5);
        let visible: Vec<_> = rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(visible, ["hi ", "abcde", "fghij", ""]);
    }

    #[test]
    fn vertical_moves_use_wrapped_rows_and_display_columns() {
        let mut editor = Editor::default();
        editor.insert("ab界cdé\n\nx");
        editor.home();
        editor.vertical(false, 4);
        assert_eq!(&editor.text()[editor.cursor()..], "\nx");
        editor.vertical(false, 4);
        assert_eq!(&editor.text()[editor.cursor()..], "cdé\n\nx");
        editor.right();
        editor.right();
        editor.vertical(false, 4);
        assert_eq!(&editor.text()[editor.cursor()..], "界cdé\n\nx");
        editor.vertical(true, 4);
        assert_eq!(&editor.text()[editor.cursor()..], "é\n\nx");
        editor.vertical(true, 4);
        assert_eq!(&editor.text()[editor.cursor()..], "\nx");
        editor.vertical(false, 12);
        assert_eq!(editor.cursor(), 0);

        editor.replace("abcd".into());
        editor.vertical(false, 4);
        assert_eq!(editor.cursor(), 0);
        editor.vertical(true, 4);
        assert_eq!(editor.cursor(), 4);
    }

    #[test]
    fn selection_uses_grapheme_widths_at_row_ends() {
        let last_letter = "لال".len();
        let newline = "لالا".len();
        for width in [4, 80] {
            let rows = note_rows("لالا\nx", width);
            assert_eq!(rows[0].selected_columns(&(last_letter..newline)), 3..4);
            assert_eq!(rows[0].selected_columns(&(newline..newline + 1)), 4..5);
        }
        let rows = note_rows("لالا", 80);
        assert_eq!(rows[0].selected_columns(&(last_letter..newline)), 3..4);
        assert_eq!(rows[0].selected_columns(&(newline..newline + 1)), 4..4);
    }
}
