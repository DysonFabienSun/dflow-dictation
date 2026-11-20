use crate::transcript::needs_space;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub struct NoteRow {
    pub text: String,
    carets: Vec<(usize, usize)>,
}

pub fn note_rows(text: &str, width: usize) -> Vec<NoteRow> {
    let width = width.max(1);
    let mut rows = vec![NoteRow::default()];
    let mut column = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        if grapheme == "\n" {
            rows.last_mut()
                .unwrap()
                .carets
                .push((offset, column.min(width - 1)));
            rows.push(NoteRow::default());
            column = 0;
            continue;
        }
        let grapheme = if grapheme.width() > width {
            "?"
        } else {
            grapheme
        };
        let cells = grapheme.width();
        if column + cells > width {
            rows.push(NoteRow::default());
            column = 0;
        }
        let row = rows.last_mut().unwrap();
        row.carets.push((offset, column.min(width - 1)));
        row.text.push_str(grapheme);
        column += cells;
    }
    if column == width {
        rows.push(NoteRow::default());
        column = 0;
    }
    rows.last_mut().unwrap().carets.push((text.len(), column));
    rows
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

#[derive(Default)]
pub struct Editor {
    note: Note,
    undo: Vec<Note>,
    group: Option<Note>,
}

impl Editor {
    pub fn text(&self) -> &str {
        &self.note.text
    }
    pub fn cursor(&self) -> usize {
        self.note.cursor
    }

    pub fn edit(&mut self, change: impl FnOnce(&mut Self) -> bool) -> bool {
        self.begin();
        let changed = change(self);
        self.finish();
        changed
    }

    pub fn begin(&mut self) {
        debug_assert!(self.group.is_none());
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
        }
    }

    pub fn undo(&mut self) -> bool {
        if let Some(previous) = self.undo.pop() {
            self.note = previous;
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
        self.finish();
        true
    }

    pub fn lowercase(&mut self) -> bool {
        let lower = self.text().to_lowercase();
        if lower == self.text() {
            return false;
        }
        self.begin();
        self.note.cursor = self.text()[..self.cursor()].to_lowercase().len();
        self.note.text = lower;
        self.align_cursor();
        self.finish();
        true
    }

    pub fn insert(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        self.note.text.insert_str(self.note.cursor, text);
        self.note.cursor += text.len();
        self.align_cursor();
        true
    }

    pub fn append(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        self.note.cursor = self.text().len();
        self.insert(text)
    }

    pub fn replace(&mut self, text: String) -> bool {
        if self.note.text == text {
            return false;
        }
        self.note.cursor = text.len();
        self.note.text = text;
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

    pub fn left(&mut self) {
        self.note.cursor = self.text()[..self.cursor()]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i);
    }

    pub fn right(&mut self) {
        self.note.cursor += self.text()[self.cursor()..]
            .graphemes(true)
            .next()
            .map_or(0, str::len);
    }

    pub fn home(&mut self) {
        self.note.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.note.cursor = self.line_end();
    }

    pub fn vertical(&mut self, down: bool, width: usize) {
        let rows = note_rows(self.text(), width);
        let (row, column) = caret_position(&rows, self.cursor());
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
        let end = self.cursor();
        self.left();
        self.erase(self.cursor(), end)
    }

    pub fn delete(&mut self) -> bool {
        let start = self.cursor();
        self.right();
        self.erase(start, self.cursor())
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

    fn erase(&mut self, start: usize, end: usize) -> bool {
        if start == end {
            return false;
        }
        self.note.text.replace_range(start..end, "");
        self.note.cursor = start;
        self.align_cursor();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
