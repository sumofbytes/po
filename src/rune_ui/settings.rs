//! The settings page on Rune: the options of `config.json` as fields, written back on Save. What
//! the fields hold and how they are written is [`crate::settings`]; this is the screen, and the
//! Rune counterpart of `src/nsh/settings.rs`.
use rune::tok::{TextStyle, type_};
use rune::{El, TextEdit, Tx, col, el, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, SegmentedControl, Switch, TextInput};

use super::theme_pick;
use crate::settings::{AgentForm, Form};

/// Where the host keeps the page: handlers reach it through this.
pub type At<S> = fn(&mut S) -> &mut Page;

/// What the host is asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Saved to `config.json`; the host applies it.
    Saved,
    Close,
}

/// An agent's fields as the page edits them.
pub struct AgentRow {
    pub name: TextEdit,
    pub id: TextEdit,
    pub command: TextEdit,
    /// Keys the page has no field for, kept as they were.
    pub extra: serde_json::Map<String, serde_json::Value>,
}

pub struct Page {
    pub theme: TextEdit,
    /// The theme list: whether it is open, what is typed in it.
    pub pick: theme_pick::State,
    pub font_family: TextEdit,
    pub font_size: TextEdit,
    pub mono_family: TextEdit,
    pub mono_size: TextEdit,
    pub idle_minutes: TextEdit,
    pub max_live: TextEdit,
    pub notifications: bool,
    pub glass: bool,
    pub viewer_left: bool,
    pub agents: Vec<AgentRow>,
    pub error: Option<String>,
    /// A theme just picked, for the host to show at once, before anything is saved.
    pub preview: Option<String>,
    /// Set by Save and Close, for the host to take.
    pub outcome: Option<Outcome>,
}

impl Page {
    /// The page as `config.json` has it now; `viewer_left` is where the viewer is.
    pub fn load(viewer_left: bool) -> Self {
        let form = Form::load(viewer_left);
        let edit = |text: &str| TextEdit::new(text);
        Self {
            theme: edit(&form.theme),
            pick: Default::default(),
            font_family: edit(&form.font_family),
            font_size: edit(&form.font_size),
            mono_family: edit(&form.mono_family),
            mono_size: edit(&form.mono_size),
            idle_minutes: edit(&form.idle_minutes),
            max_live: edit(&form.max_live),
            notifications: form.notifications,
            glass: form.glass,
            viewer_left: form.viewer_left,
            agents: form.agents.into_iter().map(|a| AgentRow { name: edit(&a.name), id: edit(&a.id), command: edit(&a.command), extra: a.extra }).collect(),
            error: None,
            preview: None,
            outcome: None,
        }
    }

    fn form(&self) -> Form {
        Form {
            theme: self.theme.text.clone(),
            font_family: self.font_family.text.clone(),
            font_size: self.font_size.text.clone(),
            mono_family: self.mono_family.text.clone(),
            mono_size: self.mono_size.text.clone(),
            idle_minutes: self.idle_minutes.text.clone(),
            max_live: self.max_live.text.clone(),
            notifications: self.notifications,
            glass: self.glass,
            viewer_left: self.viewer_left,
            agents: self.agents.iter().map(|a| AgentForm { id: a.id.text.clone(), name: a.name.text.clone(), command: a.command.text.clone(), extra: a.extra.clone() }).collect(),
        }
    }

    /// Writes the fields to `config.json`, or keeps what is wrong with them for the page to show.
    pub fn save(&mut self) {
        match self.form().save() {
            Ok(()) => {
                self.error = None;
                self.outcome = Some(Outcome::Saved);
            }
            Err(message) => self.error = Some(message),
        }
    }

    fn add_agent(&mut self) {
        self.agents.push(AgentRow { name: TextEdit::new(""), id: TextEdit::new(""), command: TextEdit::new(""), extra: Default::default() });
    }
}

const TITLE: TextStyle = TextStyle::new(18.0, 600.0);
const BODY: TextStyle = TextStyle::new(13.0, 400.0);
const CAPTION: TextStyle = TextStyle::new(12.0, 400.0);
const EYEBROW: TextStyle = TextStyle::new(11.0, 500.0);
const PAD: f32 = 32.0;
const CONTROL_WIDTH: f32 = 260.0;

fn section(k: &Kit, title: &str) -> El {
    col().pt(32.0).pb(4.0).child(text(title.to_uppercase()).font(EYEBROW).color(k.c.text_muted))
}

fn divider(k: &Kit) -> El {
    el().h(1.0).w_full().fill(k.c.border_muted)
}

/// A label and its hint on the left and its control on the right.
fn setting(k: &Kit, label: &str, hint: &str, control: El) -> El {
    row()
        .w_full()
        .items_center()
        .gap(24.0)
        .py(12.0)
        .child(col().grow(1.0).min_w(0.0).gap(2.0).child(text(label).font(BODY).color(k.c.text_base)).child(text(hint).font(CAPTION).color(k.c.text_muted)))
        .child(row().w(CONTROL_WIDTH).justify_end().child(control))
}

fn input<S: 'static>(k: &Kit, label: &str, placeholder: &str, edit: &TextEdit, at: fn(&mut Page) -> &mut TextEdit, page: At<S>) -> El {
    TextInput::new(edit).label(label).placeholder(placeholder).width(Some(CONTROL_WIDTH)).on_edit(move |s: &mut S| at(page(s))).build(k)
}

fn switch<S: 'static>(k: &Kit, label: &str, on: bool, flip: fn(&mut Page), page: At<S>) -> El {
    Switch::new(on).label(label).hide_label().on_toggle(move |s: &mut S| flip(page(s))).build(k)
}

/// The page, with its state reached through `page`.
pub fn view<S: 'static>(p: &Page, k: &Kit, page: At<S>, layer: &mut Vec<El>) -> El {
    let theme = setting(k, "Theme", "A colour scheme", theme_pick::view::<S>(k, &p.pick, p.theme.text.trim(), CONTROL_WIDTH, std::rc::Rc::new(move |s: &mut S| &mut page(s).pick), std::rc::Rc::new(move |s: &mut S, name: &str| { let page = page(s); page.theme = TextEdit::new(name); page.preview = Some(name.to_string()); }), layer));

    let appearance = col()
        .w_full()
        .child(theme)
        .child(divider(k))
        .child(setting(k, "Glass", "Translucent panes over the desktop", switch(k, "Glass", p.glass, |p| p.glass = !p.glass, page)))
        .child(divider(k))
        .child(setting(k, "UI font", "Names tried in order, comma separated", input(k, "UI font", crate::fonts::BUNDLED_UI_FAMILY, &p.font_family, |p| &mut p.font_family, page)))
        .child(divider(k))
        .child(setting(k, "UI font size", "Also scales spacing", input(k, "UI font size", "14", &p.font_size, |p| &mut p.font_size, page)))
        .child(divider(k))
        .child(setting(k, "Monospace font", "Names tried in order, comma separated", input(k, "Monospace font", crate::fonts::BUNDLED_MONO_FAMILY, &p.mono_family, |p| &mut p.mono_family, page)))
        .child(divider(k))
        .child(setting(k, "Monospace font size", "Code and diffs", input(k, "Monospace font size", "13", &p.mono_size, |p| &mut p.mono_size, page)));

    let picked = SegmentedControl::new(&["Left", "Right"], Some(if p.viewer_left { 0 } else { 1 }))
        .label("Viewer position")
        .on_pick(move |s: &mut S, i: usize, _: &mut Tx| page(s).viewer_left = i == 0)
        .build(k);
    let layout = setting(k, "Viewer position", "Which side of the conversation (Ctrl+Shift+S)", picked);

    let behavior = col()
        .w_full()
        .child(setting(k, "Notifications", "When a thread needs you in the background", switch(k, "Notifications", p.notifications, |p| p.notifications = !p.notifications, page)))
        .child(divider(k))
        .child(setting(k, "Idle minutes", "Stop an idle agent after this long; 0 never", input(k, "Idle minutes", "15", &p.idle_minutes, |p| &mut p.idle_minutes, page)))
        .child(divider(k))
        .child(setting(k, "Max live agents", "Longest idle agents stop beyond this many", input(k, "Max live agents", "8", &p.max_live, |p| &mut p.max_live, page)));

    let mut agents = col().w_full().gap(8.0);
    for (i, a) in p.agents.iter().enumerate() {
        let field = |label: &'static str, placeholder: &str, edit: &TextEdit, pick: fn(&mut AgentRow) -> &mut TextEdit, grow: f32| {
            row().grow(grow).min_w(0.0).child(TextInput::new(edit).label(label).placeholder(placeholder).width(None).on_edit(move |s: &mut S| pick(&mut page(s).agents[i])).build(k))
        };
        agents = agents.child(
            row()
                .w_full()
                .items_center()
                .gap(8.0)
                .child(field("Name", "Name", &a.name, |a| &mut a.name, 2.0))
                .child(field("ID", "ID", &a.id, |a| &mut a.id, 2.0))
                .child(field("Command", "command --acp", &a.command, |a| &mut a.command, 5.0))
                .child(Button::new("Remove").variant(ButtonVariant::Ghost).on_press(move |s: &mut S| drop(page(s).agents.remove(i))).build(k)),
        );
    }
    let add = Button::new("Add agent").on_press(move |s: &mut S| page(s).add_agent()).build(k);

    let close = IconButton::new(k.glyph.close).control_size(ControlSize::Regular).variant(IconButtonVariant::GhostMuted).label("Close settings").help("Close (Esc)").on_press(move |s: &mut S| page(s).outcome = Some(Outcome::Close)).build(k);
    let header = row()
        .px(PAD)
        .pt(24.0)
        .pb(16.0)
        .gap(8.0)
        .items_start()
        .child(col().grow(1.0).min_w(0.0).gap(2.0).child(text("Settings").font(TITLE).color(k.c.text_base)).child(text("How Po looks and behaves").font(CAPTION).color(k.c.text_muted)))
        .child(close);

    let body = col()
        .px(PAD)
        .pb(24.0)
        .child(section(k, "Appearance"))
        .child(appearance)
        .child(section(k, "Layout"))
        .child(layout)
        .child(section(k, "Behavior"))
        .child(behavior)
        .child(section(k, "Agents"))
        .child(text("Added to the bundled agents; the same id replaces one. A new command asks before it first runs.").font(CAPTION).color(k.c.text_muted).pb(8.0))
        .child(agents)
        .child(row().pt(8.0).child(add));

    let status = match &p.error {
        Some(message) => text(message.clone()).font(CAPTION).color(k.c.danger.fg),
        None => text("Saved to config.json; other keys in it are kept. MCP servers are edited there.").font(CAPTION).color(k.c.text_muted),
    };
    let footer = row()
        .px(PAD)
        .py(12.0)
        .items_center()
        .gap(8.0)
        .child(row().grow(1.0).min_w(0.0).child(status))
        .child(Button::new("Close").on_press(move |s: &mut S| page(s).outcome = Some(Outcome::Close)).build(k))
        .child(Button::new("Save").variant(ButtonVariant::Submit).on_press(move |s: &mut S| page(s).save()).build(k));

    col()
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(header)
        .child(divider(k))
        .child(col().grow(1.0).min_h(0.0).child(k.scroll_bar(col().grow(1.0).min_h(0.0).scroll(rune::ScrollSpec { stick_bottom: false }).child(body))))
        .child(divider(k))
        .child(footer)
}

#[allow(dead_code)]
const _: TextStyle = type_::BODY;
