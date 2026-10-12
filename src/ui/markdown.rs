//! Markdown for the agent's replies, laid out like gpui-kit's `TextView`: the same parser and
//! the same block spacing, heading scale, lists, quotes, code blocks, rules and tables. Text
//! is selected by dragging across any of the replies in a [`Selection`], a double click takes
//! a word and a triple click a paragraph, and links open on click.
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, ElementId, Entity, FontStyle, FontWeight, HighlightStyle, IntoElement,
    ParentElement as _, Pixels, Point, SharedString, StrikethroughStyle, Styled as _, StyledText, TextLayout,
    TextRun, UnderlineStyle, Window, canvas, div, prelude::FluentBuilder as _, px, relative, rems,
};
use ::markdown::mdast::{AlignKind, Node};

use crate::theme::Palette;

gpui::actions!(markdown, [Copy, SelectAll]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("ctrl-c", Copy, Some("Markdown")),
        gpui::KeyBinding::new("ctrl-a", SelectAll, Some("Markdown")),
    ]);
}

const BULLETS: [&str; 5] = ["•", "◦", "▪", "‣", "⁃"];
const UPPER: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER: &str = "abcdefghijklmnopqrstuvwxyz";
/// The widest a table column is measured to be, in characters.
const MAX_COLUMN: usize = 150;

#[derive(Clone, Copy, Default, PartialEq)]
struct Mark {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
}

/// A run of text with its marks and links: what one paragraph, heading or cell shows.
#[derive(Clone, Default, PartialEq)]
struct Inline {
    text: String,
    marks: Vec<(Range<usize>, Mark)>,
    links: Vec<(Range<usize>, String)>,
}

#[derive(Clone, PartialEq)]
enum Block {
    Paragraph(Inline),
    Heading(u8, Inline),
    Quote(Vec<Block>),
    List { ordered: bool, start: Option<u32>, items: Vec<Item> },
    Code { language: Option<String>, code: String },
    Rule,
    Table { align: Vec<AlignKind>, rows: Vec<Vec<Inline>> },
}

#[derive(Clone, PartialEq)]
struct Item {
    checked: Option<bool>,
    children: Vec<Block>,
}

fn parse(source: &str) -> Vec<Block> {
    match ::markdown::to_mdast(source, &::markdown::ParseOptions::gfm()) {
        Ok(Node::Root(root)) => blocks(&root.children),
        _ => vec![Block::Paragraph(Inline { text: source.to_string(), ..Default::default() })],
    }
}

fn blocks(nodes: &[Node]) -> Vec<Block> {
    nodes.iter().filter_map(block).collect()
}

fn block(node: &Node) -> Option<Block> {
    Some(match node {
        Node::Paragraph(p) => Block::Paragraph(inline(&p.children)),
        Node::Heading(h) => Block::Heading(h.depth, inline(&h.children)),
        Node::Blockquote(q) => Block::Quote(blocks(&q.children)),
        Node::List(list) => Block::List {
            ordered: list.ordered,
            start: list.start,
            items: list
                .children
                .iter()
                .filter_map(|item| match item {
                    Node::ListItem(item) => Some(Item { checked: item.checked, children: blocks(&item.children) }),
                    _ => None,
                })
                .collect(),
        },
        Node::Code(code) => Block::Code { language: code.lang.clone(), code: code.value.clone() },
        Node::ThematicBreak(_) => Block::Rule,
        Node::Table(table) => Block::Table {
            align: table.align.clone(),
            rows: table
                .children
                .iter()
                .filter_map(|row| match row {
                    Node::TableRow(row) => Some(
                        row.children
                            .iter()
                            .map(|cell| match cell {
                                Node::TableCell(cell) => inline(&cell.children),
                                _ => Inline::default(),
                            })
                            .collect(),
                    ),
                    _ => None,
                })
                .collect(),
        },
        // Raw HTML shows as written.
        Node::Html(html) => Block::Paragraph(Inline { text: html.value.clone(), ..Default::default() }),
        Node::Math(math) => Block::Code { language: None, code: math.value.clone() },
        _ => return None,
    })
}

fn inline(nodes: &[Node]) -> Inline {
    let mut out = Inline::default();
    for node in nodes {
        push_inline(node, Mark::default(), None, &mut out);
    }
    out
}

fn push_inline(node: &Node, mark: Mark, link: Option<&str>, out: &mut Inline) {
    let start = out.text.len();
    let leaf = |text: &str, mark: Mark, out: &mut Inline| {
        let start = out.text.len();
        out.text.push_str(text);
        if mark != Mark::default() {
            out.marks.push((start..out.text.len(), mark));
        }
    };
    match node {
        // A soft break inside a paragraph reflows like a space, as the kit's does.
        Node::Text(t) => leaf(&t.value.replace("\r\n", " ").replace(['\n', '\r'], " "), mark, out),
        Node::Break(_) => leaf("\n", mark, out),
        Node::InlineCode(c) => leaf(&c.value, Mark { code: true, ..mark }, out),
        Node::Html(h) => leaf(&h.value, mark, out),
        Node::InlineMath(m) => leaf(&format!("${}$", m.value), mark, out),
        Node::Image(image) => leaf(&image.alt, mark, out),
        Node::Emphasis(e) => e.children.iter().for_each(|c| push_inline(c, Mark { italic: true, ..mark }, link, out)),
        Node::Strong(s) => s.children.iter().for_each(|c| push_inline(c, Mark { bold: true, ..mark }, link, out)),
        Node::Delete(d) => d.children.iter().for_each(|c| push_inline(c, Mark { strike: true, ..mark }, link, out)),
        Node::Link(l) => l.children.iter().for_each(|c| push_inline(c, mark, Some(&l.url), out)),
        Node::LinkReference(l) => l.children.iter().for_each(|c| push_inline(c, mark, link, out)),
        other => {
            if let Some(children) = other.children() {
                children.iter().for_each(|c| push_inline(c, mark, link, out));
            }
        }
    }
    if let Some(url) = link
        && !matches!(node, Node::Link(_))
        && out.text.len() > start
    {
        out.links.push((start..out.text.len(), url.to_string()));
    }
}

/// Where a text run is, so selection can find characters in it between frames.
struct Run {
    text: SharedString,
    layout: TextLayout,
    links: Rc<Vec<(Range<usize>, String)>>,
}

/// The selection across every reply shown at once.
#[derive(Default)]
pub struct Selection {
    /// Where the drag started and where the pointer is, in window coordinates.
    anchor: Option<Point<Pixels>>,
    head: Option<Point<Pixels>>,
    /// A run and range picked by a double or triple click, or everything.
    picked: Option<Picked>,
    pub selecting: bool,
    /// The runs laid out in the last frame, in reading order.
    runs: Vec<(SharedString, Run)>,
    index: HashMap<SharedString, usize>,
    /// Parsed replies, by their id.
    parsed: HashMap<ElementId, Parsed>,
}

/// A reply's text, its blocks and its code blocks' colors, kept until the text changes.
struct Parsed {
    source: String,
    blocks: Rc<Vec<Block>>,
    colors: Rc<CodeColorMap>,
}

/// Code block colors by [`code_key`]; `None` for a language without a grammar.
type CodeColorMap = HashMap<u64, Option<Rc<crate::viewer::CodeColors>>>;

fn code_key(language: &str, code: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (language, code).hash(&mut hasher);
    hasher.finish()
}

/// Calls `f` with every code block's language and code, in order.
fn each_code(blocks: &[Block], f: &mut impl FnMut(&str, &str)) {
    for block in blocks {
        match block {
            Block::Code { language: Some(language), code } => f(language, code),
            Block::Quote(children) => each_code(children, f),
            Block::List { items, .. } => items.iter().for_each(|item| each_code(&item.children, f)),
            _ => {}
        }
    }
}

/// The text of every run `blocks` draw, in reading order, as the selection collects it.
fn leaves<'a>(blocks: &'a [Block], out: &mut Vec<&'a str>) {
    for block in blocks {
        match block {
            Block::Paragraph(inline) | Block::Heading(_, inline) => out.push(&inline.text),
            Block::Quote(children) => leaves(children, out),
            Block::List { items, .. } => items.iter().for_each(|item| leaves(&item.children, out)),
            Block::Code { code, .. } => out.push(code),
            Block::Rule => {}
            Block::Table { rows, .. } => out.extend(rows.iter().flatten().map(|cell| cell.text.as_str())),
        }
    }
}

/// The text Select All copies from `sources`, the replies in order, including those scrolled
/// out of view and so not laid out: every run's text, one run a line.
pub fn all_text<'a>(sources: impl IntoIterator<Item = &'a str>) -> String {
    let parsed: Vec<Vec<Block>> = sources.into_iter().map(parse).collect();
    let mut parts = Vec::new();
    for blocks in &parsed {
        leaves(blocks, &mut parts);
    }
    parts.join("\n").trim().to_string()
}

#[derive(Clone)]
enum Picked {
    Range(SharedString, Range<usize>),
    All,
}

fn word_kind(c: char) -> u8 {
    if c == '_' || c.is_alphanumeric() {
        0
    } else if c.is_whitespace() {
        1
    } else {
        2
    }
}

impl Selection {
    /// Forgets the runs of the last frame; call before drawing replies.
    pub fn begin_frame(&mut self) {
        self.runs.clear();
        self.index.clear();
    }

    fn register(&mut self, key: SharedString, run: Run) {
        self.index.insert(key.clone(), self.runs.len());
        self.runs.push((key, run));
    }

    fn run_at(&self, position: Point<Pixels>) -> Option<&(SharedString, Run)> {
        self.runs.iter().find(|(_, run)| run.layout.bounds().contains(&position))
    }

    pub fn mouse_down(&mut self, position: Point<Pixels>, clicks: usize) {
        self.picked = None;
        self.anchor = None;
        self.head = None;
        if clicks >= 2 {
            if let Some((key, run)) = self.run_at(position) {
                let ix = match run.layout.index_for_position(position) {
                    Ok(ix) | Err(ix) => ix,
                };
                let range = if clicks == 2 { word_at(&run.text, ix) } else { 0..run.text.len() };
                self.picked = Some(Picked::Range(key.clone(), range));
            }
            return;
        }
        self.anchor = Some(position);
        self.head = Some(position);
        self.selecting = true;
    }

    pub fn drag(&mut self, position: Point<Pixels>) {
        if self.selecting {
            self.head = Some(position);
        }
    }

    /// Ends a drag; a click that didn't drag returns the link it landed on.
    pub fn mouse_up(&mut self, position: Point<Pixels>) -> Option<String> {
        self.selecting = false;
        if self.anchor != self.head {
            return None;
        }
        self.anchor = None;
        self.head = None;
        let (_, run) = self.run_at(position)?;
        let ix = run.layout.index_for_position(position).ok()?;
        run.links.iter().find(|(r, _)| r.contains(&ix)).map(|(_, url)| url.clone())
    }

    pub fn select_all(&mut self) {
        self.picked = Some(Picked::All);
    }

    pub fn is_all(&self) -> bool {
        matches!(self.picked, Some(Picked::All))
    }

    /// Forgets parsed replies, for a new thread.
    pub fn clear(&mut self) {
        self.parsed.clear();
        self.picked = None;
    }

    /// The part of a run that's selected, from where the drag points were last frame.
    fn range_for(&self, key: &SharedString) -> Option<Range<usize>> {
        match &self.picked {
            Some(Picked::All) => return self.index.get(key).map(|&ix| 0..self.runs[ix].1.text.len()),
            Some(Picked::Range(k, range)) => return (k == key).then(|| range.clone()),
            None => {}
        }
        let (a, b) = (self.anchor?, self.head?);
        let run = &self.runs[*self.index.get(key)?].1;
        let bounds = run.layout.bounds();
        let line = run.layout.line_height();
        // The earlier point in reading order: higher up, or further left on the same line.
        let (first, last) =
            if (a.y - b.y).abs() < line / 2. { if a.x <= b.x { (a, b) } else { (b, a) } } else if a.y < b.y { (a, b) } else { (b, a) };
        if last.y < bounds.top() || first.y > bounds.bottom() {
            return None;
        }
        let ix = |p: Point<Pixels>| match run.layout.index_for_position(p) {
            Ok(ix) | Err(ix) => ix.min(run.text.len()),
        };
        let start = if first.y < bounds.top() { 0 } else { ix(first) };
        let end = if last.y > bounds.bottom() { run.text.len() } else { ix(last) };
        (start < end).then_some(start..end)
    }

    /// The selected text of every run, one run a line.
    pub fn selected_text(&self) -> String {
        let parts: Vec<&str> = self
            .runs
            .iter()
            .filter_map(|(key, run)| self.range_for(key).map(|r| &run.text[r]))
            .collect();
        parts.join("\n").trim().to_string()
    }
}

fn word_at(text: &str, ix: usize) -> Range<usize> {
    let mut ix = ix.min(text.len());
    while !text.is_char_boundary(ix) {
        ix -= 1;
    }
    let Some(c) = text[ix..].chars().next().or_else(|| text[..ix].chars().next_back()) else { return 0..0 };
    let kind = word_kind(c);
    let start = text[..ix].char_indices().rev().take_while(|(_, c)| word_kind(*c) == kind).last().map_or(ix, |(i, _)| i);
    let end = text[ix..].char_indices().find(|(_, c)| word_kind(*c) != kind).map_or(text.len(), |(i, _)| ix + i);
    start..end
}

/// Renders `source` as a reply. `id` must be unique among replies shown together.
pub fn markdown(id: impl Into<ElementId>, source: &str, selection: &Entity<Selection>, window: &mut Window, cx: &mut App) -> AnyElement {
    render(id.into(), source, selection, false, window, cx)
}

/// Like [`markdown`], with the text in italics, as for the agent's thoughts.
pub fn italic(id: impl Into<ElementId>, source: &str, selection: &Entity<Selection>, window: &mut Window, cx: &mut App) -> AnyElement {
    render(id.into(), source, selection, true, window, cx)
}

fn render(id: ElementId, source: &str, selection: &Entity<Selection>, italic: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let (blocks, colors) = selection.update(cx, |s, _| match s.parsed.get(&id) {
        Some(parsed) if parsed.source == source => (parsed.blocks.clone(), parsed.colors.clone()),
        old => {
            let blocks = Rc::new(parse(source));
            // A reply grows while it streams; code blocks it already finished keep their colors.
            let old = old.map(|parsed| parsed.colors.clone());
            let mut colors = CodeColorMap::new();
            each_code(&blocks, &mut |language, code| {
                let key = code_key(language, code);
                let reused = old.as_ref().and_then(|old| old.get(&key).cloned());
                colors.entry(key).or_insert_with(|| reused.unwrap_or_else(|| crate::viewer::CodeColors::new(code, language).map(Rc::new)));
            });
            let colors = Rc::new(colors);
            s.parsed.insert(id.clone(), Parsed { source: source.to_string(), blocks: blocks.clone(), colors: colors.clone() });
            (blocks, colors)
        }
    });
    let mut style = window.text_style();
    if italic {
        style.font_style = FontStyle::Italic;
    }
    let mut cx2 = Render { id: format!("{id:?}"), leaf: 0, selection: selection.clone(), style, colors };
    let count = blocks.len();
    div()
        .children(blocks.iter().enumerate().map(|(ix, b)| cx2.block(b, ix + 1 == count, false, 0, window, cx)))
        .into_any_element()
}

struct Render {
    id: String,
    leaf: usize,
    selection: Entity<Selection>,
    /// The text style blocks around the current one set, which the text runs carry.
    style: gpui::TextStyle,
    colors: Rc<CodeColorMap>,
}

fn gapped(element: AnyElement, last: bool) -> AnyElement {
    if last { element } else { div().pb(rems(1.)).child(element).into_any_element() }
}

impl Render {
    /// Renders `body` with the text style changed by `change`.
    fn styled<R>(&mut self, change: impl FnOnce(&mut gpui::TextStyle), body: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.style.clone();
        change(&mut self.style);
        let out = body(self);
        self.style = saved;
        out
    }

    fn block(&mut self, block: &Block, last: bool, in_list: bool, depth: usize, window: &mut Window, cx: &mut App) -> AnyElement {
        let gap_last = last || in_list;
        let palette = cx.global::<Palette>();
        let (border, muted) = (palette.border, palette.muted_foreground);
        let (code_bg, mono, mono_size, radius) =
            (palette.muted, palette.mono_font_family.clone(), palette.mono_font_size, palette.radius);
        match block {
            Block::Paragraph(inline) => gapped(self.text(inline, window, cx), gap_last),
            Block::Heading(level, inline) => {
                let (size, weight) = match level {
                    1 => (2., FontWeight::BOLD),
                    2 => (1.5, FontWeight::SEMIBOLD),
                    3 => (1.25, FontWeight::SEMIBOLD),
                    4 => (1.125, FontWeight::SEMIBOLD),
                    5 => (1., FontWeight::BOLD),
                    6 => (0.9, FontWeight::SEMIBOLD),
                    _ => (1., FontWeight::NORMAL),
                };
                // Headings scale from the base text size, like everything else.
                div()
                    .pb(rems(0.3))
                    .whitespace_normal()
                    .text_size(rems(size))
                    .font_weight(weight)
                    .child(self.styled(
                        |s| {
                            s.font_weight = weight;
                            if *level == 6 {
                                s.color = muted;
                            }
                        },
                        |this| this.text(inline, window, cx),
                    ))
                    .into_any_element()
            }
            Block::Quote(children) => {
                let count = children.len();
                let quote = div()
                    .w_full()
                    .text_color(muted)
                    .border_l_3()
                    .border_color(border)
                    .px_4()
                    .children(self.styled(
                        |s| s.color = muted,
                        |this| -> Vec<AnyElement> {
                            children.iter().enumerate().map(|(ix, c)| this.block(c, ix + 1 == count, false, depth, window, cx)).collect()
                        },
                    ))
                    .into_any_element();
                gapped(quote, gap_last)
            }
            Block::List { ordered, start, items } => div()
                .w_full()
                .min_w_0()
                .when(!gap_last, |this| this.pb(rems(1.)))
                .children(items.iter().enumerate().map(|(ix, item)| self.item(item, ix, *ordered, *start, depth, window, cx)))
                .into_any_element(),
            Block::Code { language, code } => {
                let highlights = language
                    .as_deref()
                    .and_then(|language| self.colors.get(&code_key(language, code))?.as_ref())
                    .map(|colors| colors.styles(cx))
                    .unwrap_or_default();
                let text = Inline { text: code.clone(), ..Default::default() };
                let leaf = self.styled(
                    |s| {
                        s.font_family = mono.clone();
                        s.font_weight = FontWeight::NORMAL;
                        s.font_style = FontStyle::Normal;
                    },
                    |this| this.leaf(&text, highlights, window, cx),
                );
                gapped(
                    div()
                        .w_full()
                        .min_w_0()
                        .p_3()
                        .bg(code_bg)
                        .font_family(mono)
                        .text_size(mono_size)
                        .relative()
                        .rounded(radius)
                        .child(leaf)
                        .into_any_element(),
                    last,
                )
            }
            Block::Rule => gapped(div().bg(border).h(px(2.)).into_any_element(), gap_last),
            Block::Table { align, rows } => self.table(align, rows, gap_last, window, cx),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn item(&mut self, item: &Item, ix: usize, ordered: bool, start: Option<u32>, depth: usize, window: &mut Window, cx: &mut App) -> AnyElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        let foreground = cx.global::<Palette>().foreground;
        let line_height = window.line_height();
        for (child_ix, child) in item.children.iter().enumerate() {
            let content = self.block(child, true, true, depth + 1, window, cx);
            match child {
                Block::List { .. } => rows.push(div().ml(rems(1.)).child(content).into_any_element()),
                _ if child_ix > 0 => rows.push(div().w_full().min_w_0().pl(rems(1.)).overflow_hidden().child(content).into_any_element()),
                _ => {
                    let marker: AnyElement = match item.checked {
                        Some(checked) => div()
                            .flex()
                            .mr_1p5()
                            .h(line_height)
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .flex()
                                    .size(rems(0.875))
                                    .items_center()
                                    .justify_center()
                                    .border_1()
                                    .border_color(foreground)
                                    .when(checked, |this| this.bg(foreground)),
                            )
                            .into_any_element(),
                        None => div().child(prefix(ix, start, ordered, depth)).into_any_element(),
                    };
                    rows.push(
                        div()
                            .flex()
                            .w_full()
                            .min_w_0()
                            .relative()
                            .items_start()
                            .child(marker)
                            .child(div().flex_1().min_w_0().overflow_hidden().child(content))
                            .into_any_element(),
                    );
                }
            }
        }
        div().w_full().min_w_0().children(rows).into_any_element()
    }

    fn table(&mut self, align: &[AlignKind], rows: &[Vec<Inline>], last: bool, window: &mut Window, cx: &mut App) -> AnyElement {
        let palette = cx.global::<Palette>();
        let (border, head_bg, head_fg, surface) = (palette.border, palette.table_head, palette.table_head_foreground, palette.popover);
        let columns = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let lens: Vec<usize> = (0..columns)
            .map(|c| rows.iter().filter_map(|r| r.get(c)).map(|cell| cell.text.chars().count()).max().unwrap_or(0).min(MAX_COLUMN))
            .collect();
        let count = rows.len();
        let rendered: Vec<AnyElement> = rows
            .iter()
            .enumerate()
            .map(|(row_ix, row)| {
                let cells: Vec<AnyElement> = row
                    .iter()
                    .enumerate()
                    .map(|(ix, cell)| {
                        let text = if row_ix == 0 {
                            self.styled(|s| s.color = head_fg, |this| this.text(cell, window, cx))
                        } else {
                            self.text(cell, window, cx)
                        };
                        div()
                            .overflow_hidden()
                            .when(align.get(ix) == Some(&AlignKind::Center), |this| this.text_center())
                            .when(align.get(ix) == Some(&AlignKind::Right), |this| this.text_right())
                            .min_w_16()
                            .w(relative(lens.get(ix).copied().unwrap_or(MAX_COLUMN) as f32))
                            .px_2()
                            .py_1()
                            .when(ix + 1 < row.len(), |this| this.border_r_1().border_color(border))
                            .child(text)
                            .into_any_element()
                    })
                    .collect();
                div()
                    .w_full()
                    .when(row_ix + 1 < count, |this| this.border_b_1())
                    .border_color(border)
                    .flex()
                    .flex_row()
                    .when(row_ix == 0, |this| this.bg(head_bg).text_color(head_fg))
                    .children(cells)
                    .into_any_element()
            })
            .collect();
        div()
            .when(!last, |this| this.pb(rems(1.)))
            .w_full()
            .child(div().w_full().bg(surface).border_1().border_color(border).overflow_hidden().children(rendered))
            .into_any_element()
    }

    fn text(&mut self, inline: &Inline, window: &mut Window, cx: &mut App) -> AnyElement {
        self.leaf(inline, Vec::new(), window, cx)
    }

    /// One run of selectable text, with its marks, links, extra colors and selection.
    fn leaf(&mut self, inline: &Inline, extra: Vec<(Range<usize>, HighlightStyle)>, _window: &mut Window, cx: &mut App) -> AnyElement {
        let key: SharedString = format!("{}/{}", self.id, self.leaf).into();
        self.leaf += 1;
        let palette = cx.global::<Palette>();
        let text: SharedString = inline.text.clone().into();
        let selected = self.selection.read(cx).range_for(&key);
        let base = self.style.clone();
        let mono = palette.mono_font_family.clone();

        // Each byte's style, built up from the marks, links, colors and selection.
        let mut styles: Vec<(Range<usize>, HighlightStyle, bool)> = Vec::new();
        for (range, mark) in &inline.marks {
            let mut style = HighlightStyle::default();
            if mark.bold {
                style.font_weight = Some(FontWeight::BOLD);
            }
            if mark.italic {
                style.font_style = Some(FontStyle::Italic);
            }
            if mark.strike {
                style.strikethrough = Some(StrikethroughStyle { thickness: px(1.), ..Default::default() });
            }
            if mark.code {
                style.background_color = Some(palette.accent);
            }
            styles.push((range.clone(), style, mark.code));
        }
        for (range, _) in &inline.links {
            let style = HighlightStyle {
                color: Some(palette.link),
                underline: Some(UnderlineStyle { thickness: px(1.), ..Default::default() }),
                ..Default::default()
            };
            styles.push((range.clone(), style, false));
        }
        styles.extend(extra.into_iter().map(|(r, s)| (r, s, false)));
        if let Some(range) = selected {
            styles.push((range, HighlightStyle { background_color: Some(palette.selection), ..Default::default() }, false));
        }
        let runs = runs(&text, &base, &mono, &styles);
        let links = Rc::new(inline.links.clone());

        let styled = StyledText::new(text.clone()).with_runs(runs);
        let layout = styled.layout().clone();
        let selection = self.selection.clone();
        div()
            .relative()
            .child(styled)
            // After the text is laid out, record where it went for selection next frame.
            .child(
                canvas(
                    move |_, _, cx| selection.update(cx, |s, _| s.register(key, Run { text, layout, links })),
                    |_: Bounds<Pixels>, _, _, _| {},
                )
                .absolute()
                .size_0(),
            )
            .into_any_element()
    }
}

/// Splits `text` into runs where its styles change, layering them in order.
fn runs(text: &str, base: &gpui::TextStyle, mono: &SharedString, styles: &[(Range<usize>, HighlightStyle, bool)]) -> Vec<TextRun> {
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for (range, _, _) in styles {
        cuts.push(range.start.min(text.len()));
        cuts.push(range.end.min(text.len()));
    }
    cuts.retain(|&c| text.is_char_boundary(c));
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .filter(|w| w[0] < w[1])
        .map(|w| {
            let mut style = base.clone();
            for (range, highlight, code) in styles {
                if range.start <= w[0] && w[1] <= range.end {
                    style = style.highlight(*highlight);
                    if *code {
                        style.font_family = mono.clone();
                    }
                }
            }
            style.to_run(w[1] - w[0])
        })
        .collect()
}

fn prefix(ix: usize, start: Option<u32>, ordered: bool, depth: usize) -> String {
    if !ordered {
        return format!("{} ", BULLETS[depth.min(BULLETS.len() - 1)]);
    }
    let ordinal = start.unwrap_or(1) as usize + ix;
    if depth == 0 || start == Some(0) {
        return format!("{ordinal}. ");
    }
    let letters = if depth == 1 { UPPER } else { LOWER };
    let letter = letters.chars().nth(ordinal.saturating_sub(1) % letters.len()).unwrap_or('a');
    format!("{letter}. ")
}

#[cfg(test)]
mod tests {
    use super::{Block, parse, prefix, word_at};

    #[test]
    fn soft_breaks_reflow_and_marks_land_on_text() {
        let blocks = parse("Some *emphasis* and `code`\nnext line.");
        let [Block::Paragraph(p)] = blocks.as_slice() else { panic!("one paragraph") };
        assert_eq!(p.text, "Some emphasis and code next line.");
        assert_eq!(p.marks.len(), 2);
        assert_eq!(&p.text[p.marks[1].0.clone()], "code");
    }

    #[test]
    fn links_and_lists() {
        let blocks = parse("- a [link](http://x.y)\n  1. one\n");
        let [Block::List { items, .. }] = blocks.as_slice() else { panic!("a list") };
        let Block::Paragraph(p) = &items[0].children[0] else { panic!("paragraph") };
        assert_eq!(p.links, [(2..6, "http://x.y".to_string())]);
        assert!(matches!(items[0].children[1], Block::List { ordered: true, .. }));
    }

    #[test]
    fn list_prefixes_follow_the_kit() {
        assert_eq!(prefix(0, None, false, 0), "• ");
        assert_eq!(prefix(1, None, false, 1), "◦ ");
        assert_eq!(prefix(2, Some(1), true, 0), "3. ");
        assert_eq!(prefix(0, Some(1), true, 1), "A. ");
        assert_eq!(prefix(1, Some(1), true, 2), "b. ");
    }

    #[test]
    fn select_all_takes_every_run() {
        let text = super::all_text(["# Hi\n\n- one\n- two\n\n```rs\nfn x() {}\n```\n", "| a | b |\n|---|---|\n| c | d |\n"]);
        assert_eq!(text, "Hi\none\ntwo\nfn x() {}\na\nb\nc\nd");
    }

    #[test]
    fn double_click_takes_a_word() {
        assert_eq!(word_at("hello big_world!", 8), 6..15);
        assert_eq!(word_at("a  b", 1), 1..3);
    }
}
