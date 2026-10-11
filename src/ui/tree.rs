//! A virtualized tree like gpui-kit's: folders expand and collapse on click or Left/Right,
//! Up/Down move the selection and wrap around, and a scrollbar shows while scrolling.
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _,
    IntoElement, KeyBinding, MouseButton, ParentElement as _, Render, RenderOnce, ScrollStrategy, SharedString,
    Styled, UniformListScrollHandle, Window, div, uniform_list,
};

use super::scrollbar::{Scrollbar, ScrollbarState};

gpui::actions!(tree, [SelectUp, SelectDown, SelectLeft, SelectRight]);

const CONTEXT: &str = "Tree";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectUp, Some(CONTEXT)),
        KeyBinding::new("down", SelectDown, Some(CONTEXT)),
        KeyBinding::new("left", SelectLeft, Some(CONTEXT)),
        KeyBinding::new("right", SelectRight, Some(CONTEXT)),
    ]);
}

/// An item and its children; it's a folder when it has any.
#[derive(Clone)]
pub struct TreeItem {
    pub id: SharedString,
    pub label: SharedString,
    children: Vec<TreeItem>,
    expanded: bool,
}

impl TreeItem {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self { id: id.into(), label: label.into(), children: Vec::new(), expanded: false }
    }

    pub fn children(mut self, children: Vec<TreeItem>) -> Self {
        self.children = children;
        self
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    fn find_mut<'a>(items: &'a mut [TreeItem], id: &str) -> Option<&'a mut TreeItem> {
        for item in items {
            if item.id == id {
                return Some(item);
            }
            if let Some(found) = Self::find_mut(&mut item.children, id) {
                return Some(found);
            }
        }
        None
    }
}

/// The visible rows of `items`: each item, then its children while it's open.
fn flatten(items: &[TreeItem], depth: usize, out: &mut Vec<TreeEntry>) {
    for item in items {
        let folder = !item.children.is_empty();
        out.push(TreeEntry { id: item.id.clone(), label: item.label.clone(), depth, folder, expanded: item.expanded });
        if folder && item.expanded {
            flatten(&item.children, depth + 1, out);
        }
    }
}

/// A visible row: an item at its depth.
pub struct TreeEntry {
    pub id: SharedString,
    pub label: SharedString,
    pub depth: usize,
    pub folder: bool,
    pub expanded: bool,
}

pub enum TreeEvent {
    Expanded(SharedString),
    Collapsed(SharedString),
}

/// Draws a row: its index, entry, whether it's selected and whether it was right-clicked last.
type RenderEntry = dyn Fn(usize, &TreeEntry, bool, bool, &mut Window, &mut App) -> AnyElement;

pub struct TreeState {
    focus: FocusHandle,
    items: Vec<TreeItem>,
    entries: Vec<TreeEntry>,
    selected: Option<usize>,
    right_clicked: Option<usize>,
    scroll: UniformListScrollHandle,
    scrollbar: Entity<ScrollbarState>,
    render: Rc<RenderEntry>,
}

impl EventEmitter<TreeEvent> for TreeState {}

impl TreeState {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            items: Vec::new(),
            entries: Vec::new(),
            selected: None,
            right_clicked: None,
            scroll: UniformListScrollHandle::default(),
            scrollbar: cx.new(|_| ScrollbarState::default()),
            render: Rc::new(|_, _, _, _, _, _| div().into_any_element()),
        }
    }

    /// Replaces the items; nothing is selected afterwards.
    pub fn set_items(&mut self, items: Vec<TreeItem>, cx: &mut Context<Self>) {
        self.items = items;
        self.rebuild();
        self.selected = None;
        self.right_clicked = None;
        cx.notify();
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }

    pub fn set_selected_index(&mut self, ix: Option<usize>, cx: &mut Context<Self>) {
        self.selected = ix;
        cx.notify();
    }

    pub fn selected_entry(&self) -> Option<&TreeEntry> {
        self.entries.get(self.selected?)
    }

    fn rebuild(&mut self) {
        self.entries.clear();
        flatten(&self.items, 0, &mut self.entries);
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(ix).filter(|e| e.folder) else { return };
        let id = entry.id.clone();
        let Some(item) = TreeItem::find_mut(&mut self.items, &id) else { return };
        item.expanded = !item.expanded;
        cx.emit(if item.expanded { TreeEvent::Expanded(id) } else { TreeEvent::Collapsed(id) });
        self.right_clicked = None;
        self.rebuild();
        cx.notify();
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected.unwrap_or(0).checked_sub(1).unwrap_or(self.entries.len().saturating_sub(1));
        self.selected = Some(ix);
        self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        cx.notify();
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected.map_or(0, |ix| ix + 1);
        let ix = if ix < self.entries.len() { ix } else { 0 };
        self.selected = Some(ix);
        self.scroll.scroll_to_item(ix, ScrollStrategy::Bottom);
        cx.notify();
    }

    /// Left collapses an open folder; Right opens a closed one.
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected.filter(|&ix| self.entries.get(ix).is_some_and(|e| e.folder && e.expanded)) {
            self.toggle(ix, cx);
        }
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected.filter(|&ix| self.entries.get(ix).is_some_and(|e| e.folder && !e.expanded)) {
            self.toggle(ix, cx);
        }
    }

    fn click(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = Some(ix);
        self.toggle(ix, cx);
        cx.notify();
    }
}

impl Render for TreeState {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render = self.render.clone();
        let list = uniform_list(
            "entries",
            self.entries.len(),
            cx.processor(move |state, range: Range<usize>, window, cx| {
                range
                    .map(|ix| {
                        let entry = &state.entries[ix];
                        div()
                            .id(ix)
                            .child(render(ix, entry, Some(ix) == state.selected, Some(ix) == state.right_clicked, window, cx))
                            .on_mouse_down(MouseButton::Left, cx.listener(move |state, _, _, cx| state.click(ix, cx)))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |state, _, _, cx| {
                                    state.right_clicked = Some(ix);
                                    cx.notify();
                                }),
                            )
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_grow(1.)
        .size_full();
        let handle = self.scroll.0.borrow().base_handle.clone();
        div().relative().size_full().child(list).child(Scrollbar::new(&self.scrollbar, handle))
    }
}

/// Draws a [`TreeState`], each visible row through `render`.
pub fn tree(
    state: &Entity<TreeState>,
    render: impl Fn(usize, &TreeEntry, bool, bool, &mut Window, &mut App) -> AnyElement + 'static,
) -> Tree {
    Tree { state: state.clone(), render: Rc::new(render) }
}

#[derive(IntoElement)]
pub struct Tree {
    state: Entity<TreeState>,
    render: Rc<RenderEntry>,
}

impl RenderOnce for Tree {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = self.state.read(cx).focus.clone();
        self.state.update(cx, |state, _| state.render = self.render);
        div()
            .id(gpui::ElementId::Name(format!("tree-{}", self.state.entity_id()).into()))
            .key_context(CONTEXT)
            .track_focus(&focus)
            .size_full()
            .on_action(window.listener_for(&self.state, TreeState::select_up))
            .on_action(window.listener_for(&self.state, TreeState::select_down))
            .on_action(window.listener_for(&self.state, TreeState::select_left))
            .on_action(window.listener_for(&self.state, TreeState::select_right))
            .child(self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::{TreeEntry, TreeItem, flatten};

    fn rows(items: &[TreeItem]) -> Vec<(String, usize)> {
        let mut out: Vec<TreeEntry> = Vec::new();
        flatten(items, 0, &mut out);
        out.into_iter().map(|e| (e.id.to_string(), e.depth)).collect()
    }

    #[test]
    fn folders_flatten_when_open() {
        let mut items = vec![
            TreeItem::new("src", "src").expanded(true).children(vec![
                TreeItem::new("src/ui", "ui").children(vec![TreeItem::new("src/ui/a.rs", "a.rs")]),
                TreeItem::new("src/main.rs", "main.rs"),
            ]),
            TreeItem::new("README", "README"),
        ];
        let row = |id: &str, depth| (id.to_string(), depth);
        assert_eq!(rows(&items), [row("src", 0), row("src/ui", 1), row("src/main.rs", 1), row("README", 0)]);
        TreeItem::find_mut(&mut items, "src/ui").unwrap().expanded = true;
        assert_eq!(rows(&items)[2], row("src/ui/a.rs", 2));
        TreeItem::find_mut(&mut items, "src").unwrap().expanded = false;
        assert_eq!(rows(&items), [row("src", 0), row("README", 0)]);
    }
}
