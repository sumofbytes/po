//! The Lucide icons po uses (ISC license, `assets/icons/LICENSE-LUCIDE`), drawn by NSH.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconName {
    ArrowUp,
    Calendar,
    CaseSensitive,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    Close,
    CornerDownRight,
    FileText,
    Folder,
    FolderTree,
    Heading,
    ListPlus,
    Maximize,
    Minimize,
    PanelLeft,
    Pencil,
    Plus,
    Replace,
    Search,
    Settings,
    Square,
    SquareCheck,
}

impl IconName {
    pub(crate) fn svg(self) -> (&'static str, &'static [u8]) {
        macro_rules! icon {
            ($name:literal) => {
                (concat!("po-icon-", $name), include_bytes!(concat!("../../assets/icons/", $name, ".svg")))
            };
        }
        match self {
            Self::ArrowUp => icon!("arrow-up"),
            Self::Calendar => icon!("calendar"),
            Self::CaseSensitive => icon!("case-sensitive"),
            Self::ChevronDown => icon!("chevron-down"),
            Self::ChevronLeft => icon!("chevron-left"),
            Self::ChevronRight => icon!("chevron-right"),
            Self::Close => icon!("close"),
            Self::CornerDownRight => icon!("corner-down-right"),
            Self::FileText => icon!("file-text"),
            Self::Folder => icon!("folder"),
            Self::FolderTree => icon!("folder-tree"),
            Self::Heading => icon!("heading"),
            Self::ListPlus => icon!("list-plus"),
            Self::Maximize => icon!("maximize-2"),
            Self::Minimize => icon!("minimize-2"),
            Self::PanelLeft => icon!("panel-left"),
            Self::Pencil => icon!("pencil"),
            Self::Plus => icon!("plus"),
            Self::Replace => icon!("replace"),
            Self::Search => icon!("search"),
            Self::Settings => icon!("settings"),
            Self::Square => icon!("square"),
            Self::SquareCheck => icon!("square-check"),
        }
    }
}
