//! The sidebar on Rune: the projects with their threads, what waits on the user, what agents have
//! suggested, and the pages it opens. Counterpart of `src/nsh/sidebar.rs`; it draws a
//! [`Snapshot`] and sends [`Cmd`]s, so it needs nothing of the `Reader`.
//!
//! Not ported yet: renaming a thread from its menu, and the right-click menu.
use rune::{Anim, El, Icon, TextEdit, TextStyle, Tx, col, el, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Sidebar, SidebarRow, SidebarSection, Spinner};

use crate::model::bus::{Cmd, send};
use crate::model::sidebar::{Mark, Project, Snapshot, Thread};

static FOLDER_PLUS: Icon = Icon { name: "po-folder-plus", view: 16.0, body: r#"<path d="M2 4.5C2 3.67 2.67 3 3.5 3H6.2C6.55 3 6.88 3.16 7.1 3.43L8 4.5H12.5C13.33 4.5 14 5.17 14 6V11.5C14 12.33 13.33 13 12.5 13H3.5C2.67 13 2 12.33 2 11.5Z" stroke="currentColor" stroke-linejoin="round"/><path d="M8 7V11M6 9H10" stroke="currentColor" stroke-linecap="round"/>"# };
static CHEVRON_DOWN: Icon = Icon { name: "po-chevron-down", view: 16.0, body: r#"<path d="M4.5 6.5L8 10L11.5 6.5" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/>"# };
static CHEVRON_UP: Icon = Icon { name: "po-chevron-up", view: 16.0, body: r#"<path d="M4.5 9.5L8 6L11.5 9.5" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/>"# };
static HOME: Icon = Icon { name: "po-home", view: 16.0, body: r#"<path d="M2.5 7L8 2.5L13.5 7V13H9.5V9.5H6.5V13H2.5Z" stroke="currentColor" stroke-linejoin="round"/>"# };
static NEW: Icon = Icon { name: "po-new", view: 16.0, body: r#"<path d="M8 3V13M3 8H13" stroke="currentColor" stroke-linecap="round"/>"# };
static ISSUES: Icon = Icon { name: "po-issues", view: 16.0, body: r#"<circle cx="8" cy="8" r="5.5" stroke="currentColor"/><circle cx="8" cy="8" r="1.5" fill="currentColor"/>"# };
static SEARCH: Icon = Icon { name: "po-search", view: 16.0, body: r#"<circle cx="7" cy="7" r="4.5" stroke="currentColor"/><path d="M10.5 10.5L13.5 13.5" stroke="currentColor" stroke-linecap="round"/>"# };
static SETTINGS: Icon = Icon { name: "po-settings", view: 16.0, body: r#"<circle cx="8" cy="8" r="2" stroke="currentColor"/><path d="M8 2V3.5M8 12.5V14M2 8H3.5M12.5 8H14M3.8 3.8L4.9 4.9M11.1 11.1L12.2 12.2M12.2 3.8L11.1 4.9M4.9 11.1L3.8 12.2" stroke="currentColor" stroke-linecap="round"/>"# };

const CAPTION: rune::tok::TextStyle = rune::tok::TextStyle::new(12.0, 400.0);

/// What the sidebar keeps between frames: the filter field.
pub struct State {
    pub query: TextEdit,
}

impl Default for State {
    fn default() -> Self {
        Self { query: TextEdit::new("") }
    }
}

/// Where the host keeps the sidebar's state: handlers reach it through this.
pub type At<S> = fn(&mut S) -> &mut State;

/// A small "+" in a 20 point slot, the width of a row's status slot, so the two line up.
fn plus<S: 'static>(k: &Kit, help: &'static str, on_press: impl Fn(&mut S) + 'static) -> El {
    icon_slot::<S>(k, &NEW, help, on_press)
}

/// A small icon button in that 20 point slot.
fn icon_slot<S: 'static>(k: &Kit, glyph: &'static Icon, help: &'static str, on_press: impl Fn(&mut S) + 'static) -> El {
    let b = IconButton::new(glyph).control_size(ControlSize::Small).variant(IconButtonVariant::GhostMuted).label(help).help(help).on_press(move |s: &mut S| on_press(s)).build(k);
    // The kit's hover wash, kept on at rest so the button reads as one; the hover and press overlays
    // the kit draws go over it, so they still show.
    row().size(20.0, 20.0).center().child(b.size(20.0, 20.0).fill(k.c.hover))
}

/// What a thread's state looks like at the start of its row: a ring when nothing is going on, a
/// dot when it has something to say, and the spinner while its agent works.
fn mark(k: &Kit, mark: Mark) -> El {
    let dot = |color| el().size(8.0, 8.0).radius(4.0).fill(color);
    match mark {
        Mark::Idle => el().size(10.0, 10.0).radius(5.0).border(1.0, k.c.icon_faint),
        Mark::Unread => dot(k.c.bg_accent),
        Mark::Running => Spinner::new().size(14.0).color(k.c.icon_accent).build(k),
        Mark::Asking => dot(k.c.warning.icon),
        Mark::Failed => text("×").font(rune::tok::TextStyle::new(14.0, 500.0)).color(k.c.danger.fg),
    }
}

fn thread_row<S: 'static>(k: &Kit, t: &Thread, subtitle: Option<&str>) -> El {
    let (pid, tid) = (t.project, t.id);
    let mut row = SidebarRow::new(t.label.clone())
        .selected(t.selected)
        .on_select(move |_: &mut S, _: &mut Tx| send(Cmd::OpenThread(pid, tid)))
        .on_close(move |_: &mut S, _: &mut Tx| send(Cmd::CloseThread(pid, tid)));
    row = row.avatar(mark(k, t.mark));
    if let Some(ago) = &t.ago {
        row = row.detail(ago.clone());
    }
    if let Some(project) = subtitle {
        row = row.subtitle(project.to_string());
    } else {
        row = row.height(ROW);
    }
    if t.closing {
        row = row.badge("Close again to lose its work");
    }
    row.build(k).key(format!("thread-{pid}-{tid}"))
}

/// A project: a header that toggles it (larger title, no disclosure glyph, the kit's header
/// hardcodes both) over its thread rows.
fn project_section<S: 'static>(k: &Kit, p: &Project) -> El {
    let pid = p.id;
    let new_thread = plus::<S>(k, "New thread", move |_| send(Cmd::StartThread(pid)));
    let title = text(p.name.clone()).font(TextStyle { weight: 500.0, ..k.ty.ui }).color(k.c.text_base).truncate().shrink();
    let header = row()
        .key("section-header")
        .label(p.name.clone())
        .w_full()
        .h(ROW)
        .px(8.0)
        .center_y()
        .radius(k.radius.md)
        .child(row().key("section-title").center_y().shrink().child(title).a11y_hidden())
        .child(el().key("spacer").grow(1.0).a11y_hidden())
        .child(new_thread)
        .on_press_tx(move |_: &mut S, _: &mut Tx| send(Cmd::ToggleProject(pid)));
    let header = k.interactive(header).role("button").expanded(!p.collapsed).hover_motion(Anim::INSTANT, Anim::INSTANT).focusable(false);
    let mut section = col().key(format!("project-{pid}")).role("group").label(p.name.clone()).w_full().pt(4.0).gap(0.0).motion(k.motion.disclosure).child(header);
    if !p.collapsed {
        for t in &p.threads {
            section = section.child(thread_row::<S>(k, t, None));
        }
        if let Some((expanded, hidden)) = p.fold {
            let label = if expanded { "Show less".to_string() } else { format!("{hidden} more") };
            // As the threads' rows are: a glyph where their marks are, so the words start where the titles do, and quieter than they are.
            let chevron = rune::icon(if expanded { &CHEVRON_UP } else { &CHEVRON_DOWN }, 14.0).color(k.c.icon_muted);
            section = section.child(SidebarRow::new(label).height(MORE_ROW).avatar(chevron).title_style(rune::tok::TextStyle::new(13.0, 400.0)).select_only().on_select(move |_: &mut S, _: &mut Tx| send(Cmd::ToggleMore(pid))).build(k));
        }
    }
    section
}

/// A row of the sidebar, and its "N more" row, as the earlier sidebar had them.
const ROW: f32 = 32.0;
const MORE_ROW: f32 = 28.0;

/// The height of the window's top band, which the shell draws above the sidebar's content.
const BAND: f32 = super::header::BAND;

/// How far in from the sidebar's right edge the toggle in its band sits: as far as the icons at the
/// right of its rows, which the shell lets the rows keep by laying the sidebar out the pane's full width.
pub const GUTTER: f32 = 20.0;

/// The sidebar for `snapshot`, with its filter in the host's state through `at`.
pub fn view<S: 'static>(snapshot: &Snapshot, state: &State, k: &Kit, at: At<S>, settings_open: bool) -> El {
    // The kit's field hardcodes a faint placeholder; the rows' own ink is what this one wants.
    let field = rune::text_input(&state.query, k.ty.ui_compact)
        .single_line()
        .key("input")
        .grow(1.0)
        .shrink()
        .color(k.c.text_base)
        .placeholder("Search", k.c.text_muted)
        .caret(k.c.text_base, k.selection())
        .label("Search threads")
        .on_edit(move |s: &mut S| &mut at(s).query)
        .on_input_change(move |s: &mut S, _: &mut Tx| send(Cmd::SideQuery(at(s).query.text.clone())));
    let search = row().w_full().h(ROW).px(7.0).gap(5.0).items_center().child(el().child(rune::icon(&SEARCH, 16.0).color(k.c.icon_muted)).a11y_hidden()).child(field);

    let issues_detail = match snapshot.issues {
        (0, _) => String::new(),
        (open, 0) => open.to_string(),
        (open, late) => format!("{open} · {late} late"),
    };
    let pages = col()
        .w_full()
        .child(SidebarRow::new("Home").height(ROW).icon(&HOME).selected(snapshot.home_open && !settings_open).on_select(|_: &mut S, _: &mut Tx| send(Cmd::ToggleHome)).build(k).key("home"))
        .child(SidebarRow::new("New").height(ROW).icon(&NEW).on_select(|_: &mut S, _: &mut Tx| send(Cmd::NewThread)).build(k).key("new-thread"))
        .child(col().w_full().py(4.0).child(search))
        .child(SidebarRow::new("Issues").height(ROW).icon(&ISSUES).selected(snapshot.issues_open && !settings_open).detail(issues_detail).on_select(|_: &mut S, _: &mut Tx| send(Cmd::ToggleIssues)).build(k).key("issues"));

    let mut bar = Sidebar::new().label("Projects").inset(12.0).header_gap(4.0).header_padding(12.0, 8.0).row_gap(4.0).header(pages);

    if !snapshot.suggestions.is_empty() {
        let mut list = col().w_full().gap(4.0).py(4.0);
        for s in &snapshot.suggestions {
            let id = s.id;
            list = list.child(
                col()
                    .w_full()
                    .gap(6.0)
                    .child(text(s.text.clone()).font(CAPTION).color(k.c.text_base))
                    .child(
                        row()
                            .gap(6.0)
                            .child(Button::new(s.yes).variant(ButtonVariant::Submit).on_press(move |_: &mut S| send(Cmd::Answer(id, true))).build(k))
                            .child(Button::new("Not now").on_press(move |_: &mut S| send(Cmd::Answer(id, false))).build(k)),
                    ),
            );
        }
        bar = bar.child(list);
    }

    if !snapshot.waiting.is_empty() {
        let mut needs = SidebarSection::new("Needs you").key("needs-you").count(snapshot.waiting.len());
        for w in &snapshot.waiting {
            needs = needs.row(thread_row::<S>(k, &w.thread, Some(&w.project)));
        }
        bar = bar.section(needs);
    }
    for p in &snapshot.projects {
        bar = bar.child(project_section::<S>(k, p));
    }

    let mut footer = vec![];
    if let Some(status) = &snapshot.status {
        footer.push(
            row()
                .w_full()
                .items_center()
                .gap(6.0)
                .child(row().grow(1.0).min_w(0.0).child(text(status.clone()).font(CAPTION).color(k.c.text_muted)))
                .child(Button::new("Dismiss").variant(ButtonVariant::Ghost).on_press(|_: &mut S| send(Cmd::DismissStatus)).build(k)),
        );
    }
    footer.push(SidebarRow::new("Add project").height(ROW).icon(&FOLDER_PLUS).on_select(|_: &mut S, _: &mut Tx| send(Cmd::AddProject)).build(k).key("add-project"));
    footer.push(SidebarRow::new("Settings").height(ROW).icon(&SETTINGS).on_select(|_: &mut S, _: &mut Tx| send(Cmd::OpenSettings)).build(k).key("settings"));
    for f in footer {
        bar = bar.footer(f);
    }
    // "Po" sits in the window's top band, over the strip the shell keeps empty above the sidebar;
    // the shell draws the rule under the band.
    let band = row().absolute(0.0, -BAND).w_full().h(BAND).px(20.0).items_center().child(text("Po").font(rune::tok::TextStyle::new(18.0, 600.0)).color(k.c.text_base));
    // The divider beside the sidebar, band and all, at the pane's right edge.
    let edge = rune::el().pin(None, Some(-BAND), Some(0.0), Some(0.0)).w(1.0).fill(k.c.border_base);
    col().w_full().h_full().min_h(0.0).child(band).child(edge).child(bar.build(k))
}
