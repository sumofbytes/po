//! A window of NSH's over the app: the backdrop that closes it, and the card the screen sits in,
//! as tall as the screen says it needs (as far as the app's window has room). The screen is only
//! started while it is open, and dropping it stops its engine.
use std::sync::LazyLock;

use gpui::{
    Context, Entity, EventEmitter, Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use nsh_platform_gpui::{AppView, Root};
use tokio::sync::watch;

use crate::theme::Palette;

/// What a screen tells the frame around it: the height its content needs.
pub type Height = LazyLock<watch::Sender<f32>>;

fn unmeasured() -> watch::Sender<f32> {
    watch::Sender::new(0.)
}

/// A height not yet measured.
pub const fn height() -> Height {
    LazyLock::new(unmeasured)
}

pub enum OverlayEvent {
    /// A press on the backdrop.
    Close,
}

pub struct Overlay {
    root: fn() -> Root,
    width: f32,
    /// Kept clear above the card, and below it.
    margin: f32,
    /// Made as the window opens and dropped as it closes, which stops its engine.
    view: Option<(Entity<AppView>, Subscription)>,
    height: f32,
    /// Fills the space it is given instead of floating over it, with the screen in a centered column.
    page: bool,
    _follow: Task<()>,
}

impl EventEmitter<OverlayEvent> for Overlay {}

impl Overlay {
    pub fn new(width: f32, margin: f32, root: fn() -> Root, measured: &'static Height, cx: &mut Context<Self>) -> Self {
        let mut measured = measured.subscribe();
        let follow = cx.spawn(async move |overlay, cx| {
            while measured.changed().await.is_ok() {
                let now = *measured.borrow_and_update();
                if overlay.update(cx, |overlay, cx| {
                    overlay.height = now;
                    cx.notify();
                }).is_err() {
                    return;
                }
            }
        });
        Self { root, width, margin, view: None, height: 0., page: false, _follow: follow }
    }

    /// Shows the screen as a page of the app, `width` wide at most, rather than as a window over it.
    pub fn page(width: f32, root: fn() -> Root, measured: &'static Height, cx: &mut Context<Self>) -> Self {
        Self { page: true, ..Self::new(width, 0., root, measured, cx) }
    }

    /// Starts the screen and gives it the keyboard, as the window opens.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (view, theme) = super::mount((self.root)(), window, cx);
        window.focus(&view.focus_handle(cx), cx);
        self.view = Some((view, theme));
        cx.notify();
    }

    /// Stops the screen, as the window closes.
    pub fn close(&mut self) {
        self.view = None;
        self.height = 0.;
    }
}

impl Render for Overlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.global::<Palette>();
        let (popover, border, radius) = (colors.popover, colors.border, colors.radius);
        if self.page {
            return div()
                .size_full()
                .flex()
                .justify_center()
                .bg(popover)
                .child(div().h_full().w_full().max_w(px(self.width)).flex().flex_col().children(self.view.as_ref().map(|(view, _)| view.clone())))
                .into_any_element();
        }
        div()
            .absolute()
            .inset_0()
            .id("overlay-backdrop")
            .occlude()
            .on_click(cx.listener(|_, _: &gpui::ClickEvent, _, cx| cx.emit(OverlayEvent::Close)))
            .flex()
            .justify_center()
            .items_start()
            .py(px(self.margin))
            .child(
                div()
                    .id("overlay")
                    .occlude()
                    // A click inside is the screen's, not the backdrop's.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .w(px(self.width))
                    // Until the screen has measured itself, it takes the room it has.
                    .when(self.height > 0., |card| card.h(px(self.height + 2.)))
                    .when(self.height <= 0., |card| card.h_full())
                    .max_h_full()
                    .rounded(radius * 3.)
                    .border_1()
                    .border_color(border)
                    .shadow_2xl()
                    .bg(popover)
                    .overflow_hidden()
                    .children(self.view.as_ref().map(|(view, _)| view.clone())),
            )
            .into_any_element()
    }
}
