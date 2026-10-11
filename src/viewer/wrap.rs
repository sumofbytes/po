//! Soft wrap: where each line breaks into display rows, and which rows belong to which line.
use super::text::Text;

/// A row break inside a line: the byte in the line where the next row starts, and how many
/// columns that row is indented by to line up under the line's own indent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Break {
    pub ix: u32,
    pub indent: u32,
}

/// Where `text` breaks to fit `width`, ported from gpui-kit so lines wrap exactly as they did:
/// break at the last Unicode line-break opportunity that fits, else after the last whole
/// grapheme that fits, and narrow continuation rows by the line's leading spaces, which they
/// are indented by. `measure` gives the width of a byte range of `text`.
pub fn breaks<T: Copy + PartialOrd + std::ops::Sub<Output = T>>(
    text: &str,
    width: T,
    mut measure: impl FnMut(std::ops::Range<usize>) -> T,
) -> Vec<Break> {
    use unicode_segmentation::UnicodeSegmentation;
    const MAX_INDENT: usize = 256;

    // Most lines fit; skip finding their graphemes and break opportunities.
    if measure(0..text.len()) <= width {
        return Vec::new();
    }

    let indent = text.chars().take_while(|&c| c == ' ').count().min(MAX_INDENT);
    let indent_width = measure(0..indent);
    // In ASCII every byte is a grapheme (lines have no `\r\n`), and finding them costs most of a wrap.
    let ends: Vec<usize> = if text.is_ascii() {
        (1..=text.len()).collect()
    } else {
        text.grapheme_indices(true).map(|(ix, g)| ix + g.len()).collect()
    };
    let opportunities: Vec<usize> = unicode_linebreak::linebreaks(text)
        .map(|(ix, _)| ix)
        .filter(|ix| ends.binary_search(ix).is_ok())
        .collect();
    let mut result = Vec::new();
    let (mut first, mut start) = (0, 0);
    while first < ends.len() {
        let available = if start == 0 { width } else { width - indent_width };
        // Probe exponentially, then bisect, so a long line isn't measured whole for every row.
        let remaining = ends.len() - first;
        let (mut low, mut high) = (0, 1);
        while measure(start..ends[first + high - 1]) <= available {
            low = high;
            if high == remaining {
                break;
            }
            high = (high * 2).min(remaining);
        }
        while low + 1 < high {
            let mid = (low + high) / 2;
            if measure(start..ends[first + mid - 1]) <= available {
                low = mid;
            } else {
                high = mid;
            }
        }
        // A grapheme wider than the row still takes one.
        let fitting_end = ends[first + low.max(1) - 1];
        if fitting_end == text.len() {
            break;
        }
        let candidate = opportunities.partition_point(|&ix| ix <= fitting_end);
        let end = candidate
            .checked_sub(1)
            .map(|ix| opportunities[ix])
            .filter(|&ix| ix > start && (start != 0 || ix > indent))
            .unwrap_or(fitting_end);
        result.push(Break { ix: end as u32, indent: indent as u32 });
        first = ends.partition_point(|&ix| ix <= end);
        start = end;
    }
    result
}

#[derive(Default)]
pub struct WrapMap {
    /// The first display row of each line, plus the total row count at the end. A folded-away
    /// line has no rows: its start equals the next line's.
    row_starts: Vec<u32>,
    /// How many breaks the lines before each one have, plus the total at the end.
    break_starts: Vec<u32>,
    /// Every line's breaks, in order.
    breaks: Vec<Break>,
}

impl WrapMap {
    /// One row per line, used before the first layout or without a width.
    pub fn unwrapped(lines: usize) -> Self {
        Self { row_starts: (0..=lines as u32).collect(), break_starts: vec![0; lines + 1], breaks: Vec::new() }
    }

    pub fn build<I: IntoIterator<Item = Break>>(text: &Text, mut wrap: impl FnMut(&str) -> I) -> Self {
        let mut break_starts = Vec::with_capacity(text.lines() + 1);
        let mut breaks = Vec::new();
        for line in 0..text.lines() {
            break_starts.push(breaks.len() as u32);
            breaks.extend(wrap(text.line(line)));
        }
        break_starts.push(breaks.len() as u32);
        let mut map = Self { row_starts: Vec::new(), break_starts, breaks };
        map.hide(&[]);
        map
    }

    /// Hides the lines in `hidden`, sorted ranges of line numbers, and shows every other line.
    pub fn hide(&mut self, hidden: &[std::ops::Range<usize>]) {
        let lines = self.break_starts.len() - 1;
        self.row_starts.clear();
        self.row_starts.reserve(lines + 1);
        let mut next = hidden.iter().peekable();
        let mut row = 0;
        for line in 0..lines {
            while next.peek().is_some_and(|r| r.end <= line) {
                next.next();
            }
            self.row_starts.push(row);
            if !next.peek().is_some_and(|r| r.start <= line) {
                row += 1 + self.break_starts[line + 1] - self.break_starts[line];
            }
        }
        self.row_starts.push(row);
    }

    pub fn rows(&self) -> usize {
        self.row_starts.last().map_or(0, |&rows| rows as usize)
    }

    pub fn first_row(&self, line: usize) -> usize {
        self.row_starts[line] as usize
    }

    pub fn is_hidden(&self, line: usize) -> bool {
        self.row_starts[line] == self.row_starts[line + 1]
    }

    /// The line a display row belongs to, and which of that line's rows it is. Rows past the
    /// end belong to the last row.
    pub fn line_of_row(&self, row: usize) -> (usize, usize) {
        let row = row.min(self.rows().saturating_sub(1));
        // Hidden lines share their start with the next shown one, which is the one found.
        let line = self.row_starts.partition_point(|&start| start as usize <= row) - 1;
        (line, row - self.row_starts[line] as usize)
    }

    pub fn breaks(&self, line: usize) -> &[Break] {
        &self.breaks[self.break_starts[line] as usize..self.break_starts[line + 1] as usize]
    }

    /// The bytes of `line` shown on its `sub`th row, relative to the line, and that row's indent.
    pub fn segment(&self, line: usize, sub: usize, line_len: usize) -> (std::ops::Range<usize>, u32) {
        let breaks = self.breaks(line);
        let start = if sub == 0 { 0 } else { breaks[sub - 1].ix as usize };
        let end = breaks.get(sub).map_or(line_len, |b| b.ix as usize);
        let indent = if sub == 0 { 0 } else { breaks[sub - 1].indent };
        (start..end, indent)
    }
}

#[cfg(test)]
mod tests {
    use super::{Break, WrapMap};
    use crate::viewer::text::Text;

    /// Breaks every line after each `width` bytes, like a fixed-width wrap with no word boundaries.
    fn every(width: usize) -> impl FnMut(&str) -> Vec<Break> {
        move |line| (width..line.len()).step_by(width).map(|ix| Break { ix: ix as u32, indent: 0 }).collect()
    }

    #[test]
    fn rows_map_back_to_lines() {
        let text = Text::new("abcdefg\nab\nabcde");
        let map = WrapMap::build(&text, every(3));
        // abc|def|g, ab, abc|de
        assert_eq!(map.rows(), 6);
        assert_eq!(map.first_row(1), 3);
        assert_eq!(map.first_row(2), 4);
        assert_eq!(map.line_of_row(0), (0, 0));
        assert_eq!(map.line_of_row(2), (0, 2));
        assert_eq!(map.line_of_row(3), (1, 0));
        assert_eq!(map.line_of_row(5), (2, 1));
        assert_eq!(map.line_of_row(99), (2, 1));
        assert_eq!(map.segment(0, 1, 7), (3..6, 0));
        assert_eq!(map.segment(0, 2, 7), (6..7, 0));
        assert_eq!(map.segment(1, 0, 2), (0..2, 0));
        assert_eq!(map.segment(2, 1, 5), (3..5, 0));
    }

    /// Width in columns, as a monospace font measures it.
    fn wrap(text: &str, width: usize) -> Vec<&str> {
        let breaks = super::breaks(text, width as i64, |r| text[r].chars().count() as i64);
        let mut start = 0;
        let mut rows = Vec::new();
        for b in breaks {
            rows.push(&text[start..b.ix as usize]);
            start = b.ix as usize;
        }
        rows.push(&text[start..]);
        rows
    }

    #[test]
    fn breaks_after_words() {
        assert_eq!(wrap("one two three", 8), ["one two ", "three"]);
        assert_eq!(wrap("short", 8), ["short"]);
    }

    #[test]
    fn continuation_rows_lose_the_indent_width() {
        // 4 spaces of indent leave continuation rows 9 of the 13 columns: "Inspector" just fits.
        assert_eq!(wrap("    Hsla, Inspector", 13), ["    Hsla, ", "Inspector"]);
        assert_eq!(wrap("    Hsla, Inspectors", 13), ["    Hsla, ", "Inspector", "s"]);
        assert_eq!(super::breaks("    aaa bbb ccc", 10, |r| r.len() as i64)[0].indent, 4);
    }

    #[test]
    fn long_words_split_on_graphemes() {
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn hidden_lines_have_no_rows() {
        let text = Text::new("abcdefg\nab\nabcde\nx");
        let mut map = WrapMap::build(&text, every(3));
        // abc|def|g, ab, abc|de, x; hide "ab" and "abc|de".
        map.hide(&[1..3]);
        assert_eq!(map.rows(), 4);
        assert!(map.is_hidden(1) && map.is_hidden(2) && !map.is_hidden(3));
        assert_eq!(map.line_of_row(2), (0, 2));
        assert_eq!(map.line_of_row(3), (3, 0));
        assert_eq!(map.first_row(3), 3);
        assert_eq!(map.breaks(2).len(), 1);
        map.hide(&[]);
        assert_eq!(map.rows(), 7);
    }

    #[test]
    fn unwrapped_is_one_row_per_line() {
        let map = WrapMap::unwrapped(3);
        assert_eq!(map.rows(), 3);
        assert_eq!(map.line_of_row(2), (2, 0));
        assert!(map.breaks(1).is_empty());
    }
}

