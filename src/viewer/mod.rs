//! A read-only code viewer drawn directly with gpui: soft-wrapped lines, a line-number gutter,
//! tree-sitter colors, a scrollbar that shows while scrolling, and a caret and selection that
//! move like gpui-kit's editor.
mod cursor;
mod element;
mod highlight;
mod languages;
mod search;
mod text;
mod wrap;

use std::sync::Arc;

use gpui::{
    App, AppContext as _, Context, FocusHandle, InteractiveElement, IntoElement, KeyBinding, ParentElement, Pixels, Point, Refineable,
    Render, RenderOnce, SharedString, StyleRefinement, Styled, Subscription, Task, Window, div, px,
};

use crate::ui::menu::{MenuItem, MenuState};
use crate::ui::scrollbar::{ScrollSource, Scrollbar, ScrollbarState};
use cursor::{Geometry, Selection};
use element::CodeElement;
use highlight::Highlights;
use search::{Matches, SearchBar};
use text::Text;
use wrap::WrapMap;

/// Files up to this size are highlighted before their first frame; bigger ones show plain text
/// for the moment their parse takes in the background.
const SYNC_HIGHLIGHT: usize = 128 * 1024;

gpui::actions!(
    code_view,
    [
        MoveLeft, MoveRight, MoveUp, MoveDown, MovePageUp, MovePageDown, MoveHome, MoveEnd, MoveToStart, MoveToEnd,
        MoveToPreviousWord, MoveToNextWord, SelectLeft, SelectRight, SelectUp, SelectDown, SelectToStartOfLine,
        SelectToEndOfLine, SelectToStart, SelectToEnd, SelectToPreviousWordStart, SelectToNextWordEnd, SelectAll,
        Copy, Search,
    ]
);

const CONTEXT: &str = "CodeView";

/// Binds the viewer's keys: the kit's editor bindings for Linux and Windows that apply to
/// reading.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("left", MoveLeft, Some(CONTEXT)),
        KeyBinding::new("right", MoveRight, Some(CONTEXT)),
        KeyBinding::new("up", MoveUp, Some(CONTEXT)),
        KeyBinding::new("down", MoveDown, Some(CONTEXT)),
        KeyBinding::new("pageup", MovePageUp, Some(CONTEXT)),
        KeyBinding::new("pagedown", MovePageDown, Some(CONTEXT)),
        KeyBinding::new("home", MoveHome, Some(CONTEXT)),
        KeyBinding::new("end", MoveEnd, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-home", MoveToStart, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-end", MoveToEnd, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-left", MoveToPreviousWord, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-right", MoveToNextWord, Some(CONTEXT)),
        KeyBinding::new("shift-left", SelectLeft, Some(CONTEXT)),
        KeyBinding::new("shift-right", SelectRight, Some(CONTEXT)),
        KeyBinding::new("shift-up", SelectUp, Some(CONTEXT)),
        KeyBinding::new("shift-down", SelectDown, Some(CONTEXT)),
        KeyBinding::new("shift-home", SelectToStartOfLine, Some(CONTEXT)),
        KeyBinding::new("shift-end", SelectToEndOfLine, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-home", SelectToStart, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-end", SelectToEnd, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-left", SelectToPreviousWordStart, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-right", SelectToNextWordEnd, Some(CONTEXT)),
        #[cfg(target_os = "linux")]
        KeyBinding::new("alt-shift-left", SelectToPreviousWordStart, Some(CONTEXT)),
        #[cfg(target_os = "linux")]
        KeyBinding::new("alt-shift-right", SelectToNextWordEnd, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-left", MoveToPreviousWord, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-right", MoveToNextWord, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-left", MoveHome, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-right", MoveEnd, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-up", MoveToStart, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-down", MoveToEnd, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-shift-left", SelectToPreviousWordStart, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-shift-right", SelectToNextWordEnd, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-left", SelectToStartOfLine, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-right", SelectToEndOfLine, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-up", SelectToStart, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-down", SelectToEnd, Some(CONTEXT)),
        KeyBinding::new("secondary-a", SelectAll, Some(CONTEXT)),
        KeyBinding::new("secondary-c", Copy, Some(CONTEXT)),
        KeyBinding::new("secondary-f", Search, Some(CONTEXT)),
    ]);
    search::init(cx);
}

/// Compiles the grammars for `languages` (names or file extensions) on a background thread,
/// so opening the first file of each doesn't wait for its queries.
pub fn prewarm(languages: Vec<String>) {
    std::thread::spawn(move || highlight::prewarm(languages.iter().map(String::as_str)));
}

/// A code block's captures, worked out once and colored from the theme when drawn.
pub struct CodeColors {
    names: Vec<String>,
    runs: Vec<(std::ops::Range<usize>, u16)>,
}

impl CodeColors {
    /// Parses `code` in `language` (a name or file extension); `None` without a grammar.
    pub fn new(code: &str, language: &str) -> Option<Self> {
        let highlights = highlight::highlight(code, language)?;
        let mut at = 0;
        let runs = highlights
            .runs(0..code.len())
            .filter_map(|(len, id)| {
                let range = at..at + len;
                at += len;
                (id > 0).then_some((range, id))
            })
            .collect();
        Some(Self { names: highlights.names, runs })
    }

    /// Syntax colors for the code, for code shown outside the viewer, such as Markdown code
    /// blocks.
    pub fn styles(&self, cx: &App) -> Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> {
        self.styles_in(0..usize::MAX, &self.palette(cx))
    }

    /// The theme's style for each capture name, for [`Self::styles_in`].
    pub fn palette(&self, cx: &App) -> Vec<Option<gpui::HighlightStyle>> {
        let palette = cx.global::<crate::theme::Palette>();
        self.names.iter().map(|name| palette.syntax(name)).collect()
    }

    /// How many capture names the code uses, so tests can build a palette without a theme.
    #[cfg(test)]
    pub fn palette_len(&self) -> usize {
        self.names.len()
    }

    /// Syntax colors for `range` of the code, such as one of its lines, measured from the
    /// range's start; `palette` comes from [`Self::palette`].
    pub fn styles_in(
        &self,
        range: std::ops::Range<usize>,
        palette: &[Option<gpui::HighlightStyle>],
    ) -> Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> {
        let first = self.runs.partition_point(|(run, _)| run.end <= range.start);
        self.runs[first..]
            .iter()
            .take_while(|(run, _)| run.start < range.end)
            .filter_map(|(run, id)| {
                let style = (*palette.get(*id as usize - 1)?)?;
                let (start, end) = (run.start.max(range.start), run.end.min(range.end));
                Some((start - range.start..end - range.start, style))
            })
            .collect()
    }
}

/// Whether text in `language` (a name or file extension) gets colors, for tests outside.
#[cfg(test)]
pub fn highlights(text: &str, language: &str) -> bool {
    highlight::highlight(text, language).is_some_and(|h| !h.names.is_empty())
}

pub struct CodeViewState {
    text: Text,
    language: SharedString,
    highlights: Option<Arc<Highlights>>,
    highlighting: Option<Task<()>>,
    wrap: WrapMap,
    /// What `wrap` was computed for: wrap width, font size and family.
    wrap_key: Option<(Pixels, Pixels, SharedString)>,
    /// The line at the top of the view, which of its rows, and how far into that row, kept for
    /// the next wrap to scroll back to.
    anchor: Option<(usize, usize, Pixels)>,
    /// Row height from the last layout.
    line_height: Pixels,
    scroll_y: Pixels,
    /// The furthest `scroll_y` can go, from the last layout.
    max_scroll: Pixels,
    selection: Selection,
    /// Whether the caret, at a soft wrap point, is drawn at the end of the earlier row.
    affinity: bool,
    /// The x Up and Down keep to, set by the first of a run of vertical moves.
    goal: Option<Pixels>,
    /// Scroll the caret into view at the next layout.
    follow: bool,
    /// Set by the last layout.
    geometry: Option<Geometry>,
    /// A mouse button is down and dragging extends the selection.
    selecting: bool,
    /// The word a double-click selected, which a drag keeps selected.
    word: Option<std::ops::Range<usize>>,
    drag_at: Option<Point<Pixels>>,
    auto_scroll: Option<Task<()>>,
    /// Where a right-click went down, to open the menu at when it comes up.
    menu_at: Option<Point<Pixels>>,
    context_menu: gpui::Entity<MenuState>,
    blink_on: bool,
    blink_paused: bool,
    blink_epoch: usize,
    blink_task: Task<()>,
    _focus: [Subscription; 2],
    matches: Matches,
    /// The find bar, made the first time it opens.
    search_bar: Option<gpui::Entity<SearchBar>>,
    search_open: bool,
    /// An offset to scroll into view at the next layout without moving the caret.
    reveal: Option<usize>,
    /// Folded line ranges, by first line, with their last line; the lines between are hidden.
    folded: Vec<(usize, usize)>,
    gutter_hovered: bool,
    focus: FocusHandle,
    scrollbar: gpui::Entity<ScrollbarState>,
    /// Row metrics and syntax styles from the last layout, kept while their inputs hold.
    cache: element::Cache,
    /// Redraw the view, which is cached, when its scrollbar or menu changes.
    _children: [Subscription; 2],
}

impl CodeViewState {
    pub fn new(text: impl Into<Arc<str>>, language: impl Into<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = Text::new(text);
        let focus = cx.focus_handle();
        let subscriptions = [
            cx.on_focus(&focus, window, |this, _, cx| this.start_blink(cx)),
            cx.on_blur(&focus, window, |this, _, cx| this.stop_blink(cx)),
        ];
        let scrollbar = cx.new(|_| ScrollbarState::default());
        let context_menu = cx.new(MenuState::new);
        let children = [cx.observe(&scrollbar, |_, _, cx| cx.notify()), cx.observe(&context_menu, |_, _, cx| cx.notify())];
        let mut this = Self {
            wrap: WrapMap::unwrapped(text.lines()),
            text,
            language: language.into(),
            highlights: None,
            highlighting: None,
            wrap_key: None,
            anchor: None,
            line_height: px(0.),
            scroll_y: px(0.),
            max_scroll: px(0.),
            selection: Selection::default(),
            affinity: false,
            goal: None,
            follow: false,
            geometry: None,
            selecting: false,
            word: None,
            drag_at: None,
            auto_scroll: None,
            menu_at: None,
            context_menu,
            blink_on: false,
            blink_paused: false,
            blink_epoch: 0,
            blink_task: Task::ready(()),
            _focus: subscriptions,
            matches: Matches::default(),
            search_bar: None,
            search_open: false,
            reveal: None,
            folded: Vec::new(),
            gutter_hovered: false,
            focus,
            scrollbar,
            cache: element::Cache::default(),
            _children: children,
        };
        this.rehighlight(None, cx);
        this
    }

    /// Puts the caret at the start of `line` (from 0), unfolding what hides it, and scrolls it
    /// into view at the next layout.
    pub fn go_to_line(&mut self, line: usize, cx: &mut Context<Self>) {
        let line = line.min(self.text.lines().saturating_sub(1));
        if self.fold_hiding(line).is_some() {
            self.folded.retain(|&(start, end)| !(start < line && line < end));
            self.apply_folds();
        }
        let offset = self.text.line_range(line).start;
        self.selection = Selection { anchor: offset, head: offset };
        self.reveal = Some(offset);
        cx.notify();
    }

    /// Replaces the text, keeping the scroll position and cursor where they still fit.
    pub fn set_text(&mut self, text: impl Into<Arc<str>>, cx: &mut Context<Self>) {
        self.anchor = Some(self.top());
        let old = std::mem::replace(&mut self.text, Text::new(text));
        self.selection = Selection {
            anchor: self.text.clip_left(self.selection.anchor),
            head: self.text.clip_left(self.selection.head),
        };
        self.word = None;
        self.matches.find(self.text.src());
        self.sync_folds();
        // Wrapping redoes itself at the next layout, which scrolls back to the anchor.
        self.wrap = WrapMap::unwrapped(self.text.lines());
        self.wrap_key = None;
        self.rehighlight(Some(old.src().clone()), cx);
        cx.notify();
    }


    /// Ctrl+F: opens the find bar on the selected text, or the last query. A new query starts
    /// from the first match in view.
    fn open_search(&mut self, _: &Search, window: &mut Window, cx: &mut Context<Self>) {
        let selected = &self.text.src()[self.selection.range()];
        let query = if selected.is_empty() { self.matches.query.clone() } else { selected.to_string() };
        // The same query resumes at the match it was on.
        if query != self.matches.query {
            self.matches.query = query.clone();
            self.matches.find(self.text.src());
            let (top, _) = self.row_range(self.top_row().min(self.wrap.rows() - 1));
            self.matches.start_at(top.start);
        }
        self.search_open = true;
        let bar = match &self.search_bar {
            Some(bar) => bar.clone(),
            None => {
                let view = cx.weak_entity();
                let bar = cx.new(|cx| SearchBar::new(view, window, cx));
                self.search_bar = Some(bar.clone());
                bar
            }
        };
        bar.update(cx, |bar, cx| bar.show(&query, window, cx));
        cx.notify();
    }

    fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        if query != self.matches.query {
            self.matches.query = query;
            self.matches.find(self.text.src());
            cx.notify();
        }
    }

    fn toggle_case(&mut self, cx: &mut Context<Self>) {
        self.matches.case_sensitive = !self.matches.case_sensitive;
        self.matches.find(self.text.src());
        cx.notify();
    }

    /// Steps to the next match and scrolls to it; the selection stays where it is.
    fn next_match(&mut self, cx: &mut Context<Self>) {
        if let Some(range) = self.matches.next() {
            self.reveal = Some(range.end);
            cx.notify();
        }
    }

    fn previous_match(&mut self, cx: &mut Context<Self>) {
        if let Some(range) = self.matches.previous() {
            self.reveal = Some(range.start);
            cx.notify();
        }
    }

    /// Hides the bar and its highlights and gives the text focus back; the query stays for
    /// the next Ctrl+F.
    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// The lines that can fold, by first line, with their last.
    fn fold_candidates(&self) -> &[(usize, usize)] {
        self.highlights.as_ref().map_or(&[], |h| &h.folds)
    }

    /// The fold starting at `line`, if one can.
    fn fold_at(&self, line: usize) -> Option<(usize, usize)> {
        let folds = self.fold_candidates();
        folds.binary_search_by_key(&line, |&(start, _)| start).ok().map(|ix| folds[ix])
    }

    /// Folds or unfolds the block starting at `line`. A caret left inside it moves to the end
    /// of the block's first line.
    fn toggle_fold(&mut self, line: usize, cx: &mut Context<Self>) {
        if let Some(ix) = self.folded.iter().position(|&(start, _)| start == line) {
            self.folded.remove(ix);
        } else if let Some(fold) = self.fold_at(line) {
            let ix = self.folded.partition_point(|&(start, _)| start < line);
            self.folded.insert(ix, fold);
        }
        self.apply_folds();
        self.selection.anchor = self.visible_back(self.selection.anchor);
        self.selection.head = self.visible_back(self.selection.head);
        cx.notify();
    }

    /// Keeps folds whose block can still fold after the text or its parse changed.
    fn sync_folds(&mut self) {
        let folded = std::mem::take(&mut self.folded);
        self.folded = folded.into_iter().filter_map(|(start, _)| self.fold_at(start)).collect();
        self.apply_folds();
    }

    fn apply_folds(&mut self) {
        let hidden: Vec<_> = self.folded.iter().map(|&(start, end)| start + 1..end).collect();
        self.wrap.hide(&hidden);
    }

    /// The fold hiding `line`, if any.
    fn fold_hiding(&self, line: usize) -> Option<(usize, usize)> {
        self.folded.iter().copied().find(|&(start, end)| start < line && line < end)
    }

    /// An offset on a hidden line moves back to the end of its fold's first line.
    fn visible_back(&self, offset: usize) -> usize {
        match self.fold_hiding(self.text.line_of(offset)) {
            Some((start, _)) => self.text.line_range(start).end,
            None => offset,
        }
    }

    /// An offset on a hidden line moves on to the start of its fold's last line.
    fn visible_forward(&self, offset: usize) -> usize {
        match self.fold_hiding(self.text.line_of(offset)) {
            Some((_, end)) => self.text.line_range(end).start,
            None => offset,
        }
    }

    /// The display row at the top of the view.
    fn top_row(&self) -> usize {
        if self.line_height <= px(0.) { 0 } else { (self.scroll_y / self.line_height) as usize }
    }




    /// Where the view's top is: the line, which of its rows, and how far into that row.
    fn top(&self) -> (usize, usize, Pixels) {
        if self.line_height <= px(0.) {
            return (0, 0, px(0.));
        }
        let row = (self.scroll_y / self.line_height) as usize;
        let (line, sub) = self.wrap.line_of_row(row);
        (line, sub, self.scroll_y - self.line_height * row as f32)
    }

    /// Scrolls so `anchor` is at the top again after the rows changed.
    fn restore(&mut self, (line, sub, within): (usize, usize, Pixels)) {
        let line = line.min(self.text.lines() - 1);
        let sub = sub.min(self.wrap.breaks(line).len());
        self.scroll_y = self.line_height * (self.wrap.first_row(line) + sub) as f32 + within;
    }

    /// Colors the text. A new view colors small files before its first frame; a reload, given
    /// the text it replaced, reparses only what changed, in the background.
    fn rehighlight(&mut self, old_text: Option<Arc<str>>, cx: &mut Context<Self>) {
        let src = self.text.src().clone();
        let language = self.language.to_string();
        if old_text.is_none() && src.len() <= SYNC_HIGHLIGHT {
            self.highlighting = None;
            self.highlights = highlight::highlight(&src, &language).map(Arc::new);
            crate::trace::mark("viewer highlighted");
            return;
        }
        // Old colors stay until the new ones are ready; the element keeps them on character
        // boundaries if the text moved underneath them.
        let old = old_text.zip(self.highlights.clone());
        self.highlighting = Some(cx.spawn(async move |this, cx| {
            let highlights = cx
                .background_executor()
                .spawn(async move { highlight::rehighlight(&src, &language, old.as_ref().map(|(text, h)| (&**text, &**h))) })
                .await;
            this.update(cx, |this, cx| {
                this.highlights = highlights.map(Arc::new);
                this.highlighting = None;
                this.sync_folds();
                cx.notify();
            })
            .ok();
        }));
    }

    /// Scrolls to `y`; false if the view was already there.
    fn scroll_to(&mut self, y: Pixels, cx: &mut Context<Self>) -> bool {
        let y = y.clamp(px(0.), self.max_scroll);
        if y == self.scroll_y {
            return false;
        }
        self.scroll_y = y;
        cx.notify();
        true
    }
}

/// The state draws itself filling its bounds, so it can be embedded as a cached view and skip
/// rendering while only the rest of the window changes.
impl Render for CodeViewState {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        CodeView::new(&cx.entity()).size_full()
    }
}

/// Draws a [`CodeViewState`]. Sized like any element, e.g. `CodeView::new(&state).h_full()`.
#[derive(IntoElement)]
pub struct CodeView {
    state: gpui::Entity<CodeViewState>,
    style: StyleRefinement,
}

impl CodeView {
    pub fn new(state: &gpui::Entity<CodeViewState>) -> Self {
        Self { state: state.clone(), style: StyleRefinement::default() }
    }
}

impl Styled for CodeView {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for CodeView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = self.state.read(cx).focus.clone();
        let state = &self.state;
        let mut root = div()
            .id("code-view")
            .key_context(CONTEXT)
            .track_focus(&focus)
            .size_full()
            .overflow_hidden()
            .on_action(window.listener_for(state, CodeViewState::move_left))
            .on_action(window.listener_for(state, CodeViewState::move_right))
            .on_action(window.listener_for(state, CodeViewState::move_up))
            .on_action(window.listener_for(state, CodeViewState::move_down))
            .on_action(window.listener_for(state, CodeViewState::page_up))
            .on_action(window.listener_for(state, CodeViewState::page_down))
            .on_action(window.listener_for(state, CodeViewState::move_home))
            .on_action(window.listener_for(state, CodeViewState::move_end))
            .on_action(window.listener_for(state, CodeViewState::move_to_start))
            .on_action(window.listener_for(state, CodeViewState::move_to_end))
            .on_action(window.listener_for(state, CodeViewState::move_to_previous_word))
            .on_action(window.listener_for(state, CodeViewState::move_to_next_word))
            .on_action(window.listener_for(state, CodeViewState::select_left))
            .on_action(window.listener_for(state, CodeViewState::select_right))
            .on_action(window.listener_for(state, CodeViewState::select_up))
            .on_action(window.listener_for(state, CodeViewState::select_down))
            .on_action(window.listener_for(state, CodeViewState::select_to_start_of_line))
            .on_action(window.listener_for(state, CodeViewState::select_to_end_of_line))
            .on_action(window.listener_for(state, CodeViewState::select_to_start))
            .on_action(window.listener_for(state, CodeViewState::select_to_end))
            .on_action(window.listener_for(state, CodeViewState::select_to_previous_word))
            .on_action(window.listener_for(state, CodeViewState::select_to_next_word))
            .on_action(window.listener_for(state, CodeViewState::select_all))
            .on_action(window.listener_for(state, CodeViewState::copy))
            .on_action(window.listener_for(state, CodeViewState::open_search));
        root.style().refine(&self.style);
        // Like the kit's, the bar sits above the text and pushes it down.
        let bar = self.state.read(cx).search_bar.clone().filter(|_| self.state.read(cx).search_open);
        let scrollbar = self.state.read(cx).scrollbar.clone();
        // The kit's menu for a read-only code editor, with what can't apply greyed out.
        let (copy, select) = (self.state.clone(), self.state.clone());
        let menu = crate::ui::menu::menu(
            &self.state.read(cx).context_menu,
            vec![
                MenuItem::disabled("Go to Definition"),
                MenuItem::disabled("Show Code Actions"),
                MenuItem::separator(),
                MenuItem::disabled("Cut"),
                MenuItem::new("Copy", move |window, cx| copy.update(cx, |s, cx| s.copy(&Copy, window, cx))),
                MenuItem::disabled("Paste"),
                MenuItem::separator(),
                MenuItem::new("Select All", move |window, cx| select.update(cx, |s, cx| s.select_all(&SelectAll, window, cx))),
            ],
            None,
            cx,
        );
        let source = ViewScroll(self.state.clone());
        root.flex().flex_col().children(bar).child(
            div().relative().flex_1().min_h_0().child(CodeElement::new(self.state)).child(Scrollbar::new(&scrollbar, source)),
        )
        .children(menu)
    }
}

/// The viewer's scroll position, for its scrollbar.
struct ViewScroll(gpui::Entity<CodeViewState>);

impl ScrollSource for ViewScroll {
    fn offset(&self, cx: &App) -> Pixels {
        self.0.read(cx).scroll_y
    }

    fn max(&self, cx: &App) -> Pixels {
        self.0.read(cx).max_scroll
    }

    fn scroll_to(&self, y: Pixels, cx: &mut App) {
        self.0.update(cx, |state, cx| {
            state.scroll_to(y, cx);
        });
    }
}
