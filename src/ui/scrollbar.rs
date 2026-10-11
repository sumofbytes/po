//! A vertical scrollbar like gpui-kit's in its default "show while scrolling" mode: the thumb
//! fades in when the view scrolls, stays two seconds after the last scroll (longer while the
//! pointer is on it), fades out, widens while dragged, and can be dragged or clicked along its
//! track.
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, Context, CursorStyle, Element, ElementId, Entity, GlobalElementId, Hitbox, HitboxBehavior,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    ScrollHandle, ScrollWheelEvent, Style, Task, Window, fill, point, px, relative, size,
};

use crate::theme::Palette;

const TRACK_WIDTH: Pixels = px(16.);
const THUMB_WIDTH: Pixels = px(6.);
const THUMB_ACTIVE_WIDTH: Pixels = px(8.);
const THUMB_INSET: Pixels = px(4.);
const MIN_THUMB: Pixels = px(48.);
const IDLE: Duration = Duration::from_secs(2);
const ENTER: Duration = Duration::from_millis(300);
const EXIT: Duration = Duration::from_millis(500);
const EXPAND: Duration = Duration::from_millis(300);

/// Something that scrolls vertically: where it is, how far it goes, and a way to move it.
pub trait ScrollSource: 'static {
    /// How far down it's scrolled, from 0 to [`Self::max`].
    fn offset(&self, cx: &App) -> Pixels;
    fn max(&self, cx: &App) -> Pixels;
    fn scroll_to(&self, y: Pixels, cx: &mut App);
}

impl ScrollSource for ScrollHandle {
    fn offset(&self, _: &App) -> Pixels {
        -ScrollHandle::offset(self).y
    }

    fn max(&self, _: &App) -> Pixels {
        self.max_offset().y
    }

    fn scroll_to(&self, y: Pixels, _: &mut App) {
        let y = y.clamp(px(0.), self.max_offset().y);
        self.set_offset(point(ScrollHandle::offset(self).x, -y));
    }
}

/// A value easing linearly towards a target.
#[derive(Clone, Copy)]
struct Transition {
    from: f32,
    target: f32,
    start: Instant,
    duration: Duration,
}

impl Transition {
    fn settled(value: f32) -> Self {
        Self { from: value, target: value, start: Instant::now(), duration: Duration::ZERO }
    }

    fn value(&self, now: Instant) -> f32 {
        let t = if self.duration.is_zero() {
            1.
        } else {
            (now.duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32()).min(1.)
        };
        self.from + (self.target - self.from) * t
    }

    fn running(&self, now: Instant) -> bool {
        self.value(now) != self.target
    }

    fn set(&mut self, target: f32, duration: Duration, now: Instant) {
        if target != self.target {
            *self = Self { from: self.value(now), target, start: now, duration };
        }
    }
}

/// What a scrollbar remembers between frames. Keep one per scrolled view.
pub struct ScrollbarState {
    last_offset: Option<Pixels>,
    scrolled_at: Option<Instant>,
    wake: Option<Task<()>>,
    /// Where on the thumb the pointer grabbed it, while dragging.
    grab: Option<Pixels>,
    hovered: bool,
    opacity: Transition,
    width: Transition,
}

impl Default for ScrollbarState {
    fn default() -> Self {
        Self {
            last_offset: None,
            scrolled_at: None,
            wake: None,
            grab: None,
            hovered: false,
            opacity: Transition::settled(0.),
            width: Transition::settled(THUMB_WIDTH.into()),
        }
    }
}

impl ScrollbarState {
    /// Takes this frame's scroll position, and says how opaque and wide the thumb is and
    /// whether it is still animating.
    fn frame(&mut self, offset: Pixels, cx: &mut Context<Self>) -> (f32, Pixels, bool) {
        let now = Instant::now();
        if self.last_offset.replace(offset).is_some_and(|last| last != offset) {
            self.scrolled_at = Some(now);
            // Wake once the idle hold is over, to start fading out.
            self.wake = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(IDLE).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            }));
        }
        let showing = self.opacity.value(now) > 0.;
        let wanted = self.grab.is_some()
            || self.scrolled_at.is_some_and(|at| now.duration_since(at) < IDLE)
            || (self.hovered && showing);
        self.opacity.set(if wanted { 1. } else { 0. }, if wanted { ENTER } else { EXIT }, now);
        let width = if self.grab.is_some() { THUMB_ACTIVE_WIDTH } else { THUMB_WIDTH };
        self.width.set(width.into(), EXPAND, now);
        let animating = self.opacity.running(now) || self.width.running(now);
        (self.opacity.value(now), px(self.width.value(now)), animating)
    }
}

/// Fills its parent, which must be `relative`, and draws along the right edge.
pub struct Scrollbar {
    state: Entity<ScrollbarState>,
    source: std::rc::Rc<dyn ScrollSource>,
}

impl Scrollbar {
    pub fn new(state: &Entity<ScrollbarState>, source: impl ScrollSource) -> Self {
        Self { state: state.clone(), source: std::rc::Rc::new(source) }
    }
}

impl IntoElement for Scrollbar {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

#[derive(Clone, Copy)]
struct Bar {
    track: Bounds<Pixels>,
    thumb: Bounds<Pixels>,
    /// Scroll per pixel of thumb travel.
    ratio: f32,
    opacity: f32,
}

pub struct Prepaint {
    bar: Option<Bar>,
    hitbox: Option<Hitbox>,
}

impl Element for Scrollbar {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

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
        style.position = gpui::Position::Absolute;
        style.inset.top = px(0.).into();
        style.inset.right = px(0.).into();
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
    ) -> Prepaint {
        let (max, offset) = (self.source.max(cx), self.source.offset(cx));
        let (opacity, width, animating) = self.state.update(cx, |state, cx| state.frame(offset, cx));
        if animating {
            window.request_animation_frame();
        }
        if max <= px(0.) {
            return Prepaint { bar: None, hitbox: None };
        }
        let viewport = bounds.size.height;
        let track = Bounds::new(point(bounds.right() - TRACK_WIDTH, bounds.top()), size(TRACK_WIDTH, viewport));
        let height = (viewport * (viewport / (viewport + max))).max(MIN_THUMB).min(viewport);
        let travel = viewport - height;
        let thumb = Bounds::new(
            point(bounds.right() - THUMB_INSET - width, bounds.top() + travel * (offset / max).clamp(0., 1.)),
            size(width, height),
        );
        let ratio = if travel > px(0.) { max / travel } else { 0. };
        let hitbox = window.insert_hitbox(track, HitboxBehavior::BlockMouse);
        Prepaint { bar: Some(Bar { track, thumb, ratio, opacity }), hitbox: Some(hitbox) }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (Some(bar), Some(hitbox)) = (prepaint.bar, prepaint.hitbox.clone()) else {
            self.state.update(cx, |state, _| state.hovered = false);
            return;
        };
        window.set_cursor_style(CursorStyle::Arrow, &hitbox);
        if bar.opacity > 0. {
            let palette = cx.global::<Palette>();
            let state = self.state.read(cx);
            let thumb = if state.grab.is_some() || hitbox.is_hovered(window) {
                palette.scrollbar_thumb_hover
            } else {
                palette.scrollbar_thumb
            };
            window.paint_quad(fill(bar.track, palette.scrollbar_track.opacity(bar.opacity)));
            window.paint_quad(fill(bar.thumb, thumb.opacity(bar.opacity)).corner_radii(palette.radius));
        }

        let (state, source, track) = (self.state.clone(), self.source.clone(), hitbox.clone());
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if !phase.bubble() || event.button != MouseButton::Left || !track.is_hovered(window) {
                return;
            }
            let thumb = bar.thumb;
            let grab = if thumb.top() <= event.position.y && event.position.y < thumb.bottom() {
                event.position.y - thumb.top()
            } else {
                // Clicking the track brings the thumb's middle under the pointer.
                let grab = thumb.size.height / 2.;
                source.scroll_to((event.position.y - bar.track.top() - grab) * bar.ratio, cx);
                grab
            };
            state.update(cx, |state, cx| {
                state.grab = Some(grab);
                cx.notify();
            });
            cx.stop_propagation();
        });
        let (state, source, track) = (self.state.clone(), self.source.clone(), hitbox);
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if !phase.bubble() {
                return;
            }
            let hovered = track.is_hovered(window);
            let grab = state.read(cx).grab;
            if let Some(grab) = grab {
                source.scroll_to((event.position.y - bar.track.top() - grab) * bar.ratio, cx);
            }
            state.update(cx, |state, cx| {
                if grab.is_some() || state.hovered != hovered {
                    state.hovered = hovered;
                    cx.notify();
                }
            });
        });
        // The track blocks the pointer from what's under it, so it passes the wheel on itself.
        let (source, track) = (self.source.clone(), prepaint.hitbox.clone().unwrap());
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase.bubble() && track.is_hovered(window) {
                let delta = event.delta.pixel_delta(window.line_height()).y;
                source.scroll_to(source.offset(cx) - delta, cx);
                cx.stop_propagation();
            }
        });
        let state = self.state.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
            if phase.bubble() {
                state.update(cx, |state, cx| {
                    if state.grab.take().is_some() {
                        cx.notify();
                    }
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::Transition;
    use std::time::{Duration, Instant};

    #[test]
    fn transitions_ease_from_where_they_are() {
        let start = Instant::now();
        let mut t = Transition::settled(0.);
        t.set(1., Duration::from_millis(300), start);
        assert_eq!(t.value(start), 0.);
        assert!((t.value(start + Duration::from_millis(150)) - 0.5).abs() < 1e-3);
        // Reversing midway starts from the current value, not the end.
        t.set(0., Duration::from_millis(500), start + Duration::from_millis(150));
        assert!((t.value(start + Duration::from_millis(150)) - 0.5).abs() < 1e-3);
        assert_eq!(t.value(start + Duration::from_secs(1)), 0.);
        assert!(!t.running(start + Duration::from_secs(1)));
    }
}
