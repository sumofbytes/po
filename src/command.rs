//! What can be asked of Po from outside the UI: the one vocabulary agents and scripts use.
//! Nothing here touches GPUI, so a request is parsed and checked without a window.
use serde_json::{Value, json};

/// Who is asking. Decides what a command may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    /// A thread, by the id the window gives it. Only its own views are its to change.
    Thread(u64),
}

/// A document an agent put on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentView {
    pub id: String,
    pub title: String,
    /// Markdown.
    pub body: String,
}

/// The shapes the viewer takes, as the three shortcuts name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Browse,
    Review,
    Chat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Adds a markdown view, or replaces the caller's view with the same `id`.
    ViewShow { id: Option<String>, title: String, body: String },
    ViewUpdate { id: String, title: Option<String>, body: Option<String> },
    ViewClose { id: String },
    LayoutPreset(Preset),
}

/// A command's answer: a JSON value, or why it was refused.
pub type Reply = Result<Value, String>;

/// The most views a thread may keep, and the longest body one may have.
pub const MAX_VIEWS: usize = 16;
pub const MAX_BODY: usize = 256 * 1024;

fn text<'a>(request: &'a Value, key: &str) -> Option<&'a str> {
    request[key].as_str()
}

impl Command {
    /// Reads a request such as `{"command": "view.show", "title": "Plan", "body": "…"}`.
    pub fn parse(request: &Value) -> Result<Self, String> {
        let name = text(request, "command").ok_or("no command")?;
        let need = |key: &str| text(request, key).map(String::from).ok_or_else(|| format!("{name} needs `{key}`"));
        let opt = |key: &str| text(request, key).map(String::from);
        let command = match name {
            "view.show" => Self::ViewShow { id: opt("id"), title: need("title")?, body: need("body")? },
            "view.update" => {
                if opt("title").is_none() && opt("body").is_none() {
                    return Err("view.update needs `title` or `body`".into());
                }
                Self::ViewUpdate { id: need("id")?, title: opt("title"), body: opt("body") }
            }
            "view.close" => Self::ViewClose { id: need("id")? },
            "layout.preset" => Self::LayoutPreset(match need("name")?.as_str() {
                "browse" => Preset::Browse,
                "review" => Preset::Review,
                "chat" => Preset::Chat,
                other => return Err(format!("no preset `{other}`: browse, review or chat")),
            }),
            other => return Err(format!("no command `{other}`")),
        };
        command.check()?;
        Ok(command)
    }

    fn check(&self) -> Result<(), String> {
        let body = match self {
            Self::ViewShow { body, .. } => Some(body),
            Self::ViewUpdate { body, .. } => body.as_ref(),
            _ => None,
        };
        if body.is_some_and(|body| body.len() > MAX_BODY) {
            return Err(format!("a view is at most {} KiB", MAX_BODY / 1024));
        }
        Ok(())
    }
}

/// The views of one thread. Agents have no handle on anyone else's.
#[derive(Default)]
pub struct Views {
    list: Vec<AgentView>,
    next: u64,
}

impl Views {
    // The viewer's tab strip reads these in the change that draws the views.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn list(&self) -> &[AgentView] {
        &self.list
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Applies a view command. Layout presets are the window's, not the views'.
    pub fn apply(&mut self, command: Command) -> Reply {
        match command {
            Command::ViewShow { id, title, body } => {
                let id = id.unwrap_or_else(|| {
                    self.next += 1;
                    format!("v{}", self.next)
                });
                match self.list.iter_mut().find(|view| view.id == id) {
                    Some(view) => {
                        view.title = title;
                        view.body = body;
                    }
                    None => {
                        if self.list.len() >= MAX_VIEWS {
                            return Err(format!("a thread keeps at most {MAX_VIEWS} views; close one first"));
                        }
                        self.list.push(AgentView { id: id.clone(), title, body });
                    }
                }
                Ok(json!({ "id": id }))
            }
            Command::ViewUpdate { id, title, body } => {
                let view = self.list.iter_mut().find(|view| view.id == id).ok_or_else(|| format!("no view `{id}`"))?;
                if let Some(title) = title {
                    view.title = title;
                }
                if let Some(body) = body {
                    view.body = body;
                }
                Ok(json!({ "id": id }))
            }
            Command::ViewClose { id } => {
                let before = self.list.len();
                self.list.retain(|view| view.id != id);
                if self.list.len() == before { Err(format!("no view `{id}`")) } else { Ok(json!({ "id": id })) }
            }
            Command::LayoutPreset(_) => Err("not a view command".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(id: Option<&str>, title: &str, body: &str) -> Command {
        Command::ViewShow { id: id.map(String::from), title: title.into(), body: body.into() }
    }

    #[test]
    fn parses_each_command() {
        let request = json!({ "command": "view.show", "title": "Plan", "body": "# Plan" });
        assert_eq!(Command::parse(&request), Ok(show(None, "Plan", "# Plan")));
        let request = json!({ "command": "view.update", "id": "v1", "body": "x" });
        assert_eq!(Command::parse(&request), Ok(Command::ViewUpdate { id: "v1".into(), title: None, body: Some("x".into()) }));
        assert_eq!(Command::parse(&json!({ "command": "view.close", "id": "v1" })), Ok(Command::ViewClose { id: "v1".into() }));
        assert_eq!(Command::parse(&json!({ "command": "layout.preset", "name": "review" })), Ok(Command::LayoutPreset(Preset::Review)));
    }

    #[test]
    fn refuses_what_it_cannot_read() {
        assert!(Command::parse(&json!({})).is_err());
        assert!(Command::parse(&json!({ "command": "rm" })).is_err());
        assert!(Command::parse(&json!({ "command": "view.show", "title": "t" })).is_err());
        assert!(Command::parse(&json!({ "command": "view.update", "id": "v1" })).is_err());
        assert!(Command::parse(&json!({ "command": "layout.preset", "name": "wide" })).is_err());
        let huge = "x".repeat(MAX_BODY + 1);
        assert!(Command::parse(&json!({ "command": "view.show", "title": "t", "body": huge })).is_err());
    }

    #[test]
    fn views_are_added_replaced_updated_and_closed() {
        let mut views = Views::default();
        assert_eq!(views.apply(show(None, "Plan", "a")), Ok(json!({ "id": "v1" })));
        assert_eq!(views.apply(show(Some("v1"), "Plan 2", "b")), Ok(json!({ "id": "v1" })));
        assert_eq!(views.list().len(), 1);
        assert_eq!(views.list()[0].title, "Plan 2");
        views.apply(Command::ViewUpdate { id: "v1".into(), title: None, body: Some("c".into()) }).unwrap();
        assert_eq!(views.list()[0].body, "c");
        assert!(views.apply(Command::ViewUpdate { id: "nope".into(), title: None, body: Some("c".into()) }).is_err());
        views.apply(Command::ViewClose { id: "v1".into() }).unwrap();
        assert!(views.is_empty());
        assert!(views.apply(Command::ViewClose { id: "v1".into() }).is_err());
    }

    #[test]
    fn a_thread_keeps_a_bounded_number_of_views() {
        let mut views = Views::default();
        for n in 0..MAX_VIEWS {
            views.apply(show(None, &format!("v{n}"), "")).unwrap();
        }
        assert!(views.apply(show(None, "one more", "")).is_err());
        // Replacing one is not adding one.
        assert!(views.apply(show(Some("v1"), "again", "")).is_ok());
    }
}
