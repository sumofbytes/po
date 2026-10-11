//! Lays out and paints only the rows in view. Sizes and colors match gpui-kit's code editor so
//! the switch is invisible: same paddings, gutter, row height and indent guides.
use std::ops::Range;

use gpui::{
    App, Bounds, ContentMask, CursorStyle, Edges, Element, ElementId, Entity, Font,
    GlobalElementId, HighlightStyle, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent, ShapedLine,
    SharedString, Style, TextAlign, TextRun, Window, fill, font, point, px, relative, size,
};

use super::wrap::{self, WrapMap};
use super::{CodeViewState, Geometry};
use crate::theme::Palette;

pub(super) const PADDING: Edges<Pixels> = Edges { top: px(8.), right: px(10.), bottom: px(8.), left: px(6.) };
const LINE_HEIGHT: f32 = 1.5;
/// Space kept right of the text so wrapped rows don't touch the edge.
const RIGHT_MARGIN: Pixels = px(10.);
const NUMBER_MARGIN: Pixels = px(6.);
/// The column right of the numbers where fold chevrons go.
const FOLD_COLUMN: Pixels = px(18.);
const MIN_DIGITS: usize = 3;
const MAX_DIGITS: usize = 7;
/// Rows of more bytes than this are shown uncolored, as in the kit.
const MAX_HIGHLIGHT_LINE: usize = 10_000;
const TAB_SIZE: usize = 2;
/// Rows that can be scrolled past the last line, at least.
const BOTTOM_ROWS: f32 = 3.;


const CHEVRON: Pixels = px(14.);
const CHEVRON_RIGHT: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m9 18 6-6-6-6"/></svg>"#;
const CHEVRON_DOWN: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>"#;

/// A fold toggle in the gutter.
struct Chevron {
    /// Where it can be clicked: the fold column's width, the row's height.
    hit: Bounds<Pixels>,
    icon: Bounds<Pixels>,
    line: usize,
    folded: bool,
    /// Shown without hovering the gutter: on the caret's line or while folded.
    pinned: bool,
}

pub struct CodeElement {
    state: Entity<CodeViewState>,
}

impl CodeElement {
    pub fn new(state: Entity<CodeViewState>) -> Self {
        Self { state }
    }
}

impl IntoElement for CodeElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

struct Row {
    origin: gpui::Point<Pixels>,
    text: ShapedLine,
    number: Option<ShapedLine>,
    active: bool,
}

/// The caret width, whole pixels so it stays sharp off macOS.
const CARET_WIDTH: Pixels = px(2.);
/// The caret's share of the row height, centred in it.
const CARET_HEIGHT: f32 = 0.85;

pub struct Layout {
    hitbox: Hitbox,
    rows: Vec<Row>,
    guides: Vec<Bounds<Pixels>>,
    gutter: Bounds<Pixels>,
    line_height: Pixels,
    selection: Vec<Bounds<Pixels>>,
    caret: Option<Bounds<Pixels>>,
    /// Search matches in view, and whether each is the current one.
    matches: Vec<(Bounds<Pixels>, bool)>,
    chevrons: Vec<Chevron>,
    gutter_hitbox: Hitbox,
}

/// The theme colors the viewer paints with.
struct Colors {
    background: Hsla,
    foreground: Hsla,
    active_line: Option<Hsla>,
    number: Hsla,
    active_number: Hsla,
    guide: Hsla,
    caret: Hsla,
    selection: Hsla,
}

impl Colors {
    fn new(cx: &App) -> Self {
        let palette = cx.global::<Palette>();
        Self {
            background: palette.editor_background,
            foreground: palette.editor_foreground,
            active_line: palette.editor_active_line,
            number: palette.editor_line_number,
            // The kit draws the current line's number in the text color, not `active_line_number`.
            active_number: palette.editor_foreground,
            guide: palette.border.opacity(0.85),
            caret: palette.caret,
            selection: palette.selection,
        }
    }
}

fn run(len: usize, font: &Font, color: Hsla, style: Option<&HighlightStyle>) -> TextRun {
    let mut font = font.clone();
    if let Some(style) = style {
        font.weight = style.font_weight.unwrap_or(font.weight);
        font.style = style.font_style.unwrap_or(font.style);
    }
    let color = style.and_then(|s| s.color).unwrap_or(color);
    TextRun { len, font, color, background_color: None, underline: None, strikethrough: None }
}

/// Wraps every line at `width`. Widths are sums of each character's advance, which for a
/// monospace font is what shaping gives, without shaping every candidate row.
fn wrap(state: &CodeViewState, font: &Font, font_size: Pixels, width: Pixels, window: &Window) -> WrapMap {
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(font);
    let mut ascii = [None::<Pixels>; 128];
    let mut other = std::collections::HashMap::new();
    let mut advance = |c: char| {
        let slot = match ascii.get_mut(c as usize) {
            Some(slot) => slot,
            None => other.entry(c).or_insert(None),
        };
        *slot.get_or_insert_with(|| text_system.advance(font_id, font_size, c).map_or(px(0.), |a| a.width))
    };
    // The width before each byte of the line.
    let mut before = Vec::new();
    WrapMap::build(&state.text, |line| {
        before.clear();
        let mut x = px(0.);
        for c in line.chars() {
            before.extend(std::iter::repeat_n(x, c.len_utf8()));
            x += advance(c);
        }
        before.push(x);
        wrap::breaks(line, width, |r| before[r.end] - before[r.start])
    })
}

/// Indent guide columns for a line, every `TAB_SIZE` columns of its leading whitespace.
fn guide_columns(line: &str) -> Vec<usize> {
    let indent = line
        .chars()
        .map_while(|c| match c {
            ' ' => Some(1),
            '\t' => Some(TAB_SIZE),
            _ => None,
        })
        .sum::<usize>();
    (0..indent).step_by(TAB_SIZE).collect()
}

impl CodeElement {
    #[allow(clippy::too_many_arguments)]
    fn shape_row(
        state: &CodeViewState,
        line: usize,
        segment: Range<usize>,
        styles: &[Option<HighlightStyle>],
        font: &Font,
        font_size: Pixels,
        color: Hsla,
        window: &Window,
    ) -> ShapedLine {
        let range = state.text.line_range(line);
        let text = &state.text.src()[range.start + segment.start..range.start + segment.end];
        let mut runs = Vec::new();
        match &state.highlights {
            Some(highlights) if range.len() <= MAX_HIGHLIGHT_LINE => {
                let mut at = 0;
                for (len, id) in highlights.runs(range.start + segment.start..range.start + segment.end) {
                    // Colors from before a reload can fall inside a character; keep runs whole.
                    let mut end = (at + len).min(text.len());
                    while !text.is_char_boundary(end) {
                        end += 1;
                    }
                    if end > at {
                        let style = id.checked_sub(1).and_then(|i| styles.get(i as usize)?.as_ref());
                        runs.push(run(end - at, font, color, style));
                        at = end;
                    }
                }
                if at < text.len() {
                    runs.push(run(text.len() - at, font, color, None));
                }
            }
            _ => runs.push(run(text.len(), font, color, None)),
        }
        window.text_system().shape_line(SharedString::from(text.to_string()), font_size, &runs, None)
    }
}

impl Element for CodeElement {
    type RequestLayoutState = ();
    type PrepaintState = Layout;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size = size(relative(1.).into(), relative(1.).into());
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Layout {
        let colors = Colors::new(cx);
        let palette = cx.global::<Palette>();
        let font = font(palette.mono_font_family.clone());
        let font_size = palette.mono_font_size;
        let styles: Vec<Option<HighlightStyle>> = self
            .state
            .read(cx)
            .highlights
            .as_ref()
            .map(|h| h.names.iter().map(|name| palette.syntax(name)).collect())
            .unwrap_or_default();
        let line_height = font_size * LINE_HEIGHT;
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let state = self.state.clone();

        state.update(cx, |state, _| {
            let lines = state.text.lines();
            let digits = lines.to_string().len().clamp(MIN_DIGITS, MAX_DIGITS);
            let digit_width = window
                .text_system()
                .shape_line("+".repeat(digits).into(), font_size, &[run(digits, &font, colors.number, None)], None)
                .width;
            let gutter_width = digit_width + NUMBER_MARGIN + FOLD_COLUMN;
            let text_x = bounds.left() + PADDING.left + gutter_width;
            let wrap_width = (bounds.size.width - PADDING.left - PADDING.right - gutter_width - RIGHT_MARGIN).max(px(1.));

            let key = (wrap_width, font_size, font.family.clone());
            if state.wrap_key.as_ref() != Some(&key) || state.line_height != line_height {
                // Keep the line at the top of the view in place across the rewrap.
                let anchor = state.anchor.take().unwrap_or_else(|| state.top());
                let started = std::time::Instant::now();
                state.wrap = wrap(state, &font, font_size, wrap_width, window);
                state.apply_folds();
                crate::trace::mark(&format!("viewer wrapped at {wrap_width:?} in {:?}", started.elapsed()));
                state.wrap_key = Some(key);
                state.line_height = line_height;
                state.restore(anchor);
            }

            let rows = state.wrap.rows();
            let viewport = bounds.size.height;
            let bottom = (viewport / 2.).max(line_height * BOTTOM_ROWS);
            let content = PADDING.top + line_height * rows as f32 + bottom.max(PADDING.bottom);
            state.max_scroll = (content - viewport).max(px(0.));
            state.scroll_y = state.scroll_y.clamp(px(0.), state.max_scroll);
            let space = window
                .text_system()
                .shape_line(" ".into(), font_size, &[run(1, &font, colors.foreground, None)], None)
                .width;
            state.geometry =
                Some(Geometry { bounds, text_x, line_height, space, font: font.clone(), font_size });
            state.follow_caret(viewport);

            let selected = state.selection.range();
            let head = state.cursor();
            let caret_row = state.row_of(head, state.affinity);
            let active_line = state.text.line_of(head);
            let first = (((state.scroll_y - PADDING.top) / line_height).floor().max(0.)) as usize;
            let top = bounds.top() + PADDING.top - state.scroll_y;
            let mut selection = Vec::new();
            let mut caret = None;
            let mut matches = Vec::new();
            let mut chevrons = Vec::new();
            let search = state.search_open.then_some(&state.matches);

            let mut out = Vec::new();
            let mut guides = Vec::new();
            let mut last_guides: Vec<usize> = Vec::new();
            let mut row = first;
            while row < rows {
                let y = top + line_height * row as f32;
                if y >= bounds.bottom() {
                    break;
                }
                let (line, sub) = state.wrap.line_of_row(row);
                let line_text = state.text.line(line);
                let (segment, indent) = state.wrap.segment(line, sub, line_text.len());
                let text = Self::shape_row(state, line, segment.clone(), &styles, &font, font_size, colors.foreground, window);
                let number = (sub == 0).then(|| {
                    let label = format!("{:>digits$}", (line + 1).min(9_999_999));
                    let color = if line == active_line { colors.active_number } else { colors.number };
                    window.text_system().shape_line(label.into(), font_size, &[run(digits, &font, color, None)], None)
                });
                if sub == 0 {
                    let columns = if line_text.is_empty() { last_guides.clone() } else { guide_columns(line_text) };
                    for &column in &columns {
                        let x = text_x + space * column as f32;
                        guides.push(Bounds::new(point(x, y), size(px(1.), line_height)));
                    }
                    last_guides = columns;
                }
                let origin = point(text_x + space * indent as f32, y);
                let start = state.text.line_range(line).start;
                let (range, last) = (start + segment.start..start + segment.end, sub == state.wrap.breaks(line).len());
                // A selected line break shows as one space after the line, even on empty lines.
                let newline = last && selected.start <= range.end && selected.end > range.end;
                let (from, to) = (selected.start.max(range.start), selected.end.min(range.end));
                if from < to || newline {
                    let left = text.x_for_index(from.min(range.end) - range.start);
                    let right = if newline { text.width + space } else { text.x_for_index(to - range.start) };
                    selection.push(Bounds::from_corners(point(origin.x + left, y), point(origin.x + right, y + line_height)));
                }
                if sub == 0 && let Some(_) = state.fold_at(line) {
                    let folded = state.folded.iter().any(|&(start, _)| start == line);
                    let x = bounds.left() + PADDING.left + digit_width;
                    let hit = Bounds::new(point(x, y), size(FOLD_COLUMN, line_height));
                    let inset = point((FOLD_COLUMN - CHEVRON) / 2., (line_height - CHEVRON) / 2.);
                    let icon = Bounds::new(hit.origin + inset, size(CHEVRON, CHEVRON));
                    chevrons.push(Chevron { hit, icon, line, folded, pinned: folded || line == active_line });
                }
                if let Some(search) = search {
                    let first = search.ranges.partition_point(|m| m.end <= range.start);
                    for (ix, m) in search.ranges.iter().enumerate().skip(first).take_while(|(_, m)| m.start < range.end) {
                        let left = text.x_for_index(m.start.max(range.start) - range.start);
                        let right = text.x_for_index(m.end.min(range.end) - range.start);
                        let bounds = Bounds::from_corners(point(origin.x + left, y), point(origin.x + right, y + line_height));
                        matches.push((bounds, ix == search.current));
                    }
                }
                if row == caret_row {
                    let x = origin.x + text.x_for_index(head.clamp(range.start, range.end) - range.start);
                    let height = line_height * CARET_HEIGHT;
                    caret = Some(Bounds::new(point(x, y + (line_height - height) / 2.), size(CARET_WIDTH, height)));
                }
                out.push(Row { origin, text, number, active: line == active_line });
                row += 1;
            }


            let gutter = Bounds::new(bounds.origin, size(PADDING.left + gutter_width, bounds.size.height));
            let gutter_hitbox = window.insert_hitbox(gutter, HitboxBehavior::Normal);
            Layout { hitbox, rows: out, guides, gutter, line_height, selection, caret, matches, chevrons, gutter_hitbox }
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        layout: &mut Layout,
        window: &mut Window,
        cx: &mut App,
    ) {
        let colors = Colors::new(cx);
        let line_height = layout.line_height;
        window.set_cursor_style(CursorStyle::IBeam, &layout.hitbox);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(fill(bounds, colors.background));
            if let Some(color) = colors.active_line {
                // Like the kit's, the band stops at the right padding.
                for row in layout.rows.iter().filter(|row| row.active) {
                    window.paint_quad(fill(
                        Bounds::new(point(bounds.left(), row.origin.y), size(bounds.size.width - PADDING.right, line_height)),
                        color,
                    ));
                }
            }
            for guide in &layout.guides {
                window.paint_quad(fill(*guide, colors.guide));
            }
            let state = self.state.read(cx);
            let focused = state.focus.is_focused(window);
            // Matches are tinted with a faded selection color, the current one with the full color.
            if window.is_window_active() {
                let faded = Hsla { s: 0.1, ..colors.selection };
                for (bounds, current) in &layout.matches {
                    window.paint_quad(fill(*bounds, faded));
                    if *current {
                        window.paint_quad(fill(*bounds, colors.selection));
                    }
                }
            }
            // Like the kit's, the selection only shows while the view has focus.
            if focused && window.is_window_active() {
                for bounds in &layout.selection {
                    window.paint_quad(fill(*bounds, colors.selection));
                }
            }
            let show_caret = focused && window.is_window_active() && state.caret_visible();
            for row in &layout.rows {
                row.text.paint(row.origin, line_height, TextAlign::Left, None, window, cx).ok();
            }
            if let Some(caret) = layout.caret.filter(|_| show_caret) {
                window.paint_quad(fill(caret, colors.caret));
            }
            // The gutter sits over the text, so wrapped rows never show through it.
            window.paint_quad(fill(layout.gutter, colors.background));
            for row in &layout.rows {
                if let (true, Some(color)) = (row.active, colors.active_line) {
                    let band = Bounds::new(point(layout.gutter.left(), row.origin.y), size(layout.gutter.size.width, line_height));
                    window.paint_quad(fill(band, color));
                }
                if let Some(number) = &row.number {
                    let origin = point(bounds.left() + PADDING.left, row.origin.y);
                    number.paint(origin, line_height, TextAlign::Left, None, window, cx).ok();
                }
            }
            // Like the kit's, fold toggles show while the gutter is hovered, and otherwise only on
            // the caret's line and on folded lines.
            let gutter_hovered = layout.gutter_hitbox.is_hovered(window);
            for chevron in layout.chevrons.iter().filter(|c| c.pinned || gutter_hovered) {
                let (path, svg) = if chevron.folded {
                    ("viewer-fold-chevron-right", CHEVRON_RIGHT)
                } else {
                    ("viewer-fold-chevron-down", CHEVRON_DOWN)
                };
                window
                    .paint_svg(chevron.icon, path.into(), Some(svg), gpui::TransformationMatrix::default(), colors.foreground, cx)
                    .ok();
            }
        });

        let hitbox = layout.hitbox.clone();
        let state = self.state.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase.bubble() && hitbox.should_handle_scroll(window) {
                let delta = event.delta.pixel_delta(line_height);
                // Only a scroll that moved keeps the event; at an end it goes to whatever holds the view.
                if state.update(cx, |state, cx| state.scroll_to(state.scroll_y - delta.y, cx)) {
                    cx.stop_propagation();
                }
            }
        });

        let hitbox = layout.hitbox.clone();
        let state = self.state.clone();
        let toggles: Vec<(Bounds<Pixels>, usize)> = layout.chevrons.iter().map(|c| (c.hit, c.line)).collect();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if !phase.bubble() || !hitbox.is_hovered(window) {
                return;
            }
            let toggle = toggles.iter().find(|(hit, _)| event.button == MouseButton::Left && hit.contains(&event.position));
            match toggle {
                Some(&(_, line)) => {
                    state.update(cx, |state, cx| state.toggle_fold(line, cx));
                    cx.stop_propagation();
                }
                None => state.update(cx, |state, cx| state.mouse_down(event, window, cx)),
            }
        });
        // Show or hide the hover toggles as the pointer enters or leaves the gutter.
        let gutter = layout.gutter_hitbox.clone();
        let state = self.state.clone();
        window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
            if !phase.bubble() {
                return;
            }
            let hovered = gutter.is_hovered(window);
            state.update(cx, |state, cx| {
                if state.gutter_hovered != hovered {
                    state.gutter_hovered = hovered;
                    cx.notify();
                }
            });
        });
        let state = self.state.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase.bubble() && event.pressed_button == Some(MouseButton::Left) {
                state.update(cx, |state, cx| state.mouse_drag(event.position, window, cx));
            }
        });
        let state = self.state.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase.bubble() && state.read(cx).selecting {
                state.update(cx, |state, cx| state.mouse_up(event.button, window, cx));
            }
        });

    }
}

#[cfg(test)]
mod tests {
    use super::guide_columns;

    #[test]
    fn guides_every_two_columns_of_indent() {
        assert_eq!(guide_columns("fn x"), Vec::<usize>::new());
        assert_eq!(guide_columns("    x"), [0, 2]);
        assert_eq!(guide_columns("\t x"), [0, 2]);
    }
}
