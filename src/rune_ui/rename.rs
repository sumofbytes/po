//! A thread's name typed in place: a double press on it, in the sidebar or the conversation's
//! title, swaps the name for a line, there only. Enter or leaving the line keeps what was typed;
//! Escape leaves it as it was.
use rune::tok::TextStyle;
use rune::{El, Key, KeyEvent, TextEdit, Tx, row, text};
use rune_kit::Kit;

use crate::model::bus::{Cmd, send};

/// Where a name is being typed. A thread on screen is named in both the sidebar and the title,
/// but only one of them may hold the line: two fields on one [`TextEdit`] each write whether they
/// have the keyboard to it, and the one without has the last word.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Place {
    #[default]
    Sidebar,
    Title,
}

#[derive(Default)]
pub struct Rename {
    /// The `(project, thread)` being named.
    pub editing: Option<(u64, u64)>,
    place: Place,
    pub edit: TextEdit,
    /// The name it started from, so a name left as it was isn't taken as a new one.
    from: String,
}

impl Rename {
    pub fn start(&mut self, thread: (u64, u64), name: &str, place: Place) {
        self.edit = TextEdit::new(name);
        self.edit.anchor = 0;
        self.edit.focus = self.edit.text.len();
        self.edit.focused = true;
        self.from = name.to_string();
        self.place = place;
        self.editing = Some(thread);
    }

    /// Whether the line for `thread` belongs at `place`.
    pub fn open_at(&self, thread: (u64, u64), place: Place) -> bool {
        self.editing == Some(thread) && self.place == place
    }

    /// Ends the edit and asks for the new name if it differs. Ending twice asks once.
    pub fn commit(&mut self) {
        let Some((project, thread)) = self.editing.take() else {
            return;
        };
        if self.edit.text != self.from {
            send(Cmd::RenameThread(project, thread, self.edit.text.clone()));
        }
    }

    pub fn cancel(&mut self) {
        self.editing = None;
    }

    /// Keeps the name when the line has lost the keyboard; true when it did.
    pub fn settle(&mut self) -> bool {
        if self.editing.is_none() || self.edit.focused {
            return false;
        }
        self.commit();
        true
    }

    /// Keeps the name when the line has gone from the window, which takes no keyboard from it:
    /// `sidebar` says whether the sidebar is up, `title` whose title is on screen.
    pub fn follow(&mut self, sidebar: bool, title: Option<(u64, u64)>) {
        let shown = match self.place {
            Place::Sidebar => sidebar,
            Place::Title => title == self.editing,
        };
        if !shown {
            self.commit();
        }
    }
}

/// Where the host keeps the rename: handlers reach it through this.
pub type At<S> = fn(&mut S) -> &mut Rename;

/// The line the name is typed in.
pub fn input<S: 'static>(k: &Kit, state: &Rename, at: At<S>, font: TextStyle) -> El {
    rune::text_input(&state.edit, font)
        .single_line()
        .key("rename")
        .grow(1.0)
        .shrink()
        .color(k.c.text_base)
        .placeholder("Name", k.c.text_muted)
        .caret(k.c.text_base, k.selection())
        .label("Thread name")
        .on_edit(move |s: &mut S| &mut at(s).edit)
        .on_submit(move |s: &mut S, _: &mut Tx| at(s).commit())
        .on_input_key(move |s: &mut S, event: &KeyEvent, _: &mut Tx| {
            if event.key == Key::Escape {
                at(s).cancel();
                return true;
            }
            false
        })
}

/// The line for `thread`'s title, if that is where the name is being typed. It fills what it is
/// put in, so the toolbar seats it across the band, not in its title slot, which is only as wide
/// as the name was.
pub fn line<S: 'static>(
    k: &Kit,
    state: &Rename,
    at: At<S>,
    thread: (u64, u64),
    font: TextStyle,
) -> Option<El> {
    state.open_at(thread, Place::Title).then(|| {
        row()
            .w_full()
            .items_center()
            .child(input::<S>(k, state, at, font))
    })
}

/// A thread's title in a header: the name, which a double press turns into the line while
/// `thread` is the one being renamed.
pub fn title<S: 'static>(
    k: &Kit,
    state: &Rename,
    at: At<S>,
    thread: (u64, u64),
    name: &str,
    font: TextStyle,
) -> El {
    if let Some(line) = line::<S>(k, state, at, thread, font) {
        return row().grow(1.0).shrink().items_center().child(line);
    }
    let shown = name.to_string();
    let label = name.to_string();
    let body = text(label).font(font).color(k.c.text_base).truncate();
    row()
        .key("thread-title")
        .grow(1.0)
        .shrink()
        .child(body)
        .on_double_press(move |s: &mut S| at(s).start(thread, &shown, Place::Title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Cx, Env, Runtime};

    struct App {
        rename: Rename,
    }

    fn view(s: &App, _: &mut Cx<App>) -> El {
        let kit = Kit::neutral(true);
        row().w_full().h(44.0).child(title::<App>(
            &kit,
            &s.rename,
            |s| &mut s.rename,
            (1, 2),
            "A name",
            TextStyle::new(14.0, 400.0),
        ))
    }

    #[test]
    fn a_double_press_on_the_title_keeps_the_line_open() {
        let mut rt = Runtime::new(
            App {
                rename: Rename::default(),
            },
            view,
            (400.0, 44.0),
            Env::new(true, 1.0),
        );
        rt.frame(0.0);
        let (x, y) = rt.find("thread-title").unwrap();
        for _ in 0..2 {
            rt.pointer_down(x - 5.0, y);
            rt.pointer_up(x - 5.0, y);
            rt.frame(1.0 / 60.0);
        }
        assert!(rt.state.rename.editing.is_some(), "the line opened");
        for _ in 0..5 {
            rt.update(|s, _| {
                s.rename.settle();
            });
            rt.frame(1.0 / 60.0);
        }
        assert!(rt.state.rename.editing.is_some(), "and stayed");
        assert!(rt.state.rename.edit.focused, "with the keyboard");
    }

    #[test]
    fn the_line_is_only_where_it_was_opened() {
        let mut rename = Rename::default();
        rename.start((1, 2), "A name", Place::Sidebar);
        assert!(rename.open_at((1, 2), Place::Sidebar));
        assert!(!rename.open_at((1, 2), Place::Title));
        assert!(!rename.open_at((1, 3), Place::Sidebar));
    }

    #[test]
    fn a_line_that_has_gone_from_the_window_ends() {
        let mut rename = Rename::default();
        rename.start((1, 2), "A name", Place::Title);
        rename.follow(false, Some((1, 2)));
        assert!(rename.editing.is_some(), "the title is still up");
        rename.follow(true, Some((1, 3)));
        assert!(
            rename.editing.is_none(),
            "another thread's title took its place"
        );

        rename.start((1, 2), "A name", Place::Sidebar);
        rename.follow(true, None);
        assert!(rename.editing.is_some(), "the sidebar is still up");
        rename.follow(false, None);
        assert!(rename.editing.is_none(), "the sidebar was put away");
    }
}
