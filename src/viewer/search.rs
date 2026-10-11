//! Ctrl+F: find text in the viewer. The matcher follows gpui-kit's (plain text, ASCII case
//! folded unless switched off, wrapping navigation) and the bar is laid out like the kit's
//! search panel.
use std::ops::Range;

use aho_corasick::AhoCorasick;
use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    Styled, Subscription, WeakEntity, Window, div, prelude::FluentBuilder as _,
};

use super::CodeViewState;
use crate::theme::Palette;
use crate::ui::button::Button;
use crate::ui::icon::IconName;
use crate::ui::input::{Input, InputEvent, InputState};

/// The matches of one query, and which is current.
#[derive(Default)]
pub struct Matches {
    pub query: String,
    pub case_sensitive: bool,
    pub ranges: Vec<Range<usize>>,
    pub current: usize,
}

impl Matches {
    /// Finds `query` in `text` afresh; the current match goes back to the first.
    pub fn find(&mut self, text: &str) {
        self.ranges.clear();
        self.current = 0;
        if self.query.is_empty() {
            return;
        }
        let Ok(matcher) = AhoCorasick::builder().ascii_case_insensitive(!self.case_sensitive).build([&self.query]) else {
            return;
        };
        self.ranges.extend(matcher.find_iter(text).map(|m| m.range()));
    }

    /// `2/5`, or `0/0` without matches.
    pub fn label(&self) -> String {
        if self.ranges.is_empty() { "0/0".into() } else { format!("{}/{}", self.current + 1, self.ranges.len()) }
    }

    pub fn next(&mut self) -> Option<Range<usize>> {
        if self.ranges.is_empty() {
            return None;
        }
        self.current = if self.current + 1 < self.ranges.len() { self.current + 1 } else { 0 };
        self.ranges.get(self.current).cloned()
    }

    pub fn previous(&mut self) -> Option<Range<usize>> {
        if self.ranges.is_empty() {
            return None;
        }
        self.current = self.current.checked_sub(1).unwrap_or(self.ranges.len() - 1);
        self.ranges.get(self.current).cloned()
    }

    /// Makes the first match that reaches `offset` current, or the last.
    pub fn start_at(&mut self, offset: usize) {
        let ix = self.ranges.partition_point(|r| r.end < offset);
        self.current = ix.min(self.ranges.len().saturating_sub(1));
    }
}

gpui::actions!(search_bar, [CloseSearch]);

pub fn init(cx: &mut gpui::App) {
    cx.bind_keys([gpui::KeyBinding::new("escape", CloseSearch, Some("SearchPanel"))]);
}

/// The bar shown above the text while searching.
pub struct SearchBar {
    view: WeakEntity<CodeViewState>,
    input: Entity<InputState>,
    _subscription: Subscription,
}

impl SearchBar {
    pub fn new(view: WeakEntity<CodeViewState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let _subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                let query = input.read(cx).value().to_string();
                this.view.update(cx, |view, cx| view.set_query(query, cx)).ok();
            }
            InputEvent::PressEnter { shift } => {
                let shift = *shift;
                this.view.update(cx, |view, cx| if shift { view.previous_match(cx) } else { view.next_match(cx) }).ok();
            }
        });
        Self { view, input, _subscription }
    }

    /// Shows `query` selected in a focused field, as reopening the kit's panel did.
    pub fn show(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            if !query.is_empty() {
                input.set_value(query.to_string(), window, cx);
            }
            input.select_all(window, cx);
            input.focus(window, cx);
        });
    }

    fn close(&mut self, _: &CloseSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.view.update(cx, |view, cx| view.close_search(window, cx)).ok();
    }
}

impl Render for SearchBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, has_matches, case_sensitive) = self
            .view
            .read_with(cx, |view, _| (view.matches.label(), !view.matches.ranges.is_empty(), view.matches.case_sensitive))
            .unwrap_or_default();
        let palette = cx.global::<Palette>();
        let view = self.view.clone();
        let on = |f: fn(&mut CodeViewState, &mut Context<CodeViewState>)| {
            let view = view.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                view.update(cx, |view, cx| f(view, cx)).ok();
            }
        };
        div()
            .id("search-panel")
            .flex()
            .flex_col()
            .occlude()
            .key_context("SearchPanel")
            .on_action(cx.listener(Self::close))
            .font_family(palette.font_family.clone())
            .items_center()
            .py_2()
            .px_3()
            .w_full()
            .gap_1()
            .bg(palette.popover)
            .border_b_1()
            .rounded(palette.radius / 2.)
            .border_color(palette.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap_2()
                    .child(
                        div().flex().flex_1().gap_1().child(
                            Input::new(&self.input).field().suffix(
                                Button::new("case-insensitive")
                                    .text()
                                    .xsmall()
                                    .selected(case_sensitive)
                                    .icon(IconName::CaseSensitive)
                                    .on_click(on(CodeViewState::toggle_case)),
                            ),
                        ),
                    )
                    .child(
                        Button::new("prev")
                            .xsmall()
                            .ghost()
                            .icon(IconName::ChevronLeft)
                            .disabled(!has_matches)
                            .on_click(on(CodeViewState::previous_match)),
                    )
                    .child(
                        Button::new("next")
                            .xsmall()
                            .ghost()
                            .icon(IconName::ChevronRight)
                            .disabled(!has_matches)
                            .on_click(on(CodeViewState::next_match)),
                    )
                    .child(
                        div()
                            .text_left()
                            .min_w_16()
                            .when(!has_matches, |this| this.text_color(palette.muted_foreground))
                            .child(SharedString::from(label)),
                    )
                    .child(div().w_7())
                    .child(Button::new("close").xsmall().ghost().icon(IconName::Close).on_click(
                        cx.listener(|this, _, window, cx| this.close(&CloseSearch, window, cx)),
                    )),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::Matches;

    fn matches(text: &str, query: &str, case_sensitive: bool) -> Matches {
        let mut m = Matches { query: query.into(), case_sensitive, ..Default::default() };
        m.find(text);
        m
    }

    #[test]
    fn navigation_wraps_like_the_kit() {
        let mut m = matches("Hello 世界 this is a Is test string.", "Is", false);
        assert_eq!(m.ranges.len(), 3);
        assert_eq!(m.label(), "1/3");
        assert_eq!(m.next(), Some(18..20));
        assert_eq!(m.next(), Some(23..25));
        assert_eq!(m.next(), Some(15..17));
        assert_eq!(m.previous(), Some(23..25));
        assert_eq!(m.label(), "3/3");
        assert_eq!(matches("Hello this is", "IS", true).label(), "0/0");
    }

    #[test]
    fn starts_at_the_first_match_reaching_an_offset() {
        let mut m = matches(".....aaaaa.....aaaaa.....aaaaa", "aaaaa", false);
        for (offset, ix) in [(0, 0), (5, 0), (12, 1), (16, 1), (30, 2), (31, 2)] {
            m.start_at(offset);
            assert_eq!(m.current, ix, "offset {offset}");
        }
    }
}
