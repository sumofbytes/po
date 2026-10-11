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
    dragging: Option<Side>,
    /// The whole layout's bounds, from the last frame.
    bounds: Bounds<Pixels>,
}

impl SidebarsState {
    pub fn new(left: Pixels, right: Pixels) -> Self {
        Self { left, right, dragging: None, bounds: Bounds::default() }
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
    left: Option<Sidebar>,
    center: Option<AnyElement>,
    right: Option<Sidebar>,
}

pub fn sidebars(state: &Entity<SidebarsState>) -> Sidebars {
    Sidebars { state: state.clone(), left: None, center: None, right: None }
}

impl Sidebars {
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

fn handle(state: &Entity<SidebarsState>, side: Side, active: bool, cx: &App) -> impl IntoElement {
    let palette = cx.global::<Palette>();
    let color = if active { palette.resize_handle_active } else { palette.resize_handle };
    let state = state.clone();
    div()
        .id(match side {
            Side::Left => "sidebar-handle-left",
            Side::Right => "sidebar-handle-right",
        })
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
                                            let width = right_shown.then_some(state.right).unwrap_or_default();
                                            state.left = dragged_width(Side::Left, event.position.x, state.bounds, width, &left_range);
                                        }
                                        Some(Side::Right) => {
                                            let width = left_shown.then_some(state.left).unwrap_or_default();
                                            state.right = dragged_width(Side::Right, event.position.x, state.bounds, width, &right_range);
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
                    .when(left_shown, |this| this.child(handle(&self.state, Side::Left, dragging == Some(Side::Left), cx))),
            )
            .when_some(right, |this, right| {
                this.child(
                    sidebar(right_width, &right.range)
                        .child(right.element)
                        .child(handle(&self.state, Side::Right, dragging == Some(Side::Right), cx)),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_CENTER, Side, dragged_width};
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
}
