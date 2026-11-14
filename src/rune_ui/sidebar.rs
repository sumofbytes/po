//! The sidebar on Rune: the projects with their threads, what waits on the user, what agents have
//! suggested, and the pages it opens. Counterpart of `src/nsh/sidebar.rs`; it draws a
//! [`Snapshot`] and sends [`Cmd`]s, so it needs nothing of the `Reader`.
//!
//! Not ported yet: renaming a thread from its menu, and the right-click menu.
use std::rc::Rc;

use rune::{Anim, El, Icon, Phase, TextEdit, TextStyle, Tx, col, el, row, text};
use rune_kit::shell::RowStatus;
use rune_kit::{
    Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Sidebar, SidebarRow,
    SidebarSection, Spinner,
};

use super::dropdown::{self, Line};
use crate::model::bus::{Cmd, send};
use crate::model::sidebar::{Age, Layout, Mark, Project, Snapshot, Thread};

static FOLDER_PLUS: Icon = Icon {
    name: "po-folder-plus",
    view: 16.0,
    body: r#"<path d="M2 4.5C2 3.67 2.67 3 3.5 3H6.2C6.55 3 6.88 3.16 7.1 3.43L8 4.5H12.5C13.33 4.5 14 5.17 14 6V11.5C14 12.33 13.33 13 12.5 13H3.5C2.67 13 2 12.33 2 11.5Z" stroke="currentColor" stroke-linejoin="round"/><path d="M8 7V11M6 9H10" stroke="currentColor" stroke-linecap="round"/>"#,
};
static CHEVRON_DOWN: Icon = Icon {
    name: "po-chevron-down",
    view: 16.0,
    body: r#"<path d="M4.5 6.5L8 10L11.5 6.5" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
static CHEVRON_UP: Icon = Icon {
    name: "po-chevron-up",
    view: 16.0,
    body: r#"<path d="M4.5 9.5L8 6L11.5 9.5" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
static HOME: Icon = Icon {
    name: "po-home",
    view: 16.0,
    body: r#"<path d="M2.5 7L8 2.5L13.5 7V13H9.5V9.5H6.5V13H2.5Z" stroke="currentColor" stroke-linejoin="round"/>"#,
};
static NEW: Icon = Icon {
    name: "po-new",
    view: 16.0,
    body: r#"<path d="M8 3V13M3 8H13" stroke="currentColor" stroke-linecap="round"/>"#,
};
static ISSUES: Icon = Icon {
    name: "po-issues",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="5.5" stroke="currentColor"/><circle cx="8" cy="8" r="1.5" fill="currentColor"/>"#,
};
static SEARCH: Icon = Icon {
    name: "po-search",
    view: 16.0,
    body: r#"<circle cx="7" cy="7" r="4.5" stroke="currentColor"/><path d="M10.5 10.5L13.5 13.5" stroke="currentColor" stroke-linecap="round"/>"#,
};
static SETTINGS: Icon = Icon {
    name: "po-settings",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="2" stroke="currentColor"/><path d="M8 2V3.5M8 12.5V14M2 8H3.5M12.5 8H14M3.8 3.8L4.9 4.9M11.1 11.1L12.2 12.2M12.2 3.8L11.1 4.9M4.9 11.1L3.8 12.2" stroke="currentColor" stroke-linecap="round"/>"#,
};

/// The tallest the pinned "Needs you" section grows before it scrolls on its own.
const NEEDS_MAX: f32 = 220.0;

fn caption_style() -> rune::tok::TextStyle {
    crate::config::fonts::ui(12.0, 400.0)
}

/// What the sidebar keeps between frames: the filter field.
pub struct State {
    pub query: TextEdit,
    /// A close the user pressed and has not yet confirmed in the dialog.
    pub confirm: Option<Confirm>,
    /// A thread's name being typed.
    pub rename: super::rename::Rename,
    /// A project's header being dragged to a new place in the list.
    pub drag: Option<ProjectDrag>,
    /// The layout and project menus under the search field.
    pub menu: dropdown::State,
}

/// A project dragged by its header: its place in the list, where it would land if dropped now,
/// and how far the pointer has travelled since the press.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectDrag {
    pub from: usize,
    pub to: usize,
    pub dy: f32,
}

/// Points the pointer travels before a press on a project's header becomes a drag.
const SLOP: f32 = 4.0;
/// The space the sidebar leaves between sections.
const SECTION_GAP: f32 = 4.0;

/// How tall a project's section is: its top padding, header, thread rows and fold row.
fn section_height(p: &Project) -> f32 {
    let threads = if p.collapsed {
        0.0
    } else {
        p.threads.len() as f32 * ROW + p.fold.map_or(0.0, |_| MORE_ROW)
    };
    4.0 + ROW + threads
}

/// Where the section `from` would land if dropped after the pointer travelled `dy`: the index of
/// the section whose place it takes, once its middle passes that section's middle.
fn drop_index(heights: &[f32], from: usize, dy: f32) -> usize {
    let mut tops = Vec::with_capacity(heights.len());
    let mut y = 0.0;
    for h in heights {
        tops.push(y);
        y += h + SECTION_GAP;
    }
    let middle = |i: usize| tops[i] + heights[i] / 2.0;
    let at = middle(from) + dy;
    let mut to = from;
    for j in from + 1..heights.len() {
        if at > middle(j) {
            to = j;
        }
    }
    for j in (0..from).rev() {
        if at < middle(j) {
            to = j;
        }
    }
    to
}

/// How far the section `index` sits from its place while `drag` is on: the dragged one with the
/// pointer, and those it passes a step toward the gap it left.
fn shift(drag: &ProjectDrag, index: usize, heights: &[f32]) -> f32 {
    let step = heights[drag.from] + SECTION_GAP;
    if index == drag.from {
        drag.dy
    } else if drag.from < index && index <= drag.to {
        -step
    } else if drag.to <= index && index < drag.from {
        step
    } else {
        0.0
    }
}

/// What the confirmation dialog asks about.
#[derive(Clone, Debug)]
pub struct Confirm {
    /// The thread to close, within the project; none when the whole project is.
    pub thread: Option<u64>,
    pub project: u64,
    pub name: String,
    /// Closing it would lose work: a running turn, a question, queued messages.
    pub loses_work: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            query: TextEdit::new(""),
            confirm: None,
            rename: Default::default(),
            drag: None,
            menu: Default::default(),
        }
    }
}

/// Where the host keeps the sidebar's state: handlers reach it through this.
pub type At<S> = fn(&mut S) -> &mut State;

/// A small "+" in a 20 point slot, the width of a row's status slot, so the two line up.
fn plus<S: 'static>(k: &Kit, help: &'static str, on_press: impl Fn(&mut S) + 'static) -> El {
    icon_slot::<S>(k, &NEW, help, on_press)
}

/// A small icon button in that 20 point slot.
fn icon_slot<S: 'static>(
    k: &Kit,
    glyph: &'static Icon,
    help: &'static str,
    on_press: impl Fn(&mut S) + 'static,
) -> El {
    let b = IconButton::new(glyph)
        .control_size(ControlSize::Small)
        .variant(IconButtonVariant::GhostMuted)
        .label(help)
        .help(help)
        .on_press(move |s: &mut S| on_press(s))
        .build(k);
    // No wash at rest, so the glyph stays quiet beside the title; the kit's hover and press overlays still show.
    row().size(20.0, 20.0).center().child(b.size(20.0, 20.0))
}

/// What a thread's state looks like at the start of its row: a faint ring when it is idle, a dot when
/// it has something to say, and the spinner while its agent works. Each sits in a slot as wide as
/// the spinner, so titles line up whatever the state.
fn mark(k: &Kit, mark: Mark, flush: bool) -> El {
    let dot = |color| el().size(8.0, 8.0).radius(4.0).fill(color);
    let shown = match mark {
        Mark::Idle => el().size(8.0, 8.0).radius(4.0).border(1.0, k.c.icon_faint),
        Mark::Unread => dot(k.c.bg_accent),
        Mark::Running => Spinner::new().size(14.0).color(k.c.icon_accent).build(k),
        Mark::Asking => dot(k.c.warning.icon),
        Mark::Failed => text("×")
            .font(crate::config::fonts::ui(14.0, 500.0))
            .color(k.c.danger.fg),
    };
    if flush {
        // At the left of the slot, where the pages' icons begin, not centred in it.
        return row().size(16.0, 16.0).items_center().pl(2.0).child(shown);
    }
    row().size(14.0, 14.0).center().child(shown)
}

fn thread_row<S: 'static>(
    k: &Kit,
    t: &Thread,
    subtitle: Option<&str>,
    at: At<S>,
    rename: &super::rename::Rename,
    rename_at: super::rename::At<S>,
    room: f32,
    // Its mark starts where the pages' icons do, for lists that sit under them; the project tree keeps its own.
    flush: bool,
) -> El {
    let (pid, tid) = (t.project, t.id);
    if rename.open_at((pid, tid), super::rename::Place::Sidebar) {
        let line = row()
            .key(format!("thread-{pid}-{tid}"))
            .w_full()
            .h(ROW)
            .pl(11.0)
            .pr(8.0)
            .gap(8.0)
            .items_center()
            .radius(k.radius.md)
            .fill(k.c.layer_02)
            .child(mark(k, t.mark, flush))
            .child(super::rename::input::<S>(k, rename, rename_at, k.ty.ui));
        return line;
    }
    let (name, t_loses) = (t.name.clone(), t.closing);
    let rename_name = t.name.clone();
    // Rune cuts a title mid-word; cut it at a word instead, in the type the row will set it in.
    let font = match subtitle {
        Some(_) => TextStyle {
            weight: 500.0,
            ..k.ty.ui_compact
        },
        None => k.ty.ui,
    };
    let label = at_word(&t.label, room, |s| {
        rune::text::measure(s, &[], font, None).0
    });
    // A running thread's title takes the accent the spinner has, so it reads as live in the list.
    let live;
    let kit = if t.mark == Mark::Running {
        let mut lit = k.clone();
        lit.c.text_muted = k.c.text_accent;
        lit.c.text_base = k.c.text_accent;
        live = lit;
        &live
    } else {
        k
    };
    let mut row = SidebarRow::new(label)
        .title_style(font)
        .selected(t.selected)
        .on_select(move |_: &mut S, _: &mut Tx| send(Cmd::OpenThread(pid, tid)))
        .on_double_click(move |s: &mut S, _: &mut Tx| {
            rename_at(s).start((pid, tid), &rename_name, super::rename::Place::Sidebar)
        })
        .on_close(move |s: &mut S, _: &mut Tx| {
            at(s).confirm = Some(Confirm {
                thread: Some(tid),
                project: pid,
                name: name.clone(),
                loses_work: t_loses,
            })
        });
    row = row.avatar(mark(k, t.mark, flush));
    if let Some(ago) = &t.ago {
        // In the slot the close button takes over on hover, so it stands where the section's plus does.
        row = row.status(RowStatus::Other(
            text(ago.clone()).font(k.ty.small).color(k.c.text_faint),
        ));
    }
    if let Some(project) = subtitle {
        row = row.subtitle(project.to_string());
    } else {
        row = row.height(ROW);
    }
    // The shortened title is only for the eye; assistive tech gets the whole one.
    row.build(kit)
        .key(format!("thread-{pid}-{tid}"))
        .label(t.label.clone())
}

/// What a row's title does not get of the sidebar's width: the insets and gutter, the row's own
/// padding, its mark and the slot at its end. Over-counted a little, so that a title that fits
/// here fits the row and Rune never has to cut it again.
const TITLE_RESERVE: f32 = 96.0;

/// `text` if `width` says it fits `room`, else its longest run of whole words with "…" after it
/// that does. When not even the first word fits, `text` as it is, for Rune to cut.
fn at_word(text: &str, room: f32, width: impl Fn(&str) -> f32) -> String {
    if room <= 0.0 || width(text) <= room {
        return text.to_string();
    }
    let ends: Vec<usize> = text
        .char_indices()
        .filter(|&(i, c)| c.is_whitespace() && i > 0)
        .map(|(i, _)| i)
        .collect();
    for &end in ends.iter().rev() {
        let head = text[..end].trim_end_matches(|c: char| c.is_whitespace() || ",;:-".contains(c));
        if head.is_empty() {
            continue;
        }
        let cut = format!("{head}…");
        if width(&cut) <= room {
            return cut;
        }
    }
    text.to_string()
}

/// A project: a header that toggles it (larger title, no disclosure glyph, the kit's header
/// hardcodes both) over its thread rows.
/// What a project's header needs to be dragged: where it is in the list and the list's shape.
struct Place {
    index: usize,
    ids: Rc<Vec<u64>>,
    heights: Rc<Vec<f32>>,
    /// How far it sits from its place, and whether it is the one being carried.
    shift: f32,
    lifted: bool,
}

fn project_section<S: 'static>(
    k: &Kit,
    p: &Project,
    place: Option<Place>,
    at: At<S>,
    rename: &super::rename::Rename,
    rename_at: super::rename::At<S>,
    room: f32,
) -> El {
    let pid = p.id;
    let pname = p.name.clone();
    let new_thread = plus::<S>(k, "New thread", move |_| send(Cmd::StartThread(pid)));
    // Out of sight until the pointer is on the header, as a thread row's close button is.
    let close = icon_slot::<S>(k, k.glyph.close, "Close project", move |s| {
        at(s).confirm = Some(Confirm {
            thread: None,
            project: pid,
            name: pname.clone(),
            loses_work: false,
        })
    })
    .group_opacity(0.0, 1.0)
    .focusable(false);
    let title = text(p.name.clone())
        .font(TextStyle {
            weight: 500.0,
            ..k.ty.ui
        })
        .color(k.c.text_base)
        .truncate()
        .shrink();
    let header = row()
        .key("section-header")
        .label(p.name.clone())
        .w_full()
        .h(ROW)
        .pl(11.0)
        .pr(8.0)
        .center_y()
        .radius(k.radius.md)
        .child(
            row()
                .key("section-title")
                .center_y()
                .shrink()
                .child(title)
                .a11y_hidden(),
        )
        .child(el().key("spacer").grow(1.0).a11y_hidden())
        .child(close)
        .child(new_thread)
        .hover_group()
        .on_press_tx(move |_: &mut S, _: &mut Tx| send(Cmd::ToggleProject(pid)));
    let header = match &place {
        Some(place) => {
            let (index, ids, heights) = (place.index, place.ids.clone(), place.heights.clone());
            header.on_drag(
                move |s: &mut S, d: &mut rune::Drag, tx: &mut Tx| match d.phase {
                    Phase::Down => {}
                    Phase::Move => {
                        if at(s).drag.is_none() && d.dy.abs() < SLOP {
                            return;
                        }
                        // The sections it passes spring aside; it follows the pointer exactly.
                        tx.animated();
                        at(s).drag = Some(ProjectDrag {
                            from: index,
                            to: drop_index(&heights, index, d.dy),
                            dy: d.dy,
                        });
                    }
                    Phase::Up => {
                        if let Some(drag) = at(s).drag.take()
                            && !d.cancelled()
                            && drag.to != drag.from
                        {
                            send(Cmd::MoveProject(ids[drag.from], ids[drag.to]));
                        }
                    }
                },
            )
        }
        None => header,
    };
    let header = k
        .interactive(header)
        .role("button")
        .expanded(!p.collapsed)
        .hover_motion(Anim::INSTANT, Anim::INSTANT)
        .focusable(false);
    let mut section = col()
        .key(format!("project-{pid}"))
        .role("group")
        .label(p.name.clone())
        .w_full()
        .pt(4.0)
        .gap(0.0)
        .motion(k.motion.disclosure)
        .child(header);
    if !p.collapsed {
        for t in &p.threads {
            section = section.child(thread_row::<S>(
                k, t, None, at, rename, rename_at, room, false,
            ));
        }
        if let Some((expanded, hidden)) = p.fold {
            let label = if expanded {
                "Show less".to_string()
            } else {
                format!("{hidden} more")
            };
            // As the threads' rows are: a glyph where their marks are, so the words start where the titles do, and quieter than they are.
            let chevron = rune::icon(if expanded { &CHEVRON_UP } else { &CHEVRON_DOWN }, 14.0)
                .color(k.c.icon_muted);
            section = section.child(
                SidebarRow::new(label)
                    .height(MORE_ROW)
                    .avatar(chevron)
                    .title_style(crate::config::fonts::ui(13.0, 400.0))
                    .select_only()
                    .on_select(move |_: &mut S, _: &mut Tx| send(Cmd::ToggleMore(pid)))
                    .build(k),
            );
        }
    }
    if let Some(place) = place {
        section = section.offset(0.0, place.shift);
        if place.lifted {
            // Follows the pointer exactly, opaque so the sections it passes don't show through.
            section = k.elevate(
                section
                    .raise()
                    .offset_motion(Anim::INSTANT)
                    .fill(k.c.layer_02)
                    .radius(k.radius.md),
                &k.elevation.raised,
            );
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

/// Where the sidebar sits: what the window's buttons take at its leading edge, and how wide it is.
#[derive(Clone, Copy)]
pub struct Edge {
    pub inset: f32,
    pub width: f32,
}

/// A chip in the finder: `label` and a chevron, opening `lines` when pressed.
fn chip<S: 'static>(k: &Kit, key: &str, label: String, lines: Vec<Line>, at: At<S>) -> El {
    let menu: dropdown::At<S> = Rc::new(move |s: &mut S| &mut at(s).menu);
    let face = row()
        .px(6.0)
        .py(2.0)
        .radius(k.radius.md)
        .gap(2.0)
        .items_center()
        .shrink()
        .child(
            text(label)
                .font(crate::config::fonts::ui(12.0, 500.0))
                .color(k.c.text_muted)
                .truncate()
                .shrink(),
        )
        .child(rune::icon(&dropdown::CHEVRON, 12.0).color(k.c.text_faint));
    dropdown::opens::<S>(k, key, face, lines, vec![], menu)
}

/// The search field, with under it how the list is laid out and which project it is narrowed to.
fn finder<S: 'static>(k: &Kit, snapshot: &Snapshot, search: El, at: At<S>) -> El {
    let layout = chip::<S>(
        k,
        "side-layout",
        match snapshot.layout {
            Layout::Activity => "By activity",
            Layout::Project => "By project",
        }
        .to_string(),
        vec![
            Line::choice(
                "By activity",
                snapshot.layout == Layout::Activity,
                Cmd::SideLayout(Layout::Activity),
            ),
            Line::choice(
                "By project",
                snapshot.layout == Layout::Project,
                Cmd::SideLayout(Layout::Project),
            ),
        ],
        at,
    );
    let mut tools = row().w_full().h(24.0).items_center().px(3.0).child(layout);
    // One project has nothing to narrow to.
    if snapshot.choices.len() > 1 {
        let name = snapshot
            .filter
            .and_then(|id| snapshot.choices.iter().find(|(p, _)| *p == id))
            .map_or("All projects", |(_, name)| name.as_str())
            .to_string();
        let mut lines = vec![Line::choice(
            "All projects",
            snapshot.filter.is_none(),
            Cmd::SideProject(None),
        )];
        for (id, name) in &snapshot.choices {
            lines.push(Line::choice(
                name.clone(),
                snapshot.filter == Some(*id),
                Cmd::SideProject(Some(*id)),
            ));
        }
        tools = tools.child(el().grow(1.0).a11y_hidden()).child(chip::<S>(
            k,
            "side-project",
            name,
            lines,
            at,
        ));
    }
    col().w_full().gap(8.0).child(search).child(tools)
}

/// The sidebar for `snapshot`, with its filter in the host's state through `at`.
pub fn view<S: 'static>(
    snapshot: &Snapshot,
    state: &State,
    k: &Kit,
    at: At<S>,
    rename_at: super::rename::At<S>,
    settings_open: bool,
    edge: Edge,
) -> El {
    let Edge { inset, width } = edge;
    let room = (width - TITLE_RESERVE).max(0.0);
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
        .on_input_change(move |s: &mut S, _: &mut Tx| {
            send(Cmd::SideQuery(at(s).query.text.clone()))
        });
    let search = row()
        .w_full()
        .h(ROW)
        .px(8.0)
        .gap(6.0)
        .items_center()
        .child(
            el().child(rune::icon(&SEARCH, 16.0).color(k.c.icon_muted))
                .a11y_hidden(),
        )
        .child(field);

    // The open count is a pill in the 20 point slot every row ends with, so it stands where their icons and times do.
    let (issues_open, issues_late) = snapshot.issues;
    let mut issues = SidebarRow::new("Issues")
        .height(ROW)
        .icon(&ISSUES)
        .selected(snapshot.issues_open && !settings_open);
    if issues_late > 0 {
        issues = issues.detail(format!("{issues_late} late"));
    }
    if issues_open > 0 {
        let pill = row()
            .h(16.0)
            .min_w(16.0)
            .px(5.0)
            .center()
            .radius(8.0)
            .fill(k.c.layer_03)
            .child(
                text(issues_open.to_string())
                    .font(k.ty.small_medium)
                    .color(k.c.text_muted),
            );
        issues = issues.status(RowStatus::Other(pill));
    }
    let pages = col()
        .w_full()
        .child(
            SidebarRow::new("Home")
                .height(ROW)
                .icon(&HOME)
                .selected(snapshot.home_open && !settings_open)
                .on_select(|_: &mut S, _: &mut Tx| send(Cmd::ToggleHome))
                .build(k)
                .key("home"),
        )
        .child(
            SidebarRow::new("New")
                .height(ROW)
                .icon(&NEW)
                .on_select(|_: &mut S, _: &mut Tx| send(Cmd::NewThread))
                .build(k)
                .key("new-thread"),
        )
        .child(
            issues
                .on_select(|_: &mut S, _: &mut Tx| send(Cmd::ToggleIssues))
                .build(k)
                .key("issues"),
        )
        // The search and what narrows it, right under the pages with the same step between rows, above the
        // threads they act on.
        .child(finder::<S>(k, snapshot, search, at));

    let mut bar = Sidebar::new()
        .label("Projects")
        .inset(12.0)
        .header_gap(4.0)
        .header_padding(12.0, 8.0)
        .row_gap(4.0)
        .header(pages);

    if !snapshot.suggestions.is_empty() {
        let mut list = col().w_full().gap(4.0).py(4.0);
        for s in &snapshot.suggestions {
            let id = s.id;
            list = list.child(
                col()
                    .w_full()
                    .gap(6.0)
                    .child(
                        text(s.text.clone())
                            .font(caption_style())
                            .color(k.c.text_base),
                    )
                    .child(
                        row()
                            .gap(6.0)
                            .child(
                                Button::new(s.yes)
                                    .variant(ButtonVariant::Submit)
                                    .on_press(move |_: &mut S| send(Cmd::Answer(id, true)))
                                    .build(k),
                            )
                            .child(
                                Button::new("Not now")
                                    .on_press(move |_: &mut S| send(Cmd::Answer(id, false)))
                                    .build(k),
                            ),
                    ),
            );
        }
        bar = bar.child(list);
    }

    if !snapshot.waiting.is_empty() {
        let mut needs = SidebarSection::new("Needs you")
            .key("needs-you")
            .count(snapshot.waiting.len());
        for w in &snapshot.waiting {
            needs = needs.row(thread_row::<S>(
                k,
                &w.thread,
                Some(&w.project),
                at,
                &state.rename,
                rename_at,
                room,
                true,
            ));
        }
        // Pinned above the scrolling projects so it stays in view; a long list scrolls within its own cap.
        bar = bar.header(
            col()
                .w_full()
                .max_h(NEEDS_MAX)
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .child(needs.build(k)),
        );
    }
    if snapshot.layout == Layout::Activity {
        for (age, title) in [
            (Age::Day, "Past day"),
            (Age::Week, "Past week"),
            (Age::Earlier, "Earlier"),
        ] {
            let mut section = SidebarSection::new(title).key(format!("recent-{title}"));
            let mut any = false;
            for r in snapshot.recent.iter().filter(|r| r.age == age) {
                any = true;
                section = section.row(thread_row::<S>(
                    k,
                    &r.thread,
                    Some(&r.project),
                    at,
                    &state.rename,
                    rename_at,
                    room,
                    true,
                ));
            }
            if any {
                bar = bar.child(section.build(k));
            }
        }
        if let Some((expanded, hidden)) = snapshot.recent_fold {
            let label = if expanded {
                "Show less".to_string()
            } else {
                format!("{hidden} more")
            };
            let chevron = rune::icon(if expanded { &CHEVRON_UP } else { &CHEVRON_DOWN }, 14.0)
                .color(k.c.icon_muted);
            bar = bar.child(
                SidebarRow::new(label)
                    .height(MORE_ROW)
                    .avatar(chevron)
                    .title_style(crate::config::fonts::ui(13.0, 400.0))
                    .select_only()
                    .on_select(|_: &mut S, _: &mut Tx| send(Cmd::ToggleMoreRecent))
                    .build(k),
            );
        }
    }
    // Projects are dragged to reorder them, unless a search hides some of them.
    let ids: Rc<Vec<u64>> = Rc::new(snapshot.projects.iter().map(|p| p.id).collect());
    let heights: Rc<Vec<f32>> = Rc::new(snapshot.projects.iter().map(section_height).collect());
    let draggable = !snapshot.searching && snapshot.projects.len() > 1;
    for (index, p) in snapshot.projects.iter().enumerate() {
        let place = draggable.then(|| {
            let drag = state
                .drag
                .filter(|d| d.from < heights.len() && d.to < heights.len());
            Place {
                index,
                ids: ids.clone(),
                heights: heights.clone(),
                shift: drag.map_or(0.0, |d| shift(&d, index, &heights)),
                lifted: drag.is_some_and(|d| d.from == index),
            }
        });
        bar = bar.child(project_section::<S>(
            k,
            p,
            place,
            at,
            &state.rename,
            rename_at,
            room,
        ));
    }

    let mut footer = vec![];
    if let Some(status) = &snapshot.status {
        footer.push(
            row()
                .w_full()
                .items_center()
                .gap(6.0)
                .child(
                    row().grow(1.0).min_w(0.0).child(
                        text(status.clone())
                            .font(caption_style())
                            .color(k.c.text_muted),
                    ),
                )
                .child(
                    Button::new("Dismiss")
                        .variant(ButtonVariant::Ghost)
                        .on_press(|_: &mut S| send(Cmd::DismissStatus))
                        .build(k),
                ),
        );
    }
    footer.push(
        SidebarRow::new("Add project")
            .height(ROW)
            .icon(&FOLDER_PLUS)
            .on_select(|_: &mut S, _: &mut Tx| send(Cmd::AddProject))
            .build(k)
            .key("add-project"),
    );
    footer.push(
        SidebarRow::new("Settings")
            .height(ROW)
            .icon(&SETTINGS)
            .on_select(|_: &mut S, _: &mut Tx| send(Cmd::OpenSettings))
            .build(k)
            .key("settings"),
    );
    for f in footer {
        bar = bar.footer(f);
    }
    // "Po" sits in the window's top band, over the strip the shell keeps empty above the sidebar;
    // the shell draws the rule under the band.
    let band = row()
        .absolute(0.0, -BAND)
        .w_full()
        .h(BAND)
        .pl(20.0 + inset)
        .pr(20.0)
        .items_center()
        .child(
            text("Po")
                .font(crate::config::fonts::ui(18.0, 600.0))
                .color(k.c.text_base)
                .key("brand"),
        );
    // The divider beside the sidebar, band and all, at the pane's right edge.
    let edge = rune::el()
        .pin(None, Some(-BAND), Some(0.0), Some(0.0))
        .w(1.0)
        .fill(k.c.border_base);
    col()
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(band)
        .child(edge)
        .child(bar.build(k))
}

/// The dialog that asks before a project or a thread is closed, over the whole window.
pub fn confirm_dialog<S: 'static>(k: &Kit, state: &State, at: At<S>, layer: &mut Vec<El>) {
    let Some(c) = &state.confirm else { return };
    let (title, detail) = match c.thread {
        Some(_) => (
            format!("Close \u{201c}{}\u{201d}?", c.name),
            if c.loses_work {
                "It is still working or waiting on you, and the threads it started go with it. What it is doing will be lost."
            } else {
                "The thread and any it started will be closed."
            },
        ),
        None => (
            format!("Close project \u{201c}{}\u{201d}?", c.name),
            "Its threads are closed and their agents stop.",
        ),
    };
    let (project, thread) = (c.project, c.thread);
    let cancel = Button::new("Cancel")
        .on_press(move |s: &mut S| at(s).confirm = None)
        .build(k);
    let ok = Button::new("Close")
        .variant(ButtonVariant::Danger)
        .on_press(move |s: &mut S| {
            at(s).confirm = None;
            send(match thread {
                Some(tid) => Cmd::CloseThread(project, tid),
                None => Cmd::CloseProject(project),
            });
        });
    let card = col()
        .key("confirm-card")
        .w(360.0)
        .px(20.0)
        .py(20.0)
        .gap(8.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_03)
        .child(
            text(title)
                .font(k.ty.ui)
                .color(k.c.text_base)
                .wrap_anywhere(),
        )
        .child(
            text(detail.to_string())
                .font(caption_style())
                .color(k.c.text_muted),
        )
        .child(
            row()
                .gap(8.0)
                .justify_end()
                .pt(8.0)
                .child(cancel)
                .child(ok.build(k)),
        );
    layer.push(
        rune::el()
            .key("confirm")
            .role("dialog")
            .label("Confirm close")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .fill(k.c.scrim)
            .center()
            .child(card)
            .on_press(move |s: &mut S| at(s).confirm = None),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Cx, Env, Runtime};

    struct App {
        side: State,
        snap: Snapshot,
        inset: f32,
    }
    fn view(s: &App, _: &mut Cx<App>) -> El {
        let kit = Kit::neutral(true);
        rune_kit::root(
            super::view::<App>(
                &s.snap,
                &s.side,
                &kit,
                |s| &mut s.side,
                |s| &mut s.side.rename,
                false,
                Edge {
                    inset: s.inset,
                    width: 300.0,
                },
            ),
            vec![],
        )
    }

    #[test]
    fn a_long_title_is_cut_at_a_word() {
        let wide = |s: &str| s.chars().count() as f32;
        assert_eq!(at_word("short", 10.0, wide), "short");
        assert_eq!(
            at_word("fix the sidebar colour bug", 16.0, wide),
            "fix the sidebar…"
        );
        // The ellipsis counts toward the room.
        assert_eq!(at_word("fix the sidebar colour", 15.0, wide), "fix the…");
        // Trailing punctuation does not stay in front of it.
        assert_eq!(at_word("fix, the sidebar", 8.0, wide), "fix…");
        // One long word has no boundary to cut at.
        assert_eq!(
            at_word("supercalifragilistic", 8.0, wide),
            "supercalifragilistic"
        );
    }

    #[test]
    fn the_title_keeps_clear_of_the_window_buttons() {
        let at = |inset| {
            let mut rt = Runtime::new(
                App {
                    side: State::default(),
                    snap: Snapshot::default(),
                    inset,
                },
                view,
                (300.0, 600.0),
                Env::new(true, 1.0),
            );
            rt.frame(0.0);
            rt.elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some("brand"))
                .expect("title")
                .rect[0]
        };
        assert_eq!(
            at(70.0) - at(0.0),
            70.0,
            "the title moves over by what the buttons take"
        );
    }

    #[test]
    fn a_double_press_on_a_thread_keeps_its_line_open() {
        let thread = Thread {
            project: 1,
            id: 2,
            label: "Fix it".into(),
            name: "Fix it".into(),
            mark: Mark::Idle,
            selected: false,
            controlled: false,
            ago: None,
            closing: false,
        };
        let project = Project {
            id: 1,
            name: "po".into(),
            active: true,
            collapsed: false,
            closable: false,
            threads: vec![thread],
            fold: None,
        };
        let snap = Snapshot {
            projects: vec![project],
            ..Default::default()
        };
        let mut rt = Runtime::new(
            App {
                side: State::default(),
                snap,
                inset: 0.0,
            },
            view,
            (300.0, 600.0),
            Env::new(true, 1.0),
        );
        rt.frame(0.0);
        let (x, y) = rt.find("thread-1-2").expect("row");
        for _ in 0..2 {
            rt.pointer_down(x, y);
            rt.pointer_up(x, y);
            rt.frame(1.0 / 60.0);
        }
        assert!(rt.state.side.rename.editing.is_some(), "the line opened");
        for _ in 0..5 {
            rt.update(|s, _| {
                s.side.rename.settle();
            });
            rt.frame(1.0 / 60.0);
        }
        assert!(rt.state.side.rename.editing.is_some(), "and stayed");
    }

    fn two_projects() -> Snapshot {
        let thread = |project, id| Thread {
            project,
            id,
            label: "Fix it".into(),
            name: "Fix it".into(),
            mark: Mark::Idle,
            selected: false,
            controlled: false,
            ago: None,
            closing: false,
        };
        let project = |id, threads| Project {
            id,
            name: format!("p{id}"),
            active: id == 1,
            collapsed: false,
            closable: true,
            threads,
            fold: None,
        };
        Snapshot {
            layout: Layout::Project,
            projects: vec![
                project(1, vec![thread(1, 1), thread(1, 2)]),
                project(2, vec![thread(2, 1)]),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn sections_are_as_tall_as_the_drag_reckons() {
        let snap = two_projects();
        let mut rt = Runtime::new(
            App {
                side: State::default(),
                snap: snap.clone(),
                inset: 0.0,
            },
            view,
            (300.0, 600.0),
            Env::new(true, 1.0),
        );
        rt.frame(0.0);
        let (_, first) = rt.find("project-1").expect("first");
        let (_, second) = rt.find("project-2").expect("second");
        let (h1, h2) = (
            section_height(&snap.projects[0]),
            section_height(&snap.projects[1]),
        );
        // `find` gives a middle: the two middles are half of each section and the gap apart.
        assert!(
            (second - first - (h1 / 2.0 + SECTION_GAP + h2 / 2.0)).abs() < 1.0,
            "{first} {second}"
        );
    }

    #[test]
    fn dragging_a_header_past_the_next_section_takes_its_place() {
        let heights = [100.0, 40.0, 70.0];
        assert_eq!(drop_index(&heights, 0, 10.0), 0);
        assert_eq!(drop_index(&heights, 0, 100.0), 1);
        assert_eq!(drop_index(&heights, 0, 400.0), 2);
        assert_eq!(drop_index(&heights, 2, -70.0), 1);
        assert_eq!(drop_index(&heights, 2, -400.0), 0);
        let drag = ProjectDrag {
            from: 0,
            to: 2,
            dy: 150.0,
        };
        assert_eq!(
            [0, 1, 2].map(|i| shift(&drag, i, &heights)),
            [150.0, -104.0, -104.0]
        );
    }

    #[test]
    fn a_drag_on_a_header_lifts_it() {
        let mut rt = Runtime::new(
            App {
                side: State::default(),
                snap: two_projects(),
                inset: 0.0,
            },
            view,
            (300.0, 600.0),
            Env::new(true, 1.0),
        );
        rt.frame(0.0);
        let (x, y) = rt.find("project-1").expect("first");
        // The middle of the section is on a thread row, so reach for its header.
        let y = y - section_height(&rt.state.snap.projects[0]) / 2.0 + 4.0 + ROW / 2.0;
        rt.pointer_down(x, y);
        rt.pointer_move(x, y + 100.0);
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.side.drag.map(|d| (d.from, d.to)), Some((0, 1)));
        rt.pointer_up(x, y + 100.0);
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.side.drag, None);
    }
}
