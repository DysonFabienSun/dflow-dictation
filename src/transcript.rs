use std::iter;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub struct Transcript {
    text: String,
    tail_start: usize,
}

impl Transcript {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn has_tail(&self) -> bool {
        self.text[self.tail_start..]
            .unicode_words()
            .next()
            .is_some()
    }

    pub fn append(&mut self, text: &str) {
        let text = text.trim();
        if needs_space(self.text.chars().next_back(), text.chars().next()) {
            self.text.push(' ');
        }
        self.tail_start = self.text.len();
        self.text.push_str(text);
    }

    pub fn merge(&mut self, text: &str, current: Option<&str>) -> bool {
        let text = text.trim();
        let previous = &self.text[self.tail_start..];
        let mut words = text.unicode_word_indices();
        let mut previous_end = 0;
        let mut first_word_ends = None;
        let mut previous_words = previous.unicode_word_indices().enumerate().peekable();
        while let Some((index, (old_start, old_word))) = previous_words.next() {
            let Some((start, word)) = words.next() else {
                return false;
            };
            // A standalone result can confirm the split when recognition
            // changes the final word after a matching prefix.
            let can_revise = current.is_some() && index >= 2 && previous_words.peek().is_none();
            if !same_word(old_word, word) && !can_revise {
                return false;
            }
            previous_end = start + word.len();
            first_word_ends.get_or_insert((old_start + old_word.len(), previous_end));
        }
        let Some((old_first_end, new_first_end)) = first_word_ends else {
            return false;
        };

        let Some((next_start, first)) = words.next() else {
            return false;
        };
        if let Some(current) = current {
            let suffix = iter::once(first).chain(words.map(|(_, word)| word));
            let mut current_words = current.unicode_words();
            for word in suffix {
                if !current_words
                    .next()
                    .is_some_and(|other| same_word(word, other))
                {
                    return false;
                }
            }
            if current_words.next().is_some() {
                return false;
            }
        }

        let split = {
            // Keep opening punctuation with the following words.
            let opening = text[previous_end..next_start]
                .chars()
                .rev()
                .take_while(|c| {
                    matches!(
                        c,
                        '(' | '[' | '{' | '‘' | '“' | '「' | '『' | '（' | '"' | '\''
                    )
                })
                .map(char::len_utf8)
                .sum::<usize>();
            next_start - opening
        };
        let mut revised = text[..split].to_owned();
        if self.tail_start != 0 {
            // A new request starts mid-dictation. Keep the first word's
            // formatting from when the recognizer heard the preceding audio.
            revised.replace_range(..new_first_end, &previous[..old_first_end]);
        }
        self.text.truncate(self.tail_start);
        self.text.push_str(&revised);
        self.tail_start = self.text.len();
        self.text.push_str(&text[split..]);
        true
    }
}

fn same_word(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
        || left.to_lowercase().replace(['\'', '’'], "")
            == right.to_lowercase().replace(['\'', '’'], "")
}

pub fn clean_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ")
        .chars()
        .filter(|&c| c == '\n' || !c.is_control())
        .collect()
}

pub(super) fn needs_space(left: Option<char>, right: Option<char>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    !(left.is_whitespace()
        || right.is_whitespace()
        || matches!(left, '(' | '[' | '{' | '‘' | '“')
        || matches!(
            right,
            ',' | '.'
                | '!'
                | '?'
                | ';'
                | ':'
                | ')'
                | ']'
                | '}'
                | '’'
                | '”'
                | '，'
                | '。'
                | '！'
                | '？'
        )
        || (is_cjk(left) && is_cjk(right)))
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}')
}
