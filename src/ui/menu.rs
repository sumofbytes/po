//! Popup menus like gpui-kit's: a popover of items, highlighted under the pointer or by
//! Up/Down, chosen by click or Enter, dismissed by Escape or a click elsewhere. A [`dropdown`]
//! opens one under a button; [`ContextMenu`] opens one at the pointer. Focus goes back where it
//! was when the menu closes.
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, BoxShadow, Context, ElementId, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, KeyBinding, MouseButton, ParentElement as _, Pixels, Point, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, anchored, canvas, deferred, div, hsla, point,
    prelude::FluentBuilder as _, px, rems,
};

use super::button::Button;
use crate::theme::Palette;

gpui::actions!(popup_menu, [SelectUp, SelectDown, Confirm, Cancel]);

const CONTEXT: &str = "PopupMenu";
const ITEM_HEIGHT: Pixels = px(26.);
/// The gap between a button and its menu.
const OFFSET: Pixels = px(4.);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectUp, Some(CONTEXT)),
        KeyBinding::new("down", SelectDown, Some(CONTEXT)),
        KeyBinding::new("enter", Confirm, Some(CONTEXT)),
        KeyBinding::new("escape", Cancel, Some(CONTEXT)),
    ]);
}

pub struct MenuItem {
    label: SharedString,
    on_click: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    disabled: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<SharedString>, on_click: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self { label: label.into(), on_click: Some(Rc::new(on_click)), disabled: false }
    }

    /// Shown greyed out, for what doesn't apply.
    pub fn disabled(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), on_click: None, disabled: true }
    }

    pub fn separator() -> Self {
        Self { label: SharedString::default(), on_click: None, disabled: true }
    }

    fn is_separator(&self) -> bool {
        self.disabled && self.label.is_empty()
    }

    fn choosable(&self) -> bool {
        !self.disabled
    }
}

/// An open menu: where it is, what's highlighted, and what to give focus back to.
pub struct MenuState {
    open: bool,
    position: Point<Pixels>,
    highlighted: Option<usize>,
    focus: FocusHandle,
    restore: Option<FocusHandle>,
}

impl MenuState {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self { open: false, position: Point::default(), highlighted: None, focus: cx.focus_handle(), restore: None }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the menu with its top-left corner at `position`, taking focus.
    pub fn open_at(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        self.position = position;
        self.highlighted = None;
        self.restore = window.focused(cx);
        self.focus.focus(window, cx);
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        self.highlighted = None;
        if let Some(focus) = self.restore.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn step(&mut self, step: isize, items: &[MenuItem]) {
        if let Some(ix) = next_choosable(items, self.highlighted, step) {
            self.highlighted = Some(ix);
        }
    }
}

/// The next item from `from` that can be chosen, `step` 1 or -1, wrapping around.
fn next_choosable(items: &[MenuItem], from: Option<usize>, step: isize) -> Option<usize> {
    let count = items.len() as isize;
    if !items.iter().any(MenuItem::choosable) {
        return None;
    }
    let mut ix = from.map_or(if step > 0 { -1 } else { count }, |ix| ix as isize);
    loop {
        ix = (ix + step).rem_euclid(count);
        if items[ix as usize].choosable() {
            return Some(ix as usize);
        }
    }
}

/// The popup surface's shadow, as the kit's: a hairline ring and a soft drop.
fn shadow(ring: gpui::Hsla) -> Vec<BoxShadow> {
    let ink = hsla(0., 0., 0., 0.1);
    let shadow = |y: f32, blur: f32, spread: f32, color| BoxShadow {
        color,
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset: false,
    };
    vec![shadow(0., 0., 1., ring), shadow(4., 3., -1., ink), shadow(2., 2., -2., ink)]
}

/// The open menu of `state` with `items`, floated over everything. A click in `keep` (the
/// button that opened it) is left to toggle the menu itself.
pub fn menu(state: &Entity<MenuState>, items: Vec<MenuItem>, keep: Option<Bounds<Pixels>>, cx: &App) -> Option<AnyElement> {
    let s = state.read(cx);
    if !s.open {
        return None;
    }
    let (position, highlighted, focus) = (s.position, s.highlighted, s.focus.clone());
    let palette = cx.global::<Palette>();
    let radius = palette.radius.min(px(8.));
    let (accent, accent_fg, fg, muted, border) =
        (palette.accent, palette.accent_foreground, palette.foreground, palette.muted_foreground, palette.border);
    let items = Rc::new(items);
    let rows = items.iter().enumerate().map(|(ix, item)| {
        if item.is_separator() {
            return div().h_auto().p_0().my_0p5().mx_neg_1().border_b(px(2.)).border_color(border).into_any_element();
        }
        let (hover, choose) = (state.clone(), state.clone());
        let on_click = item.on_click.clone();
        div()
            .id(ix)
            .flex()
            .items_center()
            .h(ITEM_HEIGHT)
            .px(px(8.))
            .text_sm()
            .rounded(radius)
            .text_color(if item.disabled { muted } else { fg })
            .when(!item.disabled, |this| {
                this.hover(move |this| this.bg(accent).text_color(accent_fg))
                    .when(highlighted == Some(ix), |this| this.bg(accent).text_color(accent_fg))
                    .on_hover(move |hovered, _, cx| {
                        if *hovered {
                            hover.update(cx, |s, cx| {
                                s.highlighted = Some(ix);
                                cx.notify();
                            });
                        }
                    })
                    .on_click(move |_, window, cx| {
                        choose.update(cx, |s, cx| s.close(window, cx));
                        if let Some(on_click) = &on_click {
                            on_click(window, cx);
                        }
                    })
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(item.label.clone())
            .into_any_element()
    });
    let (up, down, confirm, cancel, outside) = (state.clone(), state.clone(), state.clone(), state.clone(), state.clone());
    let (up_items, down_items, chosen) = (items.clone(), items.clone(), items.clone());
    let panel = div()
        .id("popup-menu")
        .key_context(CONTEXT)
        .track_focus(&focus)
        .occlude()
        .bg(palette.popover)
        .text_color(palette.popover_foreground)
        .shadow(shadow(palette.popover_ring))
        .rounded(palette.radius)
        .on_action(move |_: &SelectUp, _, cx| up.update(cx, |s, cx| {
            s.step(-1, &up_items);
            cx.notify();
        }))
        .on_action(move |_: &SelectDown, _, cx| down.update(cx, |s, cx| {
            s.step(1, &down_items);
            cx.notify();
        }))
        .on_action(move |_: &Confirm, window, cx| {
            let ix = confirm.read(cx).highlighted;
            confirm.update(cx, |s, cx| s.close(window, cx));
            if let Some(on_click) = ix.and_then(|ix| chosen.get(ix)).and_then(|item| item.on_click.clone()) {
                on_click(window, cx);
            }
        })
        .on_action(move |_: &Cancel, window, cx| cancel.update(cx, |s, cx| s.close(window, cx)))
        .on_mouse_down_out(move |event, window, cx| {
            if !keep.is_some_and(|keep| keep.contains(&event.position)) {
                outside.update(cx, |s, cx| s.close(window, cx));
            }
        })
        .child(div().flex().flex_col().p_1().gap_y_0p5().min_w(rems(8.)).max_w(px(500.)).children(rows));
    Some(deferred(anchored().position(position).snap_to_window_with_margin(px(8.)).child(panel)).into_any_element())
}

/// `button`, opening a menu of what `items` returns each time it opens.
pub fn dropdown(id: impl Into<ElementId>, button: Button, items: impl Fn() -> Vec<MenuItem> + 'static) -> Dropdown {
    Dropdown { id: id.into(), button, items: Rc::new(items) }
}

#[derive(IntoElement)]
pub struct Dropdown {
    id: ElementId,
    button: Button,
    items: Rc<dyn Fn() -> Vec<MenuItem>>,
}

/// A dropdown's menu and where its button is.
struct DropdownState {
    menu: Entity<MenuState>,
    trigger: Bounds<Pixels>,
}

impl RenderOnce for Dropdown {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| DropdownState {
            menu: cx.new(MenuState::new),
            trigger: Bounds::default(),
        });
        let (menu_state, trigger) = {
            let s = state.read(cx);
            (s.menu.clone(), s.trigger)
        };
        let toggle = menu_state.clone();
        let button = self.button.on_click(move |_, window, cx| {
            toggle.update(cx, |menu, cx| {
                if menu.is_open() {
                    menu.close(window, cx);
                } else {
                    menu.open_at(point(trigger.left(), trigger.bottom() + OFFSET), window, cx);
                }
            });
        });
        let open = menu_state.read(cx).is_open().then(|| (self.items)());
        div()
            .relative()
            .child(button)
            .child(canvas(move |bounds, _, cx| state.update(cx, |s, _| s.trigger = bounds), |_, _, _, _| {}).absolute().size_full())
            .children(open.and_then(|items| menu(&menu_state, items, Some(trigger), cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::{MenuItem, next_choosable};

    #[test]
    fn keys_skip_disabled_items_and_separators() {
        let items = vec![
            MenuItem::disabled("Go to Definition"),
            MenuItem::separator(),
            MenuItem::new("Copy", |_, _| {}),
            MenuItem::separator(),
            MenuItem::new("Select All", |_, _| {}),
        ];
        assert_eq!(next_choosable(&items, None, 1), Some(2));
        assert_eq!(next_choosable(&items, Some(2), 1), Some(4));
        assert_eq!(next_choosable(&items, Some(4), 1), Some(2));
        assert_eq!(next_choosable(&items, None, -1), Some(4));
        assert_eq!(next_choosable(&[MenuItem::separator()], None, 1), None);
    }
}
