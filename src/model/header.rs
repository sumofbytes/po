//! The window's own controls, which sit in the headers of the panes at its edges.

/// One of the window's controls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
    /// Shows or hides the sidebar; selected while it shows.
    Sidebar(bool),
    /// Shows or hides the viewer, which is on the right.
    Viewer(bool),
    /// The same, with the viewer on the left of the conversation.
    ViewerLeft(bool),
    /// The files, the changes and the terminal, selected while the viewer shows them.
    Files(bool),
    Changes(bool),
    Terminal(bool),
    /// Gives the viewer the room by hiding the conversation (or gives it back).
    Focus(bool),
}

/// What the shell adds to a screen's snapshot: the window's theme and the controls its header
/// carries. Decided by the layout, so the core never builds it.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Chrome {
    pub lead: Vec<Control>,
    pub trail: Vec<Control>,
}
