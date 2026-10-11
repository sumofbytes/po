//! Text inputs like gpui-kit's: a growing multi-line textarea for prompts and a bordered
//! single-line field. Editing goes through gpui's input handler, so typing, IME composition,
//! clipboard, undo and the usual caret and selection keys all work.
use std::ops::Range;
use std::time::Duration;

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId,
    InteractiveElement as _, IntoElement, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Point, RenderOnce, SharedString, Style, Styled as _, Subscription,
    Task, TextAlign, TextRun, UTF16Selection, Window, WrappedLine, div, fill, point, prelude::FluentBuilder as _,
    px, relative, rems, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::theme::Palette;

gpui::actions!(
    text_input,
    [
        Backspace, Delete, DeleteWordLeft, DeleteWordRight, Left, Right, Up, Down, WordLeft, WordRight, Home, End,
        SelectLeft, SelectRight, SelectUp, SelectDown, SelectWordLeft, SelectWordRight, SelectHome, SelectEnd,
        SelectAll, Copy, Cut, Paste, Undo, Redo, Enter, ShiftEnter,
    ]
);

const CONTEXT: &str = "TextInput";
const BLINK: Duration = Duration::from_millis(500);
const BLINK_PAUSE: Duration = Duration::from_millis(300);
const CARET_WIDTH: Pixels = px(2.);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some(CONTEXT)),
        KeyBinding::new("shift-backspace", Backspace, Some(CONTEXT)),
        KeyBinding::new("delete", Delete, Some(CONTEXT)),
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, Some(CONTEXT)),
        KeyBinding::new("ctrl-delete", DeleteWordRight, Some(CONTEXT)),
        KeyBinding::new("left", Left, Some(CONTEXT)),
        KeyBinding::new("right", Right, Some(CONTEXT)),
        KeyBinding::new("up", Up, Some(CONTEXT)),
        KeyBinding::new("down", Down, Some(CONTEXT)),
        KeyBinding::new("ctrl-left", WordLeft, Some(CONTEXT)),
        KeyBinding::new("ctrl-right", WordRight, Some(CONTEXT)),
        KeyBinding::new("home", Home, Some(CONTEXT)),
        KeyBinding::new("end", End, Some(CONTEXT)),
        KeyBinding::new("shift-left", SelectLeft, Some(CONTEXT)),
        KeyBinding::new("shift-right", SelectRight, Some(CONTEXT)),
        KeyBinding::new("shift-up", SelectUp, Some(CONTEXT)),
        KeyBinding::new("shift-down", SelectDown, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, Some(CONTEXT)),
        KeyBinding::new("shift-home", SelectHome, Some(CONTEXT)),
        KeyBinding::new("shift-end", SelectEnd, Some(CONTEXT)),
        KeyBinding::new("ctrl-a", SelectAll, Some(CONTEXT)),
        KeyBinding::new("ctrl-c", Copy, Some(CONTEXT)),
        KeyBinding::new("ctrl-x", Cut, Some(CONTEXT)),
        KeyBinding::new("ctrl-v", Paste, Some(CONTEXT)),
        KeyBinding::new("ctrl-z", Undo, Some(CONTEXT)),
        KeyBinding::new("ctrl-y", Redo, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-z", Redo, Some(CONTEXT)),
        KeyBinding::new("enter", Enter, Some(CONTEXT)),
        KeyBinding::new("shift-enter", ShiftEnter, Some(CONTEXT)),
    ]);
}

pub enum InputEvent {
    Change,
    PressEnter { shift: bool },
}

/// The laid-out text from the last frame, for hit testing and moving between rows.
struct Layout {
    lines: std::rc::Rc<Vec<WrappedLine>>,
    /// Where the first line starts, scroll included.
    origin: Point<Pixels>,
    line_height: Pixels,
}

impl Layout {
    /// Each hard line's byte range in the text and its top, relative to `origin`.
    fn rows(&self) -> impl Iterator<Item = (Range<usize>, Pixels, &WrappedLine)> + '_ {
        let mut start = 0;
        let mut top = px(0.);
        self.lines.iter().map(move |line| {
            let range = start..start + line.len();
            let this_top = top;
            start = range.end + 1;
            top += line.size(self.line_height).height;
            (range, this_top, line)
        })
    }

    fn position(&self, offset: usize) -> Point<Pixels> {
        for (range, top, line) in self.rows() {
            if offset <= range.end {
                let p = line.position_for_index(offset - range.start, self.line_height).unwrap_or_default();
                return self.origin + point(p.x, top + p.y);
            }
        }
        self.origin
    }

    fn offset(&self, position: Point<Pixels>) -> usize {
        let local = position - self.origin;
        let mut last = 0;
        for (range, top, line) in self.rows() {
            let bottom = top + line.size(self.line_height).height;
            last = range.end;
            if local.y < bottom {
                let within = point(local.x, (local.y - top).max(px(0.)));
                let ix = match line.closest_index_for_position(within, self.line_height) {
                    Ok(ix) | Err(ix) => ix,
                };
                return range.start + ix.min(range.len());
            }
        }
        last
    }
}

pub struct InputState {
    focus: FocusHandle,
    text: String,
    selection: Range<usize>,
    /// The caret is at the start of `selection`.
    reversed: bool,
    marked: Option<Range<usize>>,
    placeholder: SharedString,
    multi_line: bool,
    rows: (usize, usize),
    submit_on_enter: bool,
    layout: Option<Layout>,
    scroll: Pixels,
    goal_x: Option<Pixels>,
    selecting: bool,
    undo: Vec<(String, Range<usize>)>,
    redo: Vec<(String, Range<usize>)>,
    blink_on: bool,
    blink_paused: bool,
    blink_epoch: usize,
    blink_task: Task<()>,
    _focus: [Subscription; 2],
}

impl EventEmitter<InputEvent> for InputState {}

impl InputState {
    /// A single-line input.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let subscriptions = [
            cx.on_focus(&focus, window, |this, _, cx| this.start_blink(cx)),
            cx.on_blur(&focus, window, |this, _, cx| this.stop_blink(cx)),
        ];
        Self {
            focus,
            text: String::new(),
            selection: 0..0,
            reversed: false,
            marked: None,
            placeholder: SharedString::default(),
            multi_line: false,
            rows: (1, 1),
            submit_on_enter: true,
            layout: None,
            scroll: px(0.),
            goal_x: None,
            selecting: false,
            undo: Vec::new(),
            redo: Vec::new(),
            blink_on: false,
            blink_paused: false,
            blink_epoch: 0,
            blink_task: Task::ready(()),
            _focus: subscriptions,
        }
    }

    /// A wrapping input between `min` and `max` rows tall, scrolling beyond.
    pub fn auto_grow(mut self, min: usize, max: usize) -> Self {
        self.multi_line = true;
        self.submit_on_enter = false;
        self.rows = (min, max.max(min));
        self
    }

    /// Enter sends [`InputEvent::PressEnter`] instead of a newline; Shift+Enter still breaks
    /// the line.
    pub fn submit_on_enter(mut self, submit: bool) -> Self {
        self.submit_on_enter = submit;
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn value(&self) -> SharedString {
        self.text.clone().into()
    }

    /// Replaces the text without an undo step or a change event, the caret at its end.
    pub fn set_value(&mut self, text: impl Into<String>, _: &mut Window, cx: &mut Context<Self>) {
        self.text = text.into();
        self.selection = self.text.len()..self.text.len();
        self.marked = None;
        cx.notify();
    }

    pub fn select_all(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.selection = 0..self.text.len();
        self.reversed = false;
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }

    fn caret(&self) -> usize {
        if self.reversed { self.selection.start } else { self.selection.end }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selection = offset..offset;
        self.reversed = false;
        self.moved(cx);
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.reversed { self.selection.start = offset } else { self.selection.end = offset }
        if self.selection.end < self.selection.start {
            self.reversed = !self.reversed;
            self.selection = self.selection.end..self.selection.start;
        }
        self.moved(cx);
    }

    fn moved(&mut self, cx: &mut Context<Self>) {
        self.pause_blink(cx);
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.text.grapheme_indices(true).rev().find_map(|(ix, _)| (ix < offset).then_some(ix)).unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.text.grapheme_indices(true).find_map(|(ix, _)| (ix > offset).then_some(ix)).unwrap_or(self.text.len())
    }

    fn previous_word(&self, offset: usize) -> usize {
        previous_word(&self.text, offset)
    }

    fn next_word(&self, offset: usize) -> usize {
        next_word(&self.text, offset)
    }

    fn line_bounds(&self, offset: usize) -> Range<usize> {
        line_bounds(&self.text, offset)
    }

    /// One visual row up or down from the caret, keeping its x.
    fn vertical(&mut self, rows: f32) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let at = layout.position(self.caret());
        let x = *self.goal_x.get_or_insert(at.x);
        let target = point(x, at.y + layout.line_height * (rows + 0.5));
        let top = layout.origin.y;
        let bottom = top + layout.lines.iter().map(|l| l.size(layout.line_height).height).sum::<Pixels>();
        if target.y < top {
            return Some(0);
        }
        if target.y > bottom {
            return Some(self.text.len());
        }
        Some(layout.offset(target))
    }

    fn edit(&mut self, range: Range<usize>, new: &str, cx: &mut Context<Self>) {
        self.undo.push((self.text.clone(), self.selection.clone()));
        self.redo.clear();
        self.text.replace_range(range.clone(), new);
        let caret = range.start + new.len();
        self.selection = caret..caret;
        self.reversed = false;
        self.marked = None;
        self.goal_x = None;
        cx.emit(InputEvent::Change);
        self.moved(cx);
    }

    fn delete_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            let caret = self.caret();
            let range = offset.min(caret)..offset.max(caret);
            self.edit(range, "", cx);
        } else {
            self.edit(self.selection.clone(), "", cx);
        }
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_to(self.previous_boundary(self.caret()), cx);
    }
    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_to(self.next_boundary(self.caret()), cx);
    }
    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_to(self.previous_word(self.caret()), cx);
    }
    fn delete_word_right(&mut self, _: &DeleteWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_to(self.next_word(self.caret()), cx);
    }
    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        let to = if self.selection.is_empty() { self.previous_boundary(self.caret()) } else { self.selection.start };
        self.move_to(to, cx);
    }
    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        let to = if self.selection.is_empty() { self.next_boundary(self.caret()) } else { self.selection.end };
        self.move_to(to, cx);
    }
    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if !self.multi_line {
            return cx.propagate();
        }
        if let Some(to) = self.vertical(-1.) {
            self.move_to(to, cx);
        }
    }
    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if !self.multi_line {
            return cx.propagate();
        }
        if let Some(to) = self.vertical(1.) {
            self.move_to(to, cx);
        }
    }
    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(self.caret()), cx);
    }
    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(self.caret()), cx);
    }
    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_bounds(self.caret()).start, cx);
    }
    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_bounds(self.caret()).end, cx);
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.caret()), cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.caret()), cx);
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(to) = self.vertical(-1.) {
            self.select_to(to, cx);
        }
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(to) = self.vertical(1.) {
            self.select_to(to, cx);
        }
    }
    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_word(self.caret()), cx);
    }
    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_word(self.caret()), cx);
    }
    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_bounds(self.caret()).start, cx);
    }
    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_bounds(self.caret()).end, cx);
    }
    fn on_select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.select_all(window, cx);
    }
    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.text[self.selection.clone()].to_string()));
        }
    }
    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.text[self.selection.clone()].to_string()));
            self.edit(self.selection.clone(), "", cx);
        }
    }
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = if self.multi_line { text } else { text.replace('\n', " ") };
            self.edit(self.selection.clone(), &text, cx);
        }
    }
    fn on_undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((text, selection)) = self.undo.pop() {
            self.redo.push((std::mem::replace(&mut self.text, text), self.selection.clone()));
            self.selection = selection;
            cx.emit(InputEvent::Change);
            self.moved(cx);
        }
    }
    fn on_redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((text, selection)) = self.redo.pop() {
            self.undo.push((std::mem::replace(&mut self.text, text), self.selection.clone()));
            self.selection = selection;
            cx.emit(InputEvent::Change);
            self.moved(cx);
        }
    }
    fn enter(&mut self, _: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        if self.submit_on_enter || !self.multi_line {
            cx.emit(InputEvent::PressEnter { shift: false });
        } else {
            self.edit(self.selection.clone(), "\n", cx);
        }
    }
    fn shift_enter(&mut self, _: &ShiftEnter, _: &mut Window, cx: &mut Context<Self>) {
        if self.multi_line {
            self.edit(self.selection.clone(), "\n", cx);
        } else {
            cx.emit(InputEvent::PressEnter { shift: true });
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        let Some(offset) = self.layout.as_ref().map(|l| l.offset(event.position)) else { return };
        self.goal_x = None;
        match event.click_count {
            2 => {
                let word = self.text.split_word_bound_indices().find(|(ix, s)| *ix <= offset && offset <= ix + s.len());
                if let Some((ix, s)) = word {
                    self.selection = ix..ix + s.len();
                    self.reversed = false;
                }
                cx.notify();
            }
            n if n >= 3 => {
                self.selection = self.line_bounds(offset);
                self.reversed = false;
                cx.notify();
            }
            _ if event.modifiers.shift => self.select_to(offset, cx),
            _ => {
                self.selecting = true;
                self.move_to(offset, cx);
            }
        }
    }

    fn mouse_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.selecting
            && let Some(offset) = self.layout.as_ref().map(|l| l.offset(position))
        {
            self.select_to(offset, cx);
        }
    }

    fn start_blink(&mut self, cx: &mut Context<Self>) {
        self.blink(self.blink_epoch, cx);
    }

    fn stop_blink(&mut self, cx: &mut Context<Self>) {
        self.blink_epoch = 0;
        self.blink_paused = false;
        self.blink_on = false;
        self.blink_task = Task::ready(());
        cx.notify();
    }

    fn blink(&mut self, epoch: usize, cx: &mut Context<Self>) {
        if epoch != self.blink_epoch || self.blink_paused {
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

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for c in self.text.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += c.len_utf16();
            utf8 += c.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.text[..offset.min(self.text.len())].chars().map(char::len_utf16).sum()
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

impl EntityInputHandler for InputState {
    fn text_for_range(&mut self, range: Range<usize>, adjusted: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let range = self.range_from_utf16(&range);
        adjusted.replace(self.range_to_utf16(&range));
        Some(self.text[range].to_string())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selection), reversed: self.reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(&mut self, range: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        let range = range
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        let text = if self.multi_line { text.to_string() } else { text.replace('\n', " ") };
        self.edit(range, &text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        self.text.replace_range(range.clone(), new_text);
        self.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selection = new_selected
            .as_ref()
            .map(|r| {
                let r = self.range_from_utf16(r);
                range.start + r.start..range.start + r.end
            })
            .unwrap_or(range.start + new_text.len()..range.start + new_text.len());
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    fn bounds_for_range(&mut self, range: Range<usize>, _: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.range_from_utf16(&range);
        let start = layout.position(range.start);
        let end = layout.position(range.end);
        Some(Bounds::from_corners(start, point(end.x, end.y + layout.line_height)))
    }

    fn character_index_for_point(&mut self, point: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        Some(self.offset_to_utf16(layout.offset(point)))
    }
}

/// Where Ctrl+Left goes: the start of the last word before `offset`.
fn previous_word(text: &str, offset: usize) -> usize {
    text[..offset].split_word_bound_indices().rfind(|(_, s)| !s.trim().is_empty()).map_or(0, |(ix, _)| ix)
}

/// Where Ctrl+Right goes: the end of the first word after `offset`.
fn next_word(text: &str, offset: usize) -> usize {
    text[offset..].split_word_bound_indices().find(|(_, s)| !s.trim().is_empty()).map_or(text.len(), |(ix, s)| offset + ix + s.len())
}

/// The line holding `offset`, without its newline.
fn line_bounds(text: &str, offset: usize) -> Range<usize> {
    let start = text[..offset].rfind('\n').map_or(0, |ix| ix + 1);
    let end = text[offset..].find('\n').map_or(text.len(), |ix| offset + ix);
    start..end
}

/// The paddings around the text: the kit's medium textarea, or its small field.
fn paddings(multi_line: bool, rem_size: Pixels) -> (Pixels, Pixels) {
    if multi_line { (rems(0.75).to_pixels(rem_size), rems(0.5).to_pixels(rem_size)) } else { (px(0.), px(0.)) }
}

struct TextElement {
    state: Entity<InputState>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

pub struct Prepaint {
    hitbox: Hitbox,
    lines: std::rc::Rc<Vec<WrappedLine>>,
    origin: Point<Pixels>,
    line_height: Pixels,
    selection: Vec<Bounds<Pixels>>,
    caret: Option<Bounds<Pixels>>,
}

fn shape(text: &str, style: &gpui::TextStyle, color: gpui::Hsla, width: Option<Pixels>, window: &Window) -> Vec<WrappedLine> {
    let run = TextRun { len: text.len(), color, ..style.to_run(text.len()) };
    let font_size = style.font_size.to_pixels(window.rem_size());
    window
        .text_system()
        .shape_text(SharedString::from(text.to_string()), font_size, &[run], width, None)
        .map(|lines| lines.into_iter().collect())
        .unwrap_or_default()
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let state = self.state.read(cx);
        let (multi_line, rows) = (state.multi_line, state.rows);
        let text = if state.text.is_empty() { state.placeholder.to_string() } else { state.text.clone() };
        let style = window.text_style();
        let mut layout_style = Style::default();
        layout_style.size.width = relative(1.).into();
        if !multi_line {
            layout_style.size.height = window.line_height().into();
            return (window.request_layout(layout_style, [], cx), ());
        }
        let (pad_x, pad_y) = paddings(true, window.rem_size());
        // Read here, where this input's text style applies; measuring runs outside it.
        let line_height = window.line_height();
        let id = window.request_measured_layout(layout_style, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                gpui::AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let wrap = width.map(|w| (w - pad_x * 2.).max(px(1.)));
            let lines = shape(&text, &style, gpui::black(), wrap, window);
            let height: Pixels = lines.iter().map(|l| l.size(line_height).height).sum();
            let shown = (height / line_height).round().clamp(rows.0 as f32, rows.1 as f32);
            size(width.unwrap_or_default(), line_height * shown + pad_y * 2.)
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let muted = cx.global::<Palette>().muted_foreground;
        let style = window.text_style();
        let line_height = window.line_height();
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.state.update(cx, |state, _| {
            let (pad_x, pad_y) = paddings(state.multi_line, window.rem_size());
            let placeholder = state.text.is_empty();
            let (text, color) = if placeholder { (state.placeholder.to_string(), muted) } else { (state.text.clone(), style.color) };
            let wrap = state.multi_line.then(|| (bounds.size.width - pad_x * 2.).max(px(1.)));
            let lines = std::rc::Rc::new(shape(&text, &style, color, wrap, window));
            let content: Pixels = lines.iter().map(|l| l.size(line_height).height).sum();
            let inner = Bounds::new(bounds.origin + point(pad_x, pad_y), size(bounds.size.width - pad_x * 2., bounds.size.height - pad_y * 2.));

            // Keep the caret in view: vertically when wrapping, sideways on one line.
            let unscrolled = Layout { lines: lines.clone(), origin: inner.origin, line_height };
            let caret_at = if placeholder { point(px(0.), px(0.)) } else { unscrolled.position(state.caret()) - inner.origin };
            if state.multi_line {
                let max = (content - inner.size.height).max(px(0.));
                if caret_at.y < state.scroll {
                    state.scroll = caret_at.y;
                } else if caret_at.y + line_height > state.scroll + inner.size.height {
                    state.scroll = caret_at.y + line_height - inner.size.height;
                }
                state.scroll = state.scroll.clamp(px(0.), max);
            } else {
                let width = lines.first().map_or(px(0.), |l| l.size(line_height).width);
                let max = (width + CARET_WIDTH - inner.size.width).max(px(0.));
                if caret_at.x < state.scroll {
                    state.scroll = caret_at.x;
                } else if caret_at.x + CARET_WIDTH > state.scroll + inner.size.width {
                    state.scroll = caret_at.x + CARET_WIDTH - inner.size.width;
                }
                state.scroll = state.scroll.clamp(px(0.), max);
            }
            let origin = if state.multi_line {
                inner.origin - point(px(0.), state.scroll)
            } else {
                inner.origin - point(state.scroll, px(0.))
            };
            let layout = Layout { lines: lines.clone(), origin, line_height };

            let mut selection = Vec::new();
            if !placeholder && !state.selection.is_empty() {
                for (range, top, line) in layout.rows() {
                    let (from, to) = (state.selection.start.max(range.start), state.selection.end.min(range.end));
                    let newline = state.selection.end > range.end && state.selection.start <= range.end;
                    if from > to || (from == to && !newline) {
                        continue;
                    }
                    let a = line.position_for_index(from - range.start, line_height).unwrap_or_default();
                    let b = line.position_for_index(to - range.start, line_height).unwrap_or_default();
                    let row = |y: Pixels| origin.y + top + y;
                    if a.y == b.y {
                        let right = if newline { b.x + px(4.) } else { b.x };
                        selection.push(Bounds::from_corners(point(origin.x + a.x, row(a.y)), point(origin.x + right, row(a.y) + line_height)));
                    } else {
                        let full = inner.size.width;
                        selection.push(Bounds::from_corners(point(origin.x + a.x, row(a.y)), point(origin.x + full, row(a.y) + line_height)));
                        let mut y = a.y + line_height;
                        while y < b.y {
                            selection.push(Bounds::from_corners(point(origin.x, row(y)), point(origin.x + full, row(y) + line_height)));
                            y += line_height;
                        }
                        selection.push(Bounds::from_corners(point(origin.x, row(b.y)), point(origin.x + b.x, row(b.y) + line_height)));
                    }
                }
            }
            let at = if placeholder { origin } else { layout.position(state.caret()) };
            let height = line_height * 0.85;
            let caret = Some(Bounds::new(point(at.x, at.y + (line_height - height) / 2.), size(CARET_WIDTH, height)));
            // The placeholder is not text: hit-testing it would put the caret past an empty value.
            state.layout = Some(if placeholder { Layout { lines: std::rc::Rc::new(vec![]), origin, line_height } } else { layout });
            Prepaint { hitbox, lines, origin, line_height, selection, caret }
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.state.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.state.clone()), cx);
        window.set_cursor_style(gpui::CursorStyle::IBeam, &prepaint.hitbox);
        let palette = cx.global::<Palette>();
        let (selection_color, caret_color) = (palette.selection, palette.caret);
        let focused = focus.is_focused(window);
        let show_caret = focused && window.is_window_active() && {
            let s = self.state.read(cx);
            s.blink_paused || s.blink_on
        };
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            if focused {
                for rect in &prepaint.selection {
                    window.paint_quad(fill(*rect, selection_color));
                }
            }
            let mut y = prepaint.origin.y;
            for line in prepaint.lines.iter() {
                line.paint(point(prepaint.origin.x, y), prepaint.line_height, TextAlign::Left, None, window, cx).ok();
                y += line.size(prepaint.line_height).height;
            }
            if let Some(caret) = prepaint.caret.filter(|_| show_caret) {
                window.paint_quad(fill(caret, caret_color));
            }
        });

        let (state, hitbox) = (self.state.clone(), prepaint.hitbox.clone());
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase.bubble() && event.button == MouseButton::Left && hitbox.is_hovered(window) {
                state.update(cx, |state, cx| state.mouse_down(event, window, cx));
            }
        });
        let state = self.state.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase.bubble() && event.pressed_button == Some(MouseButton::Left) {
                state.update(cx, |state, cx| state.mouse_drag(event.position, cx));
            }
        });
        let state = self.state.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
            if phase.bubble() {
                state.update(cx, |state, _| state.selecting = false);
            }
        });
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    /// Just the text, as `Textarea::appearance(false)`.
    Plain,
    /// A small bordered field.
    Field,
}

/// Draws an [`InputState`]: plain for the textareas, or as a small bordered field with an
/// optional element after the text.
#[derive(IntoElement)]
pub struct Input {
    state: Entity<InputState>,
    look: Look,
    suffix: Option<AnyElement>,
}

impl Input {
    pub fn new(state: &Entity<InputState>) -> Self {
        Self { state: state.clone(), look: Look::Plain, suffix: None }
    }

    /// The kit's small input: background, border and radius.
    pub fn field(mut self) -> Self {
        self.look = Look::Field;
        self
    }

    pub fn suffix(mut self, suffix: impl IntoElement) -> Self {
        self.suffix = Some(suffix.into_any_element());
        self
    }
}

impl RenderOnce for Input {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.global::<Palette>();
        let focus = self.state.read(cx).focus.clone();
        let s = &self.state;
        div()
            .id(("text-input", self.state.entity_id()))
            .key_context(CONTEXT)
            .track_focus(&focus)
            .flex()
            .w_full()
            .items_center()
            .line_height(rems(1.25))
            .text_sm()
            .when(self.look == Look::Field, |this| {
                this.h_6().px_2().py_0p5().gap_1().bg(palette.input_background).rounded(palette.radius).border_1().border_color(palette.input_border)
            })
            .on_action(window.listener_for(s, InputState::backspace))
            .on_action(window.listener_for(s, InputState::delete))
            .on_action(window.listener_for(s, InputState::delete_word_left))
            .on_action(window.listener_for(s, InputState::delete_word_right))
            .on_action(window.listener_for(s, InputState::left))
            .on_action(window.listener_for(s, InputState::right))
            .on_action(window.listener_for(s, InputState::up))
            .on_action(window.listener_for(s, InputState::down))
            .on_action(window.listener_for(s, InputState::word_left))
            .on_action(window.listener_for(s, InputState::word_right))
            .on_action(window.listener_for(s, InputState::home))
            .on_action(window.listener_for(s, InputState::end))
            .on_action(window.listener_for(s, InputState::select_left))
            .on_action(window.listener_for(s, InputState::select_right))
            .on_action(window.listener_for(s, InputState::select_up))
            .on_action(window.listener_for(s, InputState::select_down))
            .on_action(window.listener_for(s, InputState::select_word_left))
            .on_action(window.listener_for(s, InputState::select_word_right))
            .on_action(window.listener_for(s, InputState::select_home))
            .on_action(window.listener_for(s, InputState::select_end))
            .on_action(window.listener_for(s, InputState::on_select_all))
            .on_action(window.listener_for(s, InputState::copy))
            .on_action(window.listener_for(s, InputState::cut))
            .on_action(window.listener_for(s, InputState::paste))
            .on_action(window.listener_for(s, InputState::on_undo))
            .on_action(window.listener_for(s, InputState::on_redo))
            .on_action(window.listener_for(s, InputState::enter))
            .on_action(window.listener_for(s, InputState::shift_enter))
            .child(div().flex_1().min_w_0().child(TextElement { state: self.state.clone() }))
            .children(self.suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::{line_bounds, next_word, previous_word};

    #[test]
    fn word_moves_and_lines() {
        let text = "let foo = bar;\n  baz";
        assert_eq!(next_word(text, 0), 3);
        assert_eq!(next_word(text, 3), 7);
        assert_eq!(previous_word(text, 7), 4);
        assert_eq!(previous_word(text, 4), 0);
        assert_eq!(line_bounds(text, 5), 0..14);
        assert_eq!(line_bounds(text, 17), 15..20);
    }
}
