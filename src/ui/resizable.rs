//! A center pane between two sidebars whose widths are dragged at their inner edges, laid out
//! and drawn like gpui-kit's resizable panels: a hairline in a wider grab band that lights up
//! while dragged. When the window is too narrow for everything, all three shrink in proportion
//! to their widths, the sidebars no further than their minimums: about where the kit's layout
//! started at the same window size, but the same however the window got there.
use std::ops::Range;

use gpui::{
    AnyElement, App, Bounds, CursorStyle, Entity, InteractiveElement as _, IntoElement, MouseButton, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, RenderOnce, Styled, Window, canvas, div, prelude::FluentBuilder as _,
    px,
};

use crate::theme::Palette;

/// The grab band either side of the hairline.
const HANDLE_PADDING: Pixels = px(4.);
/// The narrowest the center pane gets.
const MIN_CENTER: Pixels = px(100.);
/// The width the center pane shrinks from, alongside the sidebars, in narrow windows.
const CENTER_BASIS: Pixels = px(360.);

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Left,
    Right,
}

pub struct SidebarsState {
    left: Pixels,
    right: Pixels,
    /// The right width was set by a drag (or restored from one), so it is what the user wants and
    /// is no longer replaced by a default.
    right_chosen: bool,
    dragging: Option<Side>,
    /// The whole layout's bounds, from the last frame.
    bounds: Bounds<Pixels>,
}

impl SidebarsState {
    pub fn new(left: Pixels, right: Pixels) -> Self {
        Self { left, right, right_chosen: false, dragging: None, bounds: Bounds::default() }
    }

    /// The widths of the left and right sidebars, and the right one's only if it was chosen.
    pub fn widths(&self) -> (Pixels, Option<Pixels>) {
        (self.left, self.right_chosen.then_some(self.right))
    }

    /// Restores widths from an earlier run; a right width given this way counts as chosen.
    pub fn set_widths(&mut self, left: Option<Pixels>, right: Option<Pixels>) {
        if let Some(left) = left {
            self.left = left;
        }
        if let Some(right) = right {
            self.right = right;
            self.right_chosen = true;
        }
    }

    /// A handle is being dragged.
    pub fn dragging(&self) -> bool {
        self.dragging.is_some()
    }

    /// Gives the right sidebar `share` of the whole layout's width, within the range it is drawn
    /// with, unless a width was chosen for it, which is kept. Does nothing until the layout has
    /// been measured.
    pub fn set_right_share(&mut self, share: f32) {
        let width = self.bounds.size.width;
        if width > px(0.) && !self.right_chosen {
            self.right = width * share;
        }
    }
}

struct Sidebar {
    element: AnyElement,
    range: Range<Pixels>,
    visible: bool,
}

#[derive(IntoElement)]
pub struct Sidebars {
    state: Entity<SidebarsState>,
    /// Names the handles, so that layouts nested in one another keep theirs apart.
    id: &'static str,
    left: Option<Sidebar>,
    center: Option<AnyElement>,
    right: Option<Sidebar>,
}

pub fn sidebars(state: &Entity<SidebarsState>) -> Sidebars {
    Sidebars { state: state.clone(), id: "sidebar", left: None, center: None, right: None }
}

impl Sidebars {
    pub fn id(mut self, id: &'static str) -> Self {
        self.id = id;
        self
    }

    pub fn left(mut self, visible: bool, range: Range<Pixels>, element: impl IntoElement) -> Self {
        self.left = Some(Sidebar { element: element.into_any_element(), range, visible });
        self
    }

    pub fn center(mut self, element: impl IntoElement) -> Self {
        self.center = Some(element.into_any_element());
        self
    }

    pub fn right(mut self, visible: bool, range: Range<Pixels>, element: impl IntoElement) -> Self {
        self.right = Some(Sidebar { element: element.into_any_element(), range, visible });
        self
    }
}

/// The width a drag to `x` gives the sidebar on `side`, within its range and leaving the
/// center pane its minimum beside the other sidebar.
fn dragged_width(side: Side, x: Pixels, bounds: Bounds<Pixels>, other: Pixels, range: &Range<Pixels>) -> Pixels {
    let wanted = match side {
        Side::Left => x - bounds.left(),
        Side::Right => bounds.right() - x,
    };
    let room = bounds.size.width - other - MIN_CENTER;
    wanted.min(room).clamp(range.start, range.end)
}

/// A sidebar at `width`, which gives way like the kit's when space runs short.
fn sidebar(width: Pixels, range: &Range<Pixels>) -> gpui::Div {
    div().flex_grow(0.).flex_shrink(1.).flex_basis(width).min_w(range.start).max_w(range.end).h_full().relative()
}

fn handle(state: &Entity<SidebarsState>, id: &'static str, side: Side, active: bool, cx: &App) -> impl IntoElement {
    let palette = cx.global::<Palette>();
    let color = if active { palette.resize_handle_active } else { palette.resize_handle };
    let state = state.clone();
    div()
        .id((id, if side == Side::Left { 0usize } else { 1 }))
        .occlude()
        .absolute()
        .top_0()
        .left(-HANDLE_PADDING)
        .h_full()
        .w(px(1.) + HANDLE_PADDING * 2.)
        .px(HANDLE_PADDING)
        .flex_shrink_0()
        .cursor_col_resize()
        .child(div().flex_none().h_full().w(px(1.)).bg(color))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            state.update(cx, |state, cx| {
                state.dragging = Some(side);
                cx.notify();
            });
            cx.stop_propagation();
        })
}

impl RenderOnce for Sidebars {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = self.state.read(cx);
        let (left_width, right_width, dragging) = (state.left, state.right, state.dragging);
        let ranges = (
            self.left.as_ref().map(|s| s.range.clone()).unwrap_or_default(),
            self.right.as_ref().map(|s| s.range.clone()).unwrap_or_default(),
        );
        let left = self.left.filter(|s| s.visible);
        let right = self.right.filter(|s| s.visible);
        let (left_shown, right_shown) = (left.is_some(), right.is_some());
        let tracker = self.state.clone();

        div()
            .size_full()
            .flex()
            .flex_row()
            .relative()
            // Measures the layout, and follows the pointer while a handle is held.
            .child(
                canvas(
                    move |bounds, _, cx| tracker.update(cx, |state, _| state.bounds = bounds),
                    {
                        let state = self.state.clone();
                        move |_, _, window, _| {
                            if dragging.is_none() {
                                return;
                            }
                            window.set_window_cursor_style(CursorStyle::ResizeLeftRight);
                            let (moving, released) = (state.clone(), state.clone());
                            let (left_range, right_range) = ranges.clone();
                            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                                if !phase.bubble() {
                                    return;
                                }
                                moving.update(cx, |state, cx| {
                                    match state.dragging {
                                        Some(Side::Left) => {
                                            let width = if right_shown { state.right } else { Default::default() };
                                            state.left = dragged_width(Side::Left, event.position.x, state.bounds, width, &left_range);
                                        }
                                        Some(Side::Right) => {
                                            let width = if left_shown { state.left } else { Default::default() };
                                            state.right = dragged_width(Side::Right, event.position.x, state.bounds, width, &right_range);
                                            state.right_chosen = true;
                                        }
                                        None => return,
                                    }
                                    cx.notify();
                                });
                            });
                            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                                if phase.bubble() {
                                    released.update(cx, |state, cx| {
                                        state.dragging = None;
                                        cx.notify();
                                    });
                                }
                            });
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .when_some(left, |this, left| {
                this.child(sidebar(left_width, &left.range).child(left.element))
            })
            .child(
                div()
                    .flex_grow(1.)
                    .flex_shrink(1.)
                    .flex_basis(CENTER_BASIS)
                    .min_w(MIN_CENTER)
                    .h_full()
                    .relative()
                    .children(self.center)
                    .when(left_shown, |this| this.child(handle(&self.state, self.id, Side::Left, dragging == Some(Side::Left), cx))),
            )
            .when_some(right, |this, right| {
                this.child(
                    sidebar(right_width, &right.range)
                        .child(right.element)
                        .child(handle(&self.state, self.id, Side::Right, dragging == Some(Side::Right), cx)),
                )
            })
    }
}

/// The height of a folded section, in rems: that of a pane header.
pub const FOLDED_REMS: f32 = 2.75;

/// The least height an open section of a [`Stack`] gets.
const MIN_SECTION: Pixels = px(72.);

/// How a [`Stack`]'s sections are sized: each can be folded down to its header, and the open
/// ones are separated by boundaries that are dragged. One section is always open.
pub struct StackState {
    /// The height each section was dragged (or restored) to; the last open section fills the
    /// rest, so its own is only kept for when it is no longer the last.
    sizes: Vec<Option<Pixels>>,
    folded: Vec<bool>,
    /// Where each section began, from the last frame.
    tops: Vec<Pixels>,
    /// The section whose height the held handle sets.
    dragging: Option<usize>,
    /// The whole stack's bounds, from the last frame.
    bounds: Bounds<Pixels>,
}

impl StackState {
    pub fn new(sizes: Vec<Option<Pixels>>) -> Self {
        let count = sizes.len();
        Self { sizes, folded: vec![false; count], tops: vec![Pixels::default(); count], dragging: None, bounds: Bounds::default() }
    }

    pub fn folded(&self, section: usize) -> bool {
        self.folded[section]
    }

    /// Folds or unfolds `section`. The last open section stays open.
    pub fn set_folded(&mut self, section: usize, folded: bool) {
        let others_open = self.folded.iter().enumerate().any(|(ix, folded)| ix != section && !folded);
        if !folded || others_open {
            self.folded[section] = folded;
        }
    }

    pub fn toggle(&mut self, section: usize) {
        self.set_folded(section, !self.folded[section]);
    }

    /// The height `section` was set to by a drag or [`Self::set_size`], if it was.
    pub fn size(&self, section: usize) -> Option<Pixels> {
        self.sizes[section]
    }

    pub fn set_size(&mut self, section: usize, size: Pixels) {
        self.sizes[section] = Some(size);
    }

    /// A handle is being dragged.
    pub fn dragging(&self) -> bool {
        self.dragging.is_some()
    }
}

/// Sections one above the other, each with its header inside it, folded down to that header or
/// open with the boundaries between open ones dragged: the vertical counterpart of
/// [`Sidebars`], drawn the same way.
#[derive(IntoElement)]
pub struct Stack {
    state: Entity<StackState>,
    sections: Vec<AnyElement>,
    fixed: Option<(usize, gpui::Rems)>,
}

pub fn stack(state: &Entity<StackState>) -> Stack {
    Stack { state: state.clone(), sections: Vec::new(), fixed: None }
}

impl Stack {
    pub fn section(mut self, element: impl IntoElement) -> Self {
        self.sections.push(element.into_any_element());
        self
    }

    /// Holds `section` at this height, with no handle under it, for as long as it is set. The
    /// height it was dragged to is kept for when it is not.
    pub fn fixed(mut self, section: usize, height: Option<gpui::Rems>) -> Self {
        self.fixed = height.map(|height| (section, height));
        self
    }
}

/// The room the sections after `section` need: their header when folded, else their minimum.
fn room_below(folded: &[bool], section: usize, folded_height: Pixels) -> Pixels {
    folded[section + 1..].iter().map(|folded| if *folded { folded_height } else { MIN_SECTION }).fold(Pixels::default(), |sum, height| sum + height)
}

/// The height an open section gets for a saved `size`: the most that leaves `below` for the
/// sections under it in a stack of `height`. A stack not yet measured keeps `size`.
fn fitted_size(size: Pixels, height: Pixels, below: Pixels) -> Pixels {
    if height <= px(0.) { size } else { size.min((height - below).max(MIN_SECTION)) }
}

/// The height a drag to `y` gives a section that begins at `top`, leaving `below` for the
/// sections under it.
fn dragged_height(y: Pixels, top: Pixels, bounds: Bounds<Pixels>, below: Pixels) -> Pixels {
    let most = (bounds.bottom() - top - below).max(MIN_SECTION);
    (y - top).clamp(MIN_SECTION, most)
}

impl RenderOnce for Stack {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (sizes, folded, dragging, bounds_height) = {
            let state = self.state.read(cx);
            (state.sizes.clone(), state.folded.clone(), state.dragging, state.bounds.size.height)
        };
        let folded_height = window.rem_size() * FOLDED_REMS;
        let palette = cx.global::<Palette>();
        // A plain border for the line at rest, not the sidebars' handle colour, which glass
        // clears: these lines divide the sections of one pane.
        let (border, active) = (palette.border, palette.resize_handle_active);
        let last_open = folded.iter().rposition(|folded| !folded);
        let tracker = self.state.clone();
        let mut above: Option<usize> = None;
        let mut column = div()
            .size_full()
            .flex()
            .flex_col()
            .relative()
            // Measures the stack, and follows the pointer while a handle is held.
            .child(
                canvas(
                    move |bounds, _, cx| tracker.update(cx, |state, _| state.bounds = bounds),
                    {
                        let state = self.state.clone();
                        move |_, _, window, _| {
                            if dragging.is_none() {
                                return;
                            }
                            window.set_window_cursor_style(CursorStyle::ResizeUpDown);
                            let (moving, released) = (state.clone(), state.clone());
                            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                                if !phase.bubble() {
                                    return;
                                }
                                moving.update(cx, |state, cx| {
                                    let Some(section) = state.dragging else { return };
                                    // A section folded since the drag began has nothing to resize.
                                    if state.folded[section] {
                                        return;
                                    }
                                    let below = room_below(&state.folded, section, folded_height);
                                    let height = dragged_height(event.position.y, state.tops[section], state.bounds, below);
                                    state.sizes[section] = Some(height);
                                    cx.notify();
                                });
                            });
                            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                                if phase.bubble() {
                                    released.update(cx, |state, cx| {
                                        state.dragging = None;
                                        cx.notify();
                                    });
                                }
                            });
                        }
                    },
                )
                .absolute()
                .size_full(),
            );
        for (ix, element) in self.sections.into_iter().enumerate() {
            let measure = self.state.clone();
            // The content clips, not the section, so that a handle can reach above its edge.
            let mut section = div().relative().child(
                canvas(move |bounds, _, cx| measure.update(cx, |state, _| state.tops[ix] = bounds.top()), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            );
            // The last open section fills the stack, so it is never held at a height.
            let pinned = self.fixed.filter(|(pinned, _)| *pinned == ix && !folded[ix] && Some(ix) != last_open).map(|(_, height)| height);
            if let Some(height) = pinned {
                section = section.flex_none().h(height);
            } else if folded[ix] {
                // Folded, only the header shows.
                section = section.flex_none().h(folded_height).when(ix > 0, |s| s.border_t_1().border_color(border));
            } else if Some(ix) == last_open {
                section = section.flex_1().min_h(MIN_SECTION);
            } else {
                section = section.flex_grow(0.).flex_shrink(1.).min_h(MIN_SECTION);
                section = match sizes[ix] {
                    Some(height) => section.flex_basis(fitted_size(height, bounds_height, room_below(&folded, ix, folded_height))),
                    None => section.flex_basis(gpui::relative(0.5)),
                };
            }
            section = section.child(div().size_full().overflow_hidden().child(element));
            // The boundary with the open section above sets that section's height.
            if let (false, Some(resized), None) = (folded[ix], above, pinned) {
                let grab = self.state.clone();
                let (color, faint) = if dragging == Some(resized) { (active, active) } else { (border.opacity(0.5), border) };
                section = section.child(
                    div()
                        .id(("stack-handle", ix))
                        .group("stack-handle")
                        .occlude()
                        .absolute()
                        .left_0()
                        .top(-HANDLE_PADDING)
                        .w_full()
                        .h(px(1.) + HANDLE_PADDING * 2.)
                        .py(HANDLE_PADDING)
                        .cursor_row_resize()
                        .child(div().flex_none().w_full().h(px(1.)).bg(color).group_hover("stack-handle", move |line| line.bg(faint)))
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            grab.update(cx, |state, cx| {
                                state.dragging = Some(resized);
                                cx.notify();
                            });
                            cx.stop_propagation();
                        }),
                );
            }
            if pinned.is_some() {
                above = None;
            } else if !folded[ix] {
                above = Some(ix);
            }
            column = column.child(section);
        }
        column
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_CENTER, MIN_SECTION, Side, SidebarsState, StackState, dragged_height, dragged_width, fitted_size, room_below};
    use gpui::{Bounds, point, px, size};

    #[test]
    fn drags_stay_in_range_and_leave_the_center_room() {
        let bounds = Bounds::new(point(px(100.), px(0.)), size(px(1200.), px(800.)));
        let range = px(160.)..px(520.);
        assert_eq!(dragged_width(Side::Left, px(400.), bounds, px(460.), &range), px(300.));
        assert_eq!(dragged_width(Side::Left, px(150.), bounds, px(460.), &range), px(160.));
        assert_eq!(dragged_width(Side::Left, px(1000.), bounds, px(460.), &range), px(520.));
        // With a wide right sidebar, the left one stops where the center would get too narrow.
        assert_eq!(dragged_width(Side::Left, px(1000.), bounds, px(760.), &range), px(1200.) - px(760.) - MIN_CENTER);
        assert_eq!(dragged_width(Side::Right, px(900.), bounds, px(260.), &(px(300.)..px(760.))), px(400.));
    }

    #[test]
    fn a_dragged_section_leaves_the_ones_below_their_room() {
        let bounds = Bounds::new(point(px(0.), px(50.)), size(px(300.), px(800.)));
        // The second section begins 150px down the stack, with 100px needed below it.
        let top = px(200.);
        assert_eq!(dragged_height(px(350.), top, bounds, px(100.)), px(150.));
        assert_eq!(dragged_height(px(210.), top, bounds, px(100.)), MIN_SECTION);
        assert_eq!(dragged_height(px(2000.), top, bounds, px(100.)), px(850.) - top - px(100.));
        // A stack too short for everyone doesn't panic; the section gets its minimum.
        let short = Bounds::new(point(px(0.), px(0.)), size(px(300.), px(100.)));
        assert_eq!(dragged_height(px(90.), px(0.), short, px(100.)), MIN_SECTION);
    }

    #[test]
    fn a_saved_height_is_fitted_to_the_stack() {
        let below = MIN_SECTION * 2.;
        assert_eq!(fitted_size(px(900.), px(800.), below), px(800.) - below);
        assert_eq!(fitted_size(px(300.), px(800.), below), px(300.));
        assert_eq!(fitted_size(px(900.), px(100.), below), MIN_SECTION);
        assert_eq!(fitted_size(px(900.), px(0.), below), px(900.));
    }

    #[test]
    fn folded_sections_below_need_only_their_header() {
        let header = px(44.);
        assert_eq!(room_below(&[false, false, false], 0, header), MIN_SECTION * 2.);
        assert_eq!(room_below(&[false, false, true], 0, header), MIN_SECTION + header);
        assert_eq!(room_below(&[false, false, true], 2, header), px(0.));
    }

    #[test]
    fn the_last_open_section_cannot_be_folded() {
        let mut state = StackState::new(vec![None; 3]);
        state.toggle(0);
        state.toggle(2);
        assert!(state.folded(0) && state.folded(2) && !state.folded(1));
        state.toggle(1);
        assert!(!state.folded(1));
        state.toggle(0);
        assert!(!state.folded(0));
        // A saved layout that folds everything still leaves one open.
        let mut state = StackState::new(vec![None; 3]);
        for section in 0..3 {
            state.set_folded(section, true);
        }
        assert_eq!(state.folded, vec![true, true, false]);
    }

    #[test]
    fn a_width_that_was_chosen_is_kept_when_the_sidebar_opens_again() {
        let mut state = SidebarsState::new(px(280.), px(560.));
        state.bounds = Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(800.)));
        // Not chosen: opening gives it its default share.
        state.set_right_share(0.5);
        assert_eq!(state.right, px(500.));
        assert_eq!(state.widths().1, None);
        // Chosen (dragged, or restored): opening leaves it alone.
        state.set_widths(Some(px(300.)), Some(px(420.)));
        state.set_right_share(0.5);
        assert_eq!(state.widths(), (px(300.), Some(px(420.))));
    }
}
