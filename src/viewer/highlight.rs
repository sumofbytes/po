//! Syntax highlighting: parse the whole file with tree-sitter and record which capture name
//! covers each byte. Colors are looked up from the theme at paint time, so a theme switch
//! needs no reparse.
//!
//! Which capture wins where captures overlap follows gpui-kit's highlighter, so files color
//! the same as they did in its editor: a later capture covers an earlier one, except that one
//! with exactly the same range keeps the first name; injected languages (code in Markdown,
//! script in HTML) cover the host language, and within one of them the first capture wins.
use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, Parser, Point, Query, QueryCursor, Tree};

/// Separate parses for injections that aren't combined, such as fenced code blocks; the kit's
/// limit.
const MAX_INJECTIONS: usize = 512;
/// Capture names one file can color with; the canvas keeps a byte per byte of text.
const MAX_NAMES: usize = u8::MAX as usize;

/// A language's grammar and compiled queries. Compiling a query takes tens of milliseconds, so
/// each is built once per run, the first time a file needs it.
struct Grammar {
    name: String,
    language: tree_sitter::Language,
    /// Locals and highlights, after the injection patterns, which are turned off; one query
    /// like the kit's so pattern order, and so which capture wins, is the same.
    query: Query,
    injections: Option<Query>,
    /// Highlights alone, used when this language is injected into another.
    injected: OnceLock<Option<Query>>,
    highlights: String,
}

impl Grammar {
    fn injected(&self) -> Option<&Query> {
        self.injected.get_or_init(|| Query::new(&self.language, &self.highlights).ok()).as_ref()
    }
}

/// The grammar for a language name or file extension, compiled on first use.
fn grammar(name: &str) -> Option<Arc<Grammar>> {
    static CACHE: LazyLock<Mutex<HashMap<String, Option<Arc<Grammar>>>>> = LazyLock::new(Default::default);
    if let Some(grammar) = CACHE.lock().unwrap().get(name) {
        return grammar.clone();
    }
    let grammar = (|| {
        let config = super::languages::find(name)?;
        let language = config.grammar;
        let source = format!("{}{}{}", config.injections, config.locals, config.highlights);
        let mut query = Query::new(&language, &source).ok()?;
        let injection_patterns =
            (0..query.pattern_count()).take_while(|&i| query.start_byte_for_pattern(i) < config.injections.len()).count();
        for i in 0..injection_patterns {
            query.disable_pattern(i);
        }
        let injections = (!config.injections.is_empty()).then(|| Query::new(&language, config.injections).ok()).flatten();
        Some(Arc::new(Grammar {
            name: config.name.to_string(),
            language,
            query,
            injections,
            injected: OnceLock::new(),
            highlights: config.highlights.to_string(),
        }))
    })();
    CACHE.lock().unwrap().insert(name.to_string(), grammar.clone());
    grammar
}

/// Compiles the grammars for `languages` (names or file extensions), so the first file of
/// each opens without waiting for its queries. Meant for a background thread.
pub fn prewarm<'a>(languages: impl IntoIterator<Item = &'a str>) {
    for language in languages {
        grammar(language);
    }
}

/// Runs of the same capture over the text: `starts[i]..starts[i + 1]` has capture `ids[i]`,
/// where 0 means none and `n` means `names[n - 1]`.
#[derive(Default)]
pub struct Highlights {
    pub names: Vec<String>,
    /// Line ranges that can fold, by first line, with their last line.
    pub folds: Vec<(usize, usize)>,
    starts: Vec<u32>,
    ids: Vec<u16>,
    /// The parse, kept so the next version of the text reparses only what changed.
    tree: Option<Tree>,
}

impl Highlights {
    /// The runs covering `range`, clipped to it, as (length, capture id).
    pub fn runs(&self, range: Range<usize>) -> impl Iterator<Item = (usize, u16)> + '_ {
        let first = self.starts.partition_point(|&s| s as usize <= range.start).saturating_sub(1);
        let mut at = range.start;
        (first..self.starts.len()).map_while(move |i| {
            let end = self.starts.get(i + 1).map_or(usize::MAX, |&s| s as usize).min(range.end);
            if at >= range.end {
                return None;
            }
            let run = (end - at, self.ids[i]);
            at = end;
            Some(run)
        })
    }
}

/// Every byte's capture, filled in as captures are painted over each other.
struct Canvas<'a> {
    text: &'a str,
    ids: Vec<u8>,
    names: Vec<String>,
    by_name: HashMap<String, u8>,
}

impl Canvas<'_> {
    fn paint(&mut self, range: Range<usize>, name: &str) {
        let id = match self.by_name.get(name) {
            Some(&id) => id,
            // No grammar comes near this many names; any past it stay uncolored.
            None if self.names.len() >= MAX_NAMES => return,
            None => {
                self.names.push(name.to_string());
                let id = self.names.len() as u8;
                self.by_name.insert(name.to_string(), id);
                id
            }
        };
        let end = range.end.min(self.ids.len());
        if range.start < end {
            self.ids[range.start..end].fill(id);
        }
    }

    fn finish(self) -> Highlights {
        let (mut starts, mut ids) = (Vec::new(), Vec::new());
        for (ix, &id) in self.ids.iter().enumerate() {
            // Runs only start on character boundaries, so shaping never splits a character.
            if ids.last() != Some(&(id as u16)) && self.text.is_char_boundary(ix) {
                starts.push(ix as u32);
                ids.push(id as u16);
            }
        }
        if starts.is_empty() {
            starts.push(0);
            ids.push(0);
        }
        Highlights { names: self.names, folds: Vec::new(), starts, ids, tree: None }
    }
}

/// Highlights `text` as `language` (a name or file extension the kit's registry knows), or
/// `None` when there is no grammar for it.
pub fn highlight(text: &str, language: &str) -> Option<Highlights> {
    rehighlight(text, language, None)
}

/// Like [`highlight`], reusing `old`, the highlights of `old_text`, so only the parts of the
/// tree the edit touched are parsed again.
pub fn rehighlight(text: &str, language: &str, old: Option<(&str, &Highlights)>) -> Option<Highlights> {
    let grammar = grammar(language)?;
    let mut parser = Parser::new();
    parser.set_language(&grammar.language).ok()?;
    let old_tree = old.and_then(|(old_text, old)| {
        let mut tree = old.tree.clone()?;
        tree.edit(&edit(old_text, text));
        Some(tree)
    });
    let tree = parser.parse(text, old_tree.as_ref())?;

    let mut canvas = Canvas { text, ids: vec![0; text.len()], names: Vec::new(), by_name: HashMap::new() };
    let query = &grammar.query;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
    let mut last = 0..0;
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let range = cap.node.byte_range();
            if range != last {
                canvas.paint(range.clone(), query.capture_names()[cap.index as usize]);
                last = range;
            }
        }
    }

    if let Some(injections) = &grammar.injections {
        for (grammar, tree) in injection_layers(text, &tree, injections) {
            if let Some(query) = grammar.injected() {
                paint_layer(&mut canvas, query, &tree);
            }
        }
    }
    let mut highlights = canvas.finish();
    highlights.folds = folds(&tree);
    highlights.tree = Some(tree);
    Some(highlights)
}

/// The one edit that turns `old` into `new`: everything between their common start and end.
fn edit(old: &str, new: &str) -> InputEdit {
    let mut start = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
        start -= 1;
    }
    let room = old.len().min(new.len()) - start;
    let suffix = old.bytes().rev().zip(new.bytes().rev()).take(room).take_while(|(a, b)| a == b).count();
    let (mut old_end, mut new_end) = (old.len() - suffix, new.len() - suffix);
    while !old.is_char_boundary(old_end) || !new.is_char_boundary(new_end) {
        old_end += 1;
        new_end += 1;
    }
    InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point(old, start),
        old_end_position: point(old, old_end),
        new_end_position: point(new, new_end),
    }
}

/// The row and byte column of `byte` in `text`.
fn point(text: &str, byte: usize) -> Point {
    let before = &text.as_bytes()[..byte];
    let row = before.iter().filter(|&&b| b == b'\n').count();
    let column = byte - before.iter().rposition(|&b| b == b'\n').map_or(0, |ix| ix + 1);
    Point { row, column }
}

/// What can fold, as the kit decides: every named node spanning three or more lines, keeping
/// the outermost where several start on one line.
fn folds(tree: &Tree) -> Vec<(usize, usize)> {
    fn collect(node: tree_sitter::Node, out: &mut Vec<(usize, usize)>) {
        let (start, end) = (node.start_position().row, node.end_position().row);
        if end.saturating_sub(start) < 2 {
            return;
        }
        out.push((start, end));
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect(child, out);
        }
    }
    let mut out = Vec::new();
    let root = tree.root_node();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        collect(child, &mut out);
    }
    out.sort_by_key(|&(start, _)| start);
    out.dedup_by_key(|&mut (start, _)| start);
    out
}

/// Within an injected language the first capture wins unless a pattern allows overlap.
fn paint_layer(canvas: &mut Canvas, query: &Query, tree: &Tree) {
    let text = canvas.text;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
    let mut last_end = 0;
    while let Some(m) = matches.next() {
        let overlap = query.property_settings(m.pattern_index).iter().any(|p| &*p.key == "highlight.allow-overlap");
        for cap in m.captures {
            let range = cap.node.byte_range();
            if !overlap && range.start < last_end {
                continue;
            }
            if !overlap {
                last_end = range.end;
            }
            canvas.paint(range, query.capture_names()[cap.index as usize]);
        }
    }
}

/// Parses each injected region with its own language, in text order.
fn injection_layers(text: &str, tree: &Tree, injections: &Query) -> Vec<(Arc<Grammar>, Tree)> {
    let content = injections.capture_index_for_name("injection.content");
    let language = injections.capture_index_for_name("injection.language");
    let mut combined: HashMap<String, (Arc<Grammar>, Vec<tree_sitter::Range>)> = HashMap::new();
    let mut single = Vec::new();

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(injections, tree.root_node(), text.as_bytes());
    while let Some(m) = matches.next() {
        let mut name = None;
        let mut is_combined = false;
        for prop in injections.property_settings(m.pattern_index) {
            match &*prop.key {
                "injection.language" => name = prop.value.as_deref().map(str::to_string),
                "injection.combined" => is_combined = true,
                _ => {}
            }
        }
        let name = name.or_else(|| {
            let cap = m.captures.iter().find(|c| Some(c.index) == language)?;
            let name = text.get(cap.node.byte_range())?.trim();
            (!name.is_empty() && name.len() <= 64).then(|| name.to_string())
        });
        let Some(grammar) = name.and_then(|name| self::grammar(&name)) else { continue };
        let ranges: Vec<_> =
            m.captures.iter().filter(|c| Some(c.index) == content).map(|c| c.node.range()).collect();
        if ranges.is_empty() {
            continue;
        }
        if is_combined {
            combined.entry(grammar.name.clone()).or_insert_with(|| (grammar, Vec::new())).1.extend(ranges);
        } else if single.len() < MAX_INJECTIONS {
            single.push((grammar, ranges));
        }
    }

    let mut layers: Vec<(usize, Arc<Grammar>, Tree)> = Vec::new();
    for (grammar, mut ranges) in single.into_iter().chain(combined.into_values()) {
        ranges.sort_by_key(|r| r.start_byte);
        ranges.dedup_by_key(|r| r.start_byte);
        let mut parser = Parser::new();
        if parser.set_language(&grammar.language).is_err() || parser.set_included_ranges(&ranges).is_err() {
            continue;
        }
        if let Some(tree) = parser.parse(text, None) {
            layers.push((ranges[0].start_byte, grammar, tree));
        }
    }
    layers.sort_by_key(|(start, ..)| *start);
    layers.into_iter().map(|(_, grammar, tree)| (grammar, tree)).collect()
}

#[cfg(test)]
mod tests {
    use super::highlight;

    fn names_at<'a>(h: &'a super::Highlights, text: &str, needle: &str) -> Vec<&'a str> {
        let start = text.find(needle).unwrap();
        h.runs(start..start + needle.len())
            .map(|(_, id)| if id == 0 { "" } else { h.names[id as usize - 1].as_str() })
            .collect()
    }

    #[test]
    fn rust_keywords_and_strings() {
        let text = "fn main() { let s = \"hi\"; }";
        let h = highlight(text, "rs").unwrap();
        assert_eq!(names_at(&h, text, "fn"), ["keyword"]);
        assert_eq!(names_at(&h, text, "\"hi\""), ["string"]);
    }

    #[test]
    fn markdown_fences_use_the_fenced_language() {
        let text = "# Title\n\n```rust\nfn main() {}\n```\n";
        let h = highlight(text, "md").unwrap();
        assert_eq!(names_at(&h, text, "fn"), ["keyword"]);
    }

    #[test]
    fn runs_cover_the_range_exactly() {
        let text = "let x = 1;";
        let h = highlight(text, "js").unwrap();
        assert_eq!(h.runs(2..9).map(|(len, _)| len).sum::<usize>(), 7);
    }

    #[test]
    fn blocks_of_three_or_more_lines_fold() {
        let text = "fn a() {\n    if x {\n        y();\n    }\n}\nfn b() {}\n";
        let h = highlight(text, "rs").unwrap();
        assert_eq!(h.folds, [(0, 4), (1, 3)]);
    }

    #[test]
    fn reparsing_an_edit_matches_a_fresh_parse() {
        let old = "fn a() {\n    let x = 1;\n}\n";
        let new = "fn a() {\n    let x = \"ü\";\n    if x {\n        y();\n    }\n}\n";
        let fresh = highlight(new, "rs").unwrap();
        let reused = super::rehighlight(new, "rs", Some((old, &highlight(old, "rs").unwrap()))).unwrap();
        assert_eq!((&reused.names, &reused.starts, &reused.ids, &reused.folds), (&fresh.names, &fresh.starts, &fresh.ids, &fresh.folds));
    }

    #[test]
    fn edits_cover_only_what_changed() {
        let edit = super::edit("ab\ncd\nef", "ab\ncXd\nef");
        assert_eq!((edit.start_byte, edit.old_end_byte, edit.new_end_byte), (4, 4, 5));
        assert_eq!((edit.start_position.row, edit.start_position.column), (1, 1));
        let edit = super::edit("aüb", "aöb");
        assert_eq!((edit.start_byte, edit.old_end_byte, edit.new_end_byte), (1, 3, 3));
    }

    #[test]
    fn unknown_language_has_no_highlights() {
        assert!(highlight("hello", "no-such-language").is_none());
    }
}

