//! po's own components, drawn directly with gpui.
pub mod button;
pub mod file_tree;
pub mod icon;
pub mod input;
pub mod list_item;
pub mod markdown;
pub mod menu;
pub mod resizable;
pub mod scrollbar;
pub mod tree;

pub fn init(cx: &mut gpui::App) {
    tree::init(cx);
    menu::init(cx);
    markdown::init(cx);
    input::init(cx);
}
