//! The Lucide icons po uses (ISC license, `assets/icons/LICENSE-LUCIDE`), drawn in the
//! surrounding text color at gpui-kit's icon sizes.
use gpui::{App, IntoElement, ParentElement as _, Rems, RenderOnce, Styled as _, Window, canvas, div, rems};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconName {
    ArrowUp,
    CaseSensitive,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    Close,
    Folder,
    PanelLeftClose,
    PanelLeftOpen,
    PanelRightClose,
    PanelRightOpen,
    Plus,
    Replace,
    Square,
}

impl IconName {
    fn svg(self) -> (&'static str, &'static [u8]) {
        macro_rules! icon {
            ($name:literal) => {
                (concat!("po-icon-", $name), include_bytes!(concat!("../../assets/icons/", $name, ".svg")))
            };
        }
        match self {
            Self::ArrowUp => icon!("arrow-up"),
            Self::CaseSensitive => icon!("case-sensitive"),
            Self::ChevronDown => icon!("chevron-down"),
            Self::ChevronLeft => icon!("chevron-left"),
            Self::ChevronRight => icon!("chevron-right"),
            Self::Close => icon!("close"),
            Self::Folder => icon!("folder"),
            Self::PanelLeftClose => icon!("panel-left-close"),
            Self::PanelLeftOpen => icon!("panel-left-open"),
            Self::PanelRightClose => icon!("panel-right-close"),
            Self::PanelRightOpen => icon!("panel-right-open"),
            Self::Plus => icon!("plus"),
            Self::Replace => icon!("replace"),
            Self::Square => icon!("square"),
        }
    }
}

/// The kit's icon sizes, in rems like its: ¾, ⅞ and 1 of the text size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconSize {
    XSmall,
    Small,
    Medium,
}

impl IconSize {
    pub fn rems(self) -> Rems {
        match self {
            Self::XSmall => rems(0.75),
            Self::Small => rems(0.875),
            Self::Medium => rems(1.),
        }
    }
}

#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    size: IconSize,
    color: Option<gpui::Hsla>,
}

impl Icon {
    pub fn new(name: IconName) -> Self {
        Self { name, size: IconSize::Medium, color: None }
    }

    pub fn size(mut self, size: IconSize) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: gpui::Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (path, svg) = self.name.svg();
        let color = self.color;
        div().flex_none().size(self.size.rems()).child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, cx| {
                    let color = color.unwrap_or_else(|| window.text_style().color);
                    window.paint_svg(bounds, path.into(), Some(svg), gpui::TransformationMatrix::default(), color, cx).ok();
                },
            )
            .size_full(),
        )
    }
}
