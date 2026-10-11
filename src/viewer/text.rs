//! The viewer's text: one shared string plus where each line starts.
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone)]
pub struct Text {
    src: Arc<str>,
    /// Byte offset of each line's first character; always holds at least one entry.
    starts: Vec<usize>,
}

impl Text {
    pub fn new(src: impl Into<Arc<str>>) -> Self {
        let src: Arc<str> = src.into();
        let mut starts = vec![0];
        starts.extend(src.match_indices('\n').map(|(ix, _)| ix + 1));
        Self { src, starts }
    }

    pub fn src(&self) -> &Arc<str> {
        &self.src
    }

    pub fn len(&self) -> usize {
        self.src.len()
    }

    pub fn lines(&self) -> usize {
        self.starts.len()
    }

    /// The line's bytes without its `\n` or `\r\n`.
    pub fn line_range(&self, line: usize) -> Range<usize> {
        let start = self.starts[line];
        let mut end = self.starts.get(line + 1).map_or(self.src.len(), |next| next - 1);
        if self.src[start..end].ends_with('\r') {
            end -= 1;
        }
        start..end
    }

    pub fn line(&self, line: usize) -> &str {
        &self.src[self.line_range(line)]
    }

    /// The line holding byte `offset`.
    pub fn line_of(&self, offset: usize) -> usize {
        self.starts.partition_point(|&start| start <= offset) - 1
    }

    /// The character boundary at or before `offset`, never between a `\r` and its `\n`.
    pub fn clip_left(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.src.len());
        while !self.src.is_char_boundary(offset) {
            offset -= 1;
        }
        if offset > 0 && self.src[..offset].ends_with('\r') && self.src[offset..].starts_with('\n') {
            offset -= 1;
        }
        offset
    }

    /// The character boundary at or after `offset`, never between a `\r` and its `\n`.
    pub fn clip_right(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.src.len());
        while !self.src.is_char_boundary(offset) {
            offset += 1;
        }
        if offset > 0 && self.src[..offset].ends_with('\r') && self.src[offset..].starts_with('\n') {
            offset += 1;
        }
        offset
    }

    /// One character back, as Left moves.
    pub fn previous_boundary(&self, offset: usize) -> usize {
        self.clip_left(offset.saturating_sub(1))
    }

    /// One character on, as Right moves.
    pub fn next_boundary(&self, offset: usize) -> usize {
        self.clip_right(offset + 1)
    }

    /// The start of the last word before `offset`, as Ctrl+Left moves: the last Unicode word
    /// segment that isn't whitespace. Lines are scanned one at a time, which segments the same
    /// as the whole text since words always break at newlines.
    pub fn previous_word_start(&self, offset: usize) -> usize {
        use unicode_segmentation::UnicodeSegmentation;
        let mut line = self.line_of(offset);
        let mut end = offset;
        loop {
            let start = self.starts[line];
            let found = self.src[start..end]
                .split_word_bound_indices()
                .rfind(|(_, s)| !s.trim_start().is_empty())
                .map(|(i, _)| start + i);
            match found {
                Some(found) => return found,
                None if line == 0 => return 0,
                None => {
                    line -= 1;
                    end = self.line_range(line).end;
                }
            }
        }
    }

    /// The end of the first word after `offset`, as Ctrl+Right moves.
    pub fn next_word_end(&self, offset: usize) -> usize {
        use unicode_segmentation::UnicodeSegmentation;
        let mut line = self.line_of(offset);
        let mut start = offset;
        loop {
            let end = self.line_range(line).end.max(start);
            let found = self.src[start..end]
                .split_word_bound_indices()
                .find(|(_, s)| !s.trim_start().is_empty())
                .map(|(i, s)| start + i + s.len());
            match found {
                Some(found) => return found,
                None if line + 1 == self.lines() => return self.src.len(),
                None => {
                    line += 1;
                    start = self.starts[line];
                }
            }
        }
    }

    /// The word a double-click at `offset` selects: a run of word characters, or of spaces,
    /// or else the one character. Ported from gpui-kit, scanning at most 128 characters each way.
    pub fn word_range(&self, offset: usize) -> Option<Range<usize>> {
        let offset = self.clip_left(offset);
        let c = self.src[offset..].chars().next()?;
        let kind = CharKind::of(c);
        let joins = |n: &char| {
            matches!((kind, CharKind::of(*n)), (CharKind::Word, CharKind::Word) | (CharKind::Space, CharKind::Space))
        };
        let start = self.src[..offset].chars().rev().take(128).take_while(joins).fold(offset, |o, c| o - c.len_utf8());
        let after = offset + c.len_utf8();
        let end = self.src[after..].chars().take(128).take_while(joins).fold(after, |o, c| o + c.len_utf8());
        Some(start..end)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum CharKind {
    Word,
    Space,
    Newline,
    Other,
}

impl CharKind {
    fn of(c: char) -> Self {
        if c == '_'
            || c.is_ascii_alphanumeric()
            || matches!(c, '\u{00C0}'..='\u{024F}' | '\u{0400}'..='\u{04FF}' | '\u{1E00}'..='\u{1EFF}' | '\u{0300}'..='\u{036F}')
        {
            Self::Word
        } else if matches!(c, '\n' | '\r') {
            Self::Newline
        } else if c.is_whitespace() {
            Self::Space
        } else {
            Self::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Text;

    #[test]
    fn lines_drop_their_endings() {
        let text = Text::new("a\r\nbc\n\nd");
        assert_eq!(text.lines(), 4);
        assert_eq!([text.line(0), text.line(1), text.line(2), text.line(3)], ["a", "bc", "", "d"]);
        assert_eq!(text.line_of(0), 0);
        assert_eq!(text.line_of(3), 1);
        assert_eq!(text.line_of(7), 3);
    }

    #[test]
    fn steps_skip_crlf_pairs_and_multibyte_characters() {
        let text = Text::new("a\r\nü");
        assert_eq!(text.next_boundary(1), 3);
        assert_eq!(text.previous_boundary(3), 1);
        assert_eq!(text.next_boundary(3), 5);
        assert_eq!(text.previous_boundary(5), 3);
        assert_eq!(text.next_boundary(5), 5);
    }

    #[test]
    fn word_moves_skip_spaces_and_cross_lines() {
        let text = Text::new("let foo = bar;\n  baz");
        assert_eq!(text.next_word_end(0), 3);
        assert_eq!(text.next_word_end(3), 7);
        assert_eq!(text.next_word_end(13), 14);
        assert_eq!(text.next_word_end(14), 20);
        assert_eq!(text.previous_word_start(20), 17);
        assert_eq!(text.previous_word_start(17), 13);
        assert_eq!(text.previous_word_start(4), 0);
        assert_eq!(text.previous_word_start(0), 0);
    }

    #[test]
    fn double_click_words_match_the_kit() {
        let text = Text::new("test text:\nabcde 中文 hello[()]  x_y");
        let word = |o| text.word_range(o).map(|r| &text.src()[r]);
        assert_eq!(word(0), Some("test"));
        assert_eq!(word(4), Some(" "));
        assert_eq!(word(9), Some(":"));
        assert_eq!(word(11), Some("abcde"));
        assert_eq!(word(17), Some("中"));
        assert_eq!(word(29), Some("["));
        assert_eq!(word(30), Some("("));
        assert_eq!(word(34), Some("  "));
        assert_eq!(word(36), Some("x_y"));
        assert_eq!(word(38), None);
    }

    #[test]
    fn trailing_newline_ends_with_an_empty_line() {
        let text = Text::new("a\n");
        assert_eq!(text.lines(), 2);
        assert_eq!(text.line(1), "");
    }
}
