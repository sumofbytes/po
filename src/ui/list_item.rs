//! A row in a list or tree, styled like gpui-kit's `ListItem`: padded, highlighted on hover,
//! filled when selected, outlined when right-clicked.
use gpui::{
    AnyElement, App, ClickEvent, ElementId, Refineable, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement as _, StyleRefinement, Styled, Window, div, prelude::FluentBuilder as _,
};

use crate::theme::Palette;

type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct ListItem {
    id: ElementId,
    style: StyleRefinement,
    selected: bool,
    secondary_selected: bool,
    on_click: Option<OnClick>,
    children: Vec<AnyElement>,
}

impl ListItem {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            selected: false,
            secondary_selected: false,
            on_click: None,
            children: Vec::new(),
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Outlined, as the row a context menu was opened on.
    pub fn secondary_selected(mut self, selected: bool) -> Self {
        self.secondary_selected = selected;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl Styled for ListItem {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for ListItem {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ListItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.global::<Palette>();
        let active = self.selected || self.secondary_selected;
        let (hover, selected, outline) = (palette.list_hover, palette.list_selected, palette.selection);
        let mut row = div()
            .id(self.id)
            .flex()
            .relative()
            .gap_x_1()
            .py_1p5()
            .px_3()
            .text_base()
            .text_color(palette.foreground)
            .items_center()
            .justify_between();
        row.style().refine(&self.style);
        row.when_some(self.on_click, |this, on_click| this.on_click(on_click))
            .hover(move |this| if active { this } else { this.bg(hover) })
            .child(div().flex().w_full().items_center().justify_between().gap_x_1().child(div().w_full().children(self.children)))
            .when(self.selected, |this| this.bg(selected))
            .when(self.secondary_selected, |this| {
                this.child(div().absolute().inset_0().border_1().border_color(outline))
            })
    }
}
