//! The caret and selection: mapping text offsets to rows and pixels and back, and every way of
//! moving them, matching gpui-kit's editor key for key.
use std::ops::Range;
use std::time::Duration;

use gpui::{
    ClipboardItem, Context, Font, MouseButton, MouseDownEvent, Pixels, Point, ShapedLine, TextRun, Window, px,
};

use super::element::PADDING;
use super::*;

const BLINK: Duration = Duration::from_millis(500);
/// How long the caret stays solid after it moves before blinking again.
const BLINK_PAUSE: Duration = Duration::from_millis(300);
const AUTO_SCROLL_TICK: Duration = Duration::from_millis(16);

/// What the element measured in its last layout, for turning pointer positions and caret
/// moves into offsets between frames.
#[derive(Clone)]
pub(super) struct Geometry {
    pub bounds: gpui::Bounds<Pixels>,
    /// Window x of the text column, before any continuation-row indent.
    pub text_x: Pixels,
    pub line_height: Pixels,
    pub space: Pixels,
    pub font: Font,
    pub font_size: Pixels,
}

/// The selection, as where it started and where the caret is. Empty when they're equal.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub(super) struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// How fast dragging past an edge scrolls, ported from the kit: from 16px inside the edge,
/// ramping from 12 to 64px a tick over the next 96px.
fn auto_scroll_delta(y: Pixels, bounds: gpui::Bounds<Pixels>) -> Option<Pixels> {
    const MIN: f32 = 12.;
    const MAX: f32 = 64.;
    const INNER: f32 = 16.;
    const RAMP: f32 = 80.;
    let (top, bottom) = (bounds.top() + px(INNER), bounds.bottom() - px(INNER));
    let speed = |past: Pixels| px(MIN + (past / px(INNER + RAMP)).min(1.) * (MAX - MIN));
    if y > bottom {
        Some(speed(y - bottom))
    } else if y < top {
        Some(-speed(top - y))
    } else {
        None
    }
}

impl CodeViewState {
    pub(super) fn cursor(&self) -> usize {
        self.selection.head
    }

    /// The display row `offset` is drawn on. At a soft wrap the offset ends one row and starts
    /// the next; `affinity` picks the end of the earlier row.
    pub(super) fn row_of(&self, offset: usize, affinity: bool) -> usize {
        let line = self.text.line_of(offset);
        // A folded-away line shows nowhere; take the row after the fold.
        if self.wrap.is_hidden(line) {
            return self.wrap.first_row(line);
        }
        let column = offset - self.text.line_range(line).start;
        let breaks = self.wrap.breaks(line);
        let mut sub = breaks.partition_point(|b| b.ix as usize <= column);
        if affinity && sub > 0 && breaks[sub - 1].ix as usize == column {
            sub -= 1;
        }
        self.wrap.first_row(line) + sub
    }

    /// The text of a display row: its offsets and how many columns it's indented by.
    pub(super) fn row_range(&self, row: usize) -> (Range<usize>, u32) {
        let (line, sub) = self.wrap.line_of_row(row);
        let range = self.text.line_range(line);
        let (segment, indent) = self.wrap.segment(line, sub, range.len());
        (range.start + segment.start..range.start + segment.end, indent)
    }

    /// Whether the row ends at a soft wrap rather than the line's end.
    fn wraps_after(&self, row: usize) -> bool {
        let (line, sub) = self.wrap.line_of_row(row);
        sub < self.wrap.breaks(line).len()
    }

    fn shape(&self, range: Range<usize>, geometry: &Geometry, window: &Window) -> ShapedLine {
        let text = &self.text.src()[range];
        let run = TextRun {
            len: text.len(),
            font: geometry.font.clone(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window.text_system().shape_line(text.to_string().into(), geometry.font_size, &[run], None)
    }

    /// The caret's x for `offset`, from the text column's left edge.
    fn x_of(&self, offset: usize, affinity: bool, window: &Window) -> Pixels {
        let Some(geometry) = self.geometry.clone() else { return px(0.) };
        let (range, indent) = self.row_range(self.row_of(offset, affinity));
        let shaped = self.shape(range.clone(), &geometry, window);
        geometry.space * indent as f32 + shaped.x_for_index(offset.clamp(range.start, range.end) - range.start)
    }

    /// The offset nearest `x` on a display row, and whether it sits at the row's wrapped end.
    fn offset_in_row(&self, row: usize, x: Pixels, window: &Window) -> (usize, bool) {
        let Some(geometry) = self.geometry.clone() else { return (0, false) };
        let (range, indent) = self.row_range(row);
        let shaped = self.shape(range.clone(), &geometry, window);
        let ix = shaped.closest_index_for_x(x - geometry.space * indent as f32);
        let offset = self.text.clip_left(range.start + ix.min(range.len()));
        (offset, offset == range.end && self.wraps_after(row))
    }

    /// The offset under a window position, clamped to the text.
    pub(super) fn offset_at(&self, position: Point<Pixels>, window: &Window) -> (usize, bool) {
        let Some(geometry) = self.geometry.clone() else { return (0, false) };
        let top = geometry.bounds.top() + PADDING.top - self.scroll_y;
        let row = ((position.y - top) / geometry.line_height).floor().max(0.) as usize;
        self.offset_in_row(row.min(self.wrap.rows() - 1), position.x - geometry.text_x, window)
    }

    /// Collapses the selection to `offset`.
    fn move_to(&mut self, offset: usize, affinity: bool, goal: Option<Pixels>, cx: &mut Context<Self>) {
        self.selection = Selection { anchor: offset, head: offset };
        self.affinity = affinity;
        self.goal = goal;
        self.moved(cx);
    }

    /// Moves the caret end of the selection to `offset`. While a double-click drag is going,
    /// the selection keeps covering the word it started on.
    fn select_to(&mut self, offset: usize, affinity: bool, goal: Option<Pixels>, cx: &mut Context<Self>) {
        self.selection.head = offset;
        if let Some(word) = self.word.clone() {
            if offset < word.start {
                self.selection.anchor = word.end;
            } else {
                self.selection.anchor = word.start;
                self.selection.head = offset.max(word.end);
            }
        }
        self.affinity = affinity;
        self.goal = goal;
        self.moved(cx);
    }

    fn moved(&mut self, cx: &mut Context<Self>) {
        self.follow = true;
        self.pause_blink(cx);
        cx.notify();
    }

    /// Where Up and Down land `rows` display rows from `offset`, keeping to the goal column.
    fn vertical(&self, offset: usize, affinity: bool, rows: isize, goal: Pixels, window: &Window) -> (usize, bool, bool) {
        let row = self.row_of(offset, affinity);
        let target = (row as isize + rows).clamp(0, self.wrap.rows() as isize - 1) as usize;
        let (offset, affinity) = self.offset_in_row(target, goal, window);
        (offset, affinity, target == row)
    }

    fn goal(&self, offset: usize, affinity: bool, window: &Window) -> Pixels {
        self.goal.unwrap_or_else(|| self.x_of(offset, affinity, window))
    }

    fn move_vertical(&mut self, rows: isize, collapse: bool, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selection;
        let (from, affinity, goal) = if selection.is_empty() || !collapse {
            (selection.head, self.affinity, self.goal(selection.head, self.affinity, window))
        } else {
            // A selection first collapses to the side it's moving away from.
            let from = if rows < 0 { selection.range().start } else { selection.range().end };
            let affinity = rows > 0 && self.at_row_end(from);
            (from, affinity, self.x_of(from, affinity, window))
        };
        let (offset, affinity, _) = self.vertical(from, affinity, rows, goal, window);
        self.move_to(offset, affinity, Some(goal), cx);
    }

    fn select_vertical(&mut self, rows: isize, window: &mut Window, cx: &mut Context<Self>) {
        let head = self.selection.head;
        let goal = self.goal(head, self.affinity, window);
        let (offset, affinity, same_row) = self.vertical(head, self.affinity, rows, goal, window);
        // With no row further, selecting reaches the start or end of the text.
        let (offset, affinity) = if same_row { (if rows < 0 { 0 } else { self.text.len() }, false) } else { (offset, affinity) };
        self.select_to(offset, affinity, Some(goal), cx);
    }

    /// Whether `offset` is a soft wrap point, where a caret coming from the left stays on the
    /// earlier row.
    fn at_row_end(&self, offset: usize) -> bool {
        let line = self.text.line_of(offset);
        let column = (offset - self.text.line_range(line).start) as u32;
        self.wrap.breaks(line).iter().any(|b| b.ix == column)
    }

    /// Home: the start of the caret's row, then the start of its line.
    fn row_start(&self) -> usize {
        let head = self.selection.head;
        let (range, _) = self.row_range(self.row_of(head, self.affinity));
        if head != range.start { range.start } else { self.text.line_range(self.text.line_of(head)).start }
    }

    /// End: the end of the caret's row, then the end of its line.
    fn row_end(&self) -> usize {
        let head = self.selection.head;
        let (range, _) = self.row_range(self.row_of(head, self.affinity));
        if head != range.end { range.end } else { self.text.line_range(self.text.line_of(head)).end }
    }

    fn page_rows(&self) -> isize {
        self.geometry.as_ref().map_or(1, |g| (g.bounds.size.height / g.line_height) as isize)
    }

    pub(super) fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selection;
        if selection.is_empty() && selection.head == 0 {
            return cx.propagate();
        }
        let offset =
            if selection.is_empty() { self.visible_back(self.text.previous_boundary(selection.head)) } else { selection.range().start };
        self.move_to(offset, false, None, cx);
    }

    pub(super) fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selection;
        if selection.is_empty() && selection.head == self.text.len() {
            return cx.propagate();
        }
        let offset =
            if selection.is_empty() { self.visible_forward(self.text.next_boundary(selection.head)) } else { selection.range().end };
        self.move_to(offset, false, None, cx);
    }

    pub(super) fn move_up(&mut self, _: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(-1, true, window, cx);
    }

    pub(super) fn move_down(&mut self, _: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(1, true, window, cx);
    }

    pub(super) fn page_up(&mut self, _: &MovePageUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(-self.page_rows(), false, window, cx);
    }

    pub(super) fn page_down(&mut self, _: &MovePageDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(self.page_rows(), false, window, cx);
    }

    pub(super) fn move_home(&mut self, _: &MoveHome, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.row_start();
        self.move_to(offset, false, None, cx);
    }

    pub(super) fn move_end(&mut self, _: &MoveEnd, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.row_end();
        self.move_to(offset, true, None, cx);
    }

    pub(super) fn move_to_start(&mut self, _: &MoveToStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, false, None, cx);
    }

    pub(super) fn move_to_end(&mut self, _: &MoveToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.text.len(), false, None, cx);
    }

    pub(super) fn move_to_previous_word(&mut self, _: &MoveToPreviousWord, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.text.previous_word_start(self.selection.head);
        self.move_to(offset, false, None, cx);
    }

    pub(super) fn move_to_next_word(&mut self, _: &MoveToNextWord, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.text.next_word_end(self.selection.head);
        self.move_to(offset, false, None, cx);
    }

    pub(super) fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.visible_back(self.text.previous_boundary(self.selection.head));
        self.select_to(offset, false, None, cx);
    }

    pub(super) fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.visible_forward(self.text.next_boundary(self.selection.head));
        self.select_to(offset, false, None, cx);
    }

    pub(super) fn select_up(&mut self, _: &SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        self.select_vertical(-1, window, cx);
    }

    pub(super) fn select_down(&mut self, _: &SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        self.select_vertical(1, window, cx);
    }

    pub(super) fn select_to_start_of_line(&mut self, _: &SelectToStartOfLine, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.row_start();
        self.select_to(offset, false, None, cx);
    }

    pub(super) fn select_to_end_of_line(&mut self, _: &SelectToEndOfLine, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.row_end();
        self.select_to(offset, true, None, cx);
    }

    pub(super) fn select_to_start(&mut self, _: &SelectToStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, false, None, cx);
    }

    pub(super) fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.text.len(), false, None, cx);
    }

    pub(super) fn select_to_previous_word(&mut self, _: &SelectToPreviousWordStart, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.text.previous_word_start(self.selection.head);
        self.select_to(offset, false, None, cx);
    }

    pub(super) fn select_to_next_word(&mut self, _: &SelectToNextWordEnd, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.text.next_word_end(self.selection.head);
        self.select_to(offset, false, None, cx);
    }

    pub(super) fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        // Unlike other moves, selecting everything leaves the view where it is.
        self.selection = Selection { anchor: 0, head: self.text.len() };
        self.affinity = false;
        self.goal = None;
        cx.notify();
    }

    pub(super) fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let text = &self.text.src()[self.selection.range()];
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
    }

    pub(super) fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        self.selecting = true;
        let (offset, affinity) = self.offset_at(event.position, window);
        match event.button {
            MouseButton::Left if event.click_count >= 3 => {
                let range = self.text.line_range(self.text.line_of(offset));
                self.word = None;
                self.selection = Selection { anchor: range.start, head: range.end };
                self.affinity = false;
                self.moved(cx);
            }
            MouseButton::Left if event.click_count == 2 => {
                let Some(word) = self.text.word_range(offset) else { return };
                self.selection = Selection { anchor: word.start, head: word.end };
                self.word = Some(word);
                self.affinity = false;
                self.moved(cx);
            }
            MouseButton::Right => {
                let range = self.selection.range();
                if !(range.start <= offset && offset < range.end) {
                    self.move_to(offset, affinity, None, cx);
                }
                self.menu_at = Some(event.position);
            }
            MouseButton::Left if event.modifiers.shift => self.select_to(offset, affinity, None, cx),
            MouseButton::Left => self.move_to(offset, affinity, None, cx),
            _ => {}
        }
    }

    pub(super) fn mouse_drag(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || !self.focus.is_focused(window) {
            return;
        }
        let (offset, affinity) = self.offset_at(position, window);
        self.select_to(offset, affinity, None, cx);
        self.drag_at = Some(position);
        let delta = self.geometry.as_ref().and_then(|g| auto_scroll_delta(position.y, g.bounds));
        match delta {
            None => self.auto_scroll = None,
            Some(_) if self.auto_scroll.is_some() => {}
            Some(_) => {
                self.auto_scroll = Some(cx.spawn_in(window, async move |this, cx| {
                    loop {
                        cx.background_executor().timer(AUTO_SCROLL_TICK).await;
                        let more = this.update_in(cx, |this, window, cx| this.auto_scroll_tick(window, cx));
                        if !more.unwrap_or(false) {
                            break;
                        }
                    }
                }));
            }
        }
    }

    /// One step of scrolling while a drag is held past an edge. False once it isn't.
    fn auto_scroll_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (Some(position), Some(geometry)) = (self.drag_at, self.geometry.clone()) else { return false };
        let Some(delta) = auto_scroll_delta(position.y, geometry.bounds).filter(|_| self.selecting) else {
            self.auto_scroll = None;
            return false;
        };
        self.scroll_to(self.scroll_y + delta, cx);
        let (offset, affinity) = self.offset_at(position, window);
        self.select_to(offset, affinity, None, cx);
        // Moving the caret mustn't also scroll; the drag owns the scroll while it lasts.
        self.follow = false;
        true
    }

    pub(super) fn mouse_up(&mut self, button: MouseButton, window: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        self.word = None;
        self.drag_at = None;
        self.auto_scroll = None;
        if button == MouseButton::Right
            && let Some(position) = self.menu_at.take()
        {
            self.context_menu.update(cx, |menu, cx| menu.open_at(position, window, cx));
        }
    }

    pub(super) fn start_blink(&mut self, cx: &mut Context<Self>) {
        self.blink(self.blink_epoch, cx);
    }

    pub(super) fn stop_blink(&mut self, cx: &mut Context<Self>) {
        self.blink_epoch = 0;
        self.blink_paused = false;
        self.blink_on = false;
        self.blink_task = Task::ready(());
        cx.notify();
    }

    fn blink(&mut self, epoch: usize, cx: &mut Context<Self>) {
        if epoch != self.blink_epoch {
            return;
        }
        if self.blink_paused {
            self.blink_on = true;
            return;
        }
        self.blink_on = !self.blink_on;
        cx.notify();
        self.blink_epoch += 1;
        let epoch = self.blink_epoch;
        self.blink_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BLINK).await;
            this.update(cx, |this, cx| this.blink(epoch, cx)).ok();
        });
    }

    /// Shows the caret solid while it moves; blinking resumes once it rests.
    fn pause_blink(&mut self, cx: &mut Context<Self>) {
        if self.blink_epoch == 0 {
            return;
        }
        self.blink_paused = true;
        self.blink_on = true;
        self.blink_epoch += 1;
        let epoch = self.blink_epoch;
        self.blink_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BLINK_PAUSE).await;
            this.update(cx, |this, cx| {
                if epoch == this.blink_epoch {
                    this.blink_paused = false;
                    this.blink(epoch, cx);
                }
            })
            .ok();
        });
    }

    pub(super) fn caret_visible(&self) -> bool {
        self.blink_paused || self.blink_on
    }

    /// Scrolls the caret into view after it moved, keeping three rows between it and the
    /// edge (one in short views), as the kit does.
    /// A search match to reveal takes the caret's place.
    pub(super) fn follow_caret(&mut self, viewport: Pixels) {
        let target = match (self.reveal.take(), std::mem::take(&mut self.follow)) {
            (Some(offset), _) => self.row_of(offset, false),
            (None, true) => self.row_of(self.selection.head, self.affinity),
            (None, false) => return,
        };
        let line_height = self.line_height;
        let rows = (viewport / line_height) as usize;
        let margin = if rows < 24 { line_height } else { line_height * 3. }.min(viewport / 2.);
        let top = PADDING.top + line_height * target as f32;
        if top - self.scroll_y > viewport - margin {
            self.scroll_y = top - (viewport - margin);
        } else if top - self.scroll_y < margin {
            self.scroll_y = (top - margin).max(px(0.));
        }
        self.scroll_y = self.scroll_y.clamp(px(0.), self.max_scroll);
    }
}

#[cfg(test)]
mod tests {
    use super::{Selection, auto_scroll_delta};
    use gpui::{Bounds, point, px, size};

    #[test]
    fn selection_range_is_ordered() {
        let s = Selection { anchor: 9, head: 3 };
        assert_eq!(s.range(), 3..9);
        assert!(!s.is_empty());
    }

    #[test]
    fn auto_scroll_ramps_past_the_edges() {
        let bounds = Bounds::new(point(px(0.), px(100.)), size(px(200.), px(400.)));
        assert_eq!(auto_scroll_delta(px(300.), bounds), None);
        // Triggers start 16px inside the 100..500 bounds and reach full speed 96px further.
        assert_eq!(auto_scroll_delta(px(484.), bounds), None);
        assert_eq!(auto_scroll_delta(px(532.), bounds), Some(px(38.)));
        assert_eq!(auto_scroll_delta(px(580.), bounds), Some(px(64.)));
        assert_eq!(auto_scroll_delta(px(900.), bounds), Some(px(64.)));
        assert_eq!(auto_scroll_delta(px(20.), bounds), Some(px(-64.)));
    }
}
