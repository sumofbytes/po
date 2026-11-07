//! What the screens and the [`crate::Reader`] exchange, with no view code: each screen's snapshot
//! and the commands it sends back. Both UIs (`nsh` and `rune_ui`) draw from these.
pub mod bus;
pub mod chat;
pub mod header;
pub mod agent;
pub mod diff;
pub mod explorer;
pub mod sidebar;
pub mod source;
pub mod finder;
pub mod tabs;
pub mod strip;
