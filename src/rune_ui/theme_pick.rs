//! The theme list in the settings: a button that opens a searchable list of the bundled schemes,
//! each with the name first and bars of its colours at the right. Typing filters; the arrows move
//! and Enter picks.
use rune::{El, Key, ScrollSpec, TextEdit, col, el, icon, row, text, tok::TextStyle};
use rune_kit::{Kit, Placement, Popover, TextInput};

use super::dropdown::CHEVRON;
use crate::schemes;

/// Where the host keeps the state, and what it does with the name picked.
pub type At<S> = std::rc::Rc<dyn for<'a> Fn(&'a mut S) -> &'a mut State>;
pub type Choose<S> = std::rc::Rc<dyn Fn(&mut S, &str)>;

const ROW: f32 = 28.0;
const SHOWN: f32 = 280.0;
fn small_style() -> TextStyle {
    crate::fonts::ui(13.0, 400.0)
}

#[derive(Default)]
pub struct State {
    pub open: bool,
    pub search: TextEdit,
    /// The highlighted row, among those the search leaves.
    active: usize,
}

impl State {
    fn close(&mut self) {
        self.open = false;
        self.search.focused = false;
    }
}

/// The schemes the search leaves: names that start with it first, then those that contain it.
fn matching(query: &str) -> Vec<&'static (&'static str, Vec<String>)> {
    let query = query.trim().to_lowercase();
    let all = schemes::previews().iter();
    if query.is_empty() {
        return all.collect();
    }
    let (mut first, mut rest) = (vec![], vec![]);
    for scheme in all {
        let name = scheme.0.to_lowercase();
        if name.starts_with(&query) {
            first.push(scheme);
        } else if name.contains(&query) {
            rest.push(scheme);
        }
    }
    first.extend(rest);
    first
}

/// The button, showing `current`, and (in `layer`) the list when it is open. `at` reaches the
/// state; `choose` is told the name picked.
pub fn view<S: 'static>(
    k: &Kit,
    state: &State,
    current: &str,
    width: f32,
    at: At<S>,
    choose: Choose<S>,
    layer: &mut Vec<El>,
) -> El {
    let label = if current.is_empty() {
        "Choose a theme"
    } else {
        current
    };
    let face = row()
        .key("theme-pick")
        .w(width)
        .px(10.0)
        .py(6.0)
        .radius(6.0)
        .gap(6.0)
        .items_center()
        .fill(k.c.layer_02)
        .child(
            row()
                .grow(1.0)
                .min_w(0.0)
                .child(text(label).font(small_style()).color(k.c.text_base)),
        )
        .child(icon(&CHEVRON, 14.0).color(k.c.text_muted));
    let press_at = at.clone();
    let current_name = current.to_string();
    let trigger = k.interactive(face).on_press(move |s: &mut S| {
        let state = press_at(s);
        state.open = !state.open;
        state.search = TextEdit::new("");
        state.search.focused = state.open;
        // Start on the theme in use, so the list opens scrolled to it.
        state.active = matching("")
            .iter()
            .position(|scheme| scheme.0.eq_ignore_ascii_case(current_name.trim()))
            .unwrap_or(0);
    });
    let close_at = at.clone();
    let popover = Popover::new("theme-list")
        .trigger(trigger)
        .label("Theme")
        .placement(Placement::BottomStart)
        .padding(0.0)
        .open(state.open)
        .on_close(move |s: &mut S| close_at(s).close());
    if !state.open {
        return popover.build(k, layer);
    }

    let shown = matching(&state.search.text);
    let active = state.active.min(shown.len().saturating_sub(1));
    let (edit_at, change_at, key_at, key_choose) =
        (at.clone(), at.clone(), at.clone(), choose.clone());
    let field = TextInput::new(&state.search)
        .plain()
        .leading_icon(k.glyph.search)
        .height(36.0)
        .width(None)
        .placeholder("Search themes")
        .label("Search themes")
        .on_edit(move |s: &mut S| &mut edit_at(s).search)
        .on_change(move |s: &mut S, tx| {
            change_at(s).active = 0;
            if let Some(first) = matching(&change_at(s).search.text).first() {
                tx.scroll_into_view(format!("theme-option-{}", first.0));
            }
        })
        .on_key(move |s: &mut S, event, tx| {
            let state = key_at(s);
            if state.search.preedit().is_some() {
                return false;
            }
            let shown = matching(&state.search.text);
            let last = shown.len().saturating_sub(1);
            let delta: isize = match &event.key {
                Key::Escape => {
                    state.close();
                    return true;
                }
                Key::Enter => {
                    let name = shown.get(state.active.min(last)).map(|scheme| scheme.0);
                    state.close();
                    if let Some(name) = name {
                        key_choose(s, name);
                    }
                    return true;
                }
                Key::Down => 1,
                Key::Up => -1,
                _ => return false,
            };
            if !shown.is_empty() {
                state.active = (state.active.min(last) as isize + delta)
                    .rem_euclid(shown.len() as isize) as usize;
                tx.scroll_into_view(format!("theme-option-{}", shown[state.active].0));
            }
            true
        })
        .build(k)
        .key("theme-search");

    let mut rows = col()
        .key("rows")
        .w_full()
        .growth(rune::GrowthSpec::default())
        .virtualize(ROW)
        .reveal_selected(true);
    for (i, (name, colors)) in shown.iter().enumerate() {
        let name = *name;
        let (hover_at, press_at, press_choose) = (at.clone(), at.clone(), choose.clone());
        let mut bars = row().gap(2.0).items_center();
        for color in colors.iter().filter_map(|c| rune::color::hex(c)) {
            bars = bars.child(el().w(4.0).h(14.0).radius(2.0).fill(color));
        }
        let on = name.eq_ignore_ascii_case(current);
        let check = icon(k.glyph.check_small, 14.0)
            .color(k.c.icon_base)
            .opacity(if on { 1.0 } else { 0.0 });
        rows = rows.child(
            row()
                .key(format!("theme-option-{name}"))
                .role("option")
                .label(name)
                .focus_selected(i == active)
                .w_full()
                .h(ROW)
                .px(8.0)
                .gap(8.0)
                .items_center()
                .radius(4.0)
                .fill(if i == active {
                    k.c.hover
                } else {
                    rune::Color::TRANSPARENT
                })
                .child(
                    text(name)
                        .font(small_style())
                        .color(k.c.text_base)
                        .grow(1.0)
                        .shrink()
                        .truncate()
                        .a11y_hidden(),
                )
                .child(bars)
                .child(el().w(14.0).h(14.0).child(check))
                .on_hover(move |s: &mut S| hover_at(s).active = i)
                .on_press(move |s: &mut S| {
                    press_at(s).close();
                    press_choose(s, name);
                }),
        );
    }
    let list = if shown.is_empty() {
        row()
            .h(ROW)
            .px(8.0)
            .items_center()
            .child(text("No themes").font(small_style()).color(k.c.text_muted))
    } else {
        k.scroll_bar(
            col()
                .w_full()
                .h((shown.len() as f32 * ROW).min(SHOWN))
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(rows),
        )
    };
    let body = col()
        .w(width.max(320.0))
        .shrink()
        .min_h(0.0)
        .child(row().w_full().px(4.0).child(field))
        .child(el().w_full().h(1.0).fill(k.c.border_weak).a11y_hidden())
        .child(col().w_full().padding(4.0).child(list));
    popover.child(body).build(k, layer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Cx, Env, Runtime};

    #[derive(Default)]
    struct Host {
        pick: State,
        chosen: Option<String>,
    }

    fn host_view(s: &Host, _: &mut Cx<Host>) -> El {
        let mut layer = vec![];
        let at: At<Host> = std::rc::Rc::new(|s: &mut Host| &mut s.pick);
        let choose: Choose<Host> =
            std::rc::Rc::new(|s: &mut Host, name: &str| s.chosen = Some(name.to_string()));
        let body = super::view::<Host>(
            &Kit::neutral(true),
            &s.pick,
            s.chosen.as_deref().unwrap_or(""),
            260.0,
            at,
            choose,
            &mut layer,
        );
        rune_kit::root(col().child(body), layer)
    }

    #[test]
    fn clicking_the_button_opens_the_list_and_a_row_picks() {
        let mut rt = Runtime::new(
            Host::default(),
            host_view,
            (800.0, 600.0),
            Env::new(true, 1.0),
        );
        rt.frame(0.0);
        let trigger = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("theme-pick"))
            .expect("trigger");
        let (x, y) = (
            trigger.rect[0] + trigger.rect[2] / 2.0,
            trigger.rect[1] + trigger.rect[3] / 2.0,
        );
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        assert!(rt.state.pick.open);
        rt.frame(1.0);
        rt.frame(2.0);
        let row = rt
            .elements()
            .into_iter()
            .find(|e| {
                e.key
                    .as_deref()
                    .is_some_and(|k| k.starts_with("theme-option-"))
            })
            .expect("a row");
        let (x, y) = (
            row.rect[0] + row.rect[2] / 2.0,
            row.rect[1] + row.rect[3] / 2.0,
        );
        rt.pointer_move(x, y);
        rt.frame(3.0);
        rt.pointer_down(x, y);
        rt.frame(3.1);
        rt.pointer_up(x, y);
        rt.frame(3.2);
        assert!(!rt.state.pick.open);
        assert!(rt.state.chosen.is_some());
    }
}
