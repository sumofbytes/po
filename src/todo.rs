//! The todos window, opened from the sidebar: a list of things to do, each with a date it is due by, nested under one
//! another and grouped under headers. There is one list for all windows, kept in `todos.json`.
use std::path::PathBuf;

use gpui::{AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, prelude::FluentBuilder as _, px, rems};
use serde_json::{Value, json};

use crate::theme::Palette;
use crate::ui::button::Button;
use crate::ui::icon::{Icon, IconName, IconSize};
use crate::ui::input::{Input, InputEvent, InputState};
use crate::{ROW_INSET, row_group, show_on_hover, side_centered, side_chevron, side_row};

/// A row of the list: roomier than the sidebar's, as the window has the space.
const TODO_ROW_HEIGHT: gpui::Rems = rems(2.0);
/// How far a subtask sits in from its parent: the lead column and the gap after it, so its box
/// lines up with the parent's text.
const TODO_INDENT: gpui::Rems = rems(1.5);

/// A day, as days since 1970-01-01.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Date(i64);

const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
/// From Sunday, as `Date::weekday` counts.
const WEEKDAYS: [&str; 7] = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];

fn leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Date {
    /// The day, if there is one by that number in that month.
    pub fn from_ymd(year: i64, month: u32, day: u32) -> Option<Self> {
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        // Howard Hinnant's days_from_civil.
        let y = if month <= 2 { year - 1 } else { year };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(month);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Some(Self(era * 146097 + doe - 719468))
    }

    pub fn ymd(self) -> (i64, u32, u32) {
        let z = self.0 + 719468;
        let era = z.div_euclid(146097);
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (yoe + era * 400 + i64::from(month <= 2), month, day)
    }

    /// 0 for Sunday through 6 for Saturday.
    pub fn weekday(self) -> usize {
        (self.0 + 4).rem_euclid(7) as usize
    }

    pub fn plus(self, days: i64) -> Self {
        Self(self.0 + days)
    }

    /// `2026-11-02`, as the file keeps it.
    pub fn iso(self) -> String {
        let (y, m, d) = self.ymd();
        format!("{y:04}-{m:02}-{d:02}")
    }

    fn from_iso(text: &str) -> Option<Self> {
        let mut parts = text.splitn(3, '-');
        let y = parts.next()?.parse().ok()?;
        let m = parts.next()?.parse().ok()?;
        let d = parts.next()?.parse().ok()?;
        Self::from_ymd(y, m, d)
    }
}

/// Today where the user is.
pub fn today() -> Date {
    #[cfg(unix)]
    {
        // SAFETY: `localtime_r` writes only into the `tm` it is given.
        let local = unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            (!libc::localtime_r(&now, &mut tm).is_null()).then_some(tm)
        };
        if let Some(day) = local.and_then(|tm| Date::from_ymd(i64::from(tm.tm_year) + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32)) {
            return day;
        }
    }
    Date((crate::now_ms() / 86_400_000) as i64)
}

/// A month by its name or the first three letters of it.
fn month(word: &str) -> Option<u32> {
    (word.len() >= 3).then(|| MONTHS.iter().position(|name| name.starts_with(word))).flatten().map(|ix| ix as u32 + 1)
}

/// The first day after `today` that is `weekday`.
fn next_weekday(today: Date, weekday: usize) -> Date {
    let ahead = (weekday as i64 - today.weekday() as i64).rem_euclid(7);
    today.plus(if ahead == 0 { 7 } else { ahead })
}

/// A day of `month` with no year given: this year's, or next year's once this year's has passed.
fn coming(today: Date, month: u32, day: u32) -> Option<Date> {
    let year = today.ymd().0;
    let date = Date::from_ymd(year, month, day)?;
    if date >= today { Some(date) } else { Date::from_ymd(year + 1, month, day) }
}

/// What the user typed for a due date, read the way people say it: `today`, `tomorrow`, a weekday
/// (the next one), `next week` (its Monday), `+3d` or `2w` from today, `nov 2` or `2 nov`, or
/// `2026-11-02`. Nothing typed is no date, `Some(None)`; what can't be read is `None`.
pub fn parse_due(text: &str, today: Date) -> Option<Option<Date>> {
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return Some(None);
    }
    let words: Vec<&str> = text.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).collect();
    let date = match words.as_slice() {
        ["today" | "tod"] => Some(today),
        ["tomorrow" | "tmr" | "tom"] => Some(today.plus(1)),
        ["next", "week"] => Some(next_weekday(today, 1)),
        [word] if word.len() >= 3 && WEEKDAYS.iter().any(|day| day.starts_with(word)) => WEEKDAYS.iter().position(|day| day.starts_with(word)).map(|ix| next_weekday(today, ix)),
        [word] if word.contains('-') => Date::from_iso(word),
        [word] => {
            let count = word.trim_start_matches('+');
            let (number, unit) = count.split_at(count.find(|c: char| !c.is_ascii_digit()).unwrap_or(count.len()));
            let number: i64 = number.parse().ok()?;
            match unit {
                "d" | "day" | "days" => Some(today.plus(number)),
                "w" | "wk" | "week" | "weeks" => Some(today.plus(number * 7)),
                _ => None,
            }
        }
        [first, second] | [first, second, _] => {
            let (name, day) = if first.chars().all(|c| c.is_ascii_digit()) { (second, first) } else { (first, second) };
            let (month, day) = (month(name)?, day.parse().ok()?);
            match words.get(2) {
                Some(year) => Date::from_ymd(year.parse().ok()?, month, day),
                None => coming(today, month, day),
            }
        }
        _ => None,
    };
    date.map(Some)
}

/// A due date as the list shows it, near ones by how near: `Today`, `Tomorrow`, `Fri`, then
/// `Nov 2`, with the year once it isn't this one.
pub fn due_label(due: Date, today: Date) -> String {
    let short = |name: &str| {
        let mut chars = name.chars();
        chars.next().map(|c| c.to_ascii_uppercase()).into_iter().chain(chars.take(2)).collect::<String>()
    };
    match due.0 - today.0 {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        2..=6 => short(WEEKDAYS[due.weekday()]),
        _ => {
            let (year, month, day) = due.ymd();
            let name = short(MONTHS[month as usize - 1]);
            if year == today.ymd().0 { format!("{name} {day}") } else { format!("{name} {day} {year}") }
        }
    }
}

/// A todo, or a header that groups the todos under it. Headers are only ever at the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: u64,
    pub text: String,
    pub header: bool,
    pub done: bool,
    pub due: Option<Date>,
    /// Its children are hidden.
    pub folded: bool,
    pub children: Vec<Item>,
}

impl Item {
    /// Its todos, done and in all, at every depth.
    pub fn progress(&self) -> (usize, usize) {
        self.children.iter().fold((0, 0), |(done, all), child| {
            let (d, a) = child.progress();
            (done + d + usize::from(child.done && !child.header), all + a + usize::from(!child.header))
        })
    }

    fn finish(&mut self) {
        self.done = true;
        self.children.iter_mut().for_each(Item::finish);
    }

    /// Whether it shows: `show` takes it, or something under it. An empty header shows only while
    /// nothing is filtered, so it can be filled.
    fn shows(&self, show: Show) -> bool {
        let taken = match show {
            Show::All => true,
            _ if self.header => false,
            Show::Active => !self.done,
            Show::Done => self.done,
        };
        taken || self.children.iter().any(|child| child.shows(show))
    }
}

/// Which todos the list shows, as its footer picks.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Show {
    #[default]
    All,
    Active,
    Done,
}

/// Where a todo with no header goes at the top: after the others like it, before the first header,
/// so it never looks as though it were under one.
fn ungrouped_end(items: &[Item]) -> usize {
    items.iter().position(|item| item.header).unwrap_or(items.len())
}

#[derive(Default, Debug, PartialEq)]
pub struct Todos {
    pub items: Vec<Item>,
    next: u64,
}

fn find(items: &mut [Item], id: u64) -> Option<&mut Item> {
    items.iter_mut().find_map(|item| if item.id == id { Some(item) } else { find(&mut item.children, id) })
}

/// Takes `id` out of `items`; a header leaves its todos where it was.
fn remove(items: &mut Vec<Item>, id: u64) -> bool {
    if let Some(ix) = items.iter().position(|item| item.id == id) {
        let item = items.remove(ix);
        if item.header {
            let at = ungrouped_end(items);
            items.splice(at..at, item.children);
        }
        return true;
    }
    items.iter_mut().any(|item| remove(&mut item.children, id))
}

/// Drops the done todos, and what is under them, from `items`.
fn clear_done(items: &mut Vec<Item>) {
    items.retain(|item| item.header || !item.done);
    items.iter_mut().for_each(|item| clear_done(&mut item.children));
}

impl Todos {
    /// Adds a todo at the end of `parent`'s, or of the todos under no header, or a header at the
    /// end of the list.
    pub fn add(&mut self, parent: Option<u64>, text: String, due: Option<Date>, header: bool) -> u64 {
        self.next += 1;
        let item = Item { id: self.next, text, header, done: false, due, folded: false, children: vec![] };
        match parent.filter(|_| !header).and_then(|id| find(&mut self.items, id)) {
            Some(parent) => {
                parent.folded = false;
                parent.children.push(item);
            }
            None if header => self.items.push(item),
            None => self.items.insert(ungrouped_end(&self.items), item),
        }
        self.next
    }

    pub fn get(&mut self, id: u64) -> Option<&mut Item> {
        find(&mut self.items, id)
    }

    /// Checks a todo off, and everything under it, or unchecks just it.
    pub fn toggle(&mut self, id: u64) {
        if let Some(item) = self.get(id) {
            if item.done { item.done = false } else { item.finish() }
        }
    }

    pub fn remove(&mut self, id: u64) {
        remove(&mut self.items, id);
    }

    pub fn clear_done(&mut self) {
        clear_done(&mut self.items);
    }

    /// How many todos are left to do, and whether any are done.
    pub fn counts(&self) -> (usize, bool) {
        let (done, all) = self.items.iter().fold((0, 0), |(done, all), item| {
            let (d, a) = item.progress();
            (done + d + usize::from(item.done && !item.header), all + a + usize::from(!item.header))
        });
        (all - done, done > 0)
    }

    /// How many todos not yet done were due before `today`.
    pub fn late(&self, today: Date) -> usize {
        fn late(items: &[Item], today: Date) -> usize {
            items.iter().map(|item| usize::from(!item.header && !item.done && item.due.is_some_and(|due| due < today)) + late(&item.children, today)).sum()
        }
        late(&self.items, today)
    }

    fn to_json(&self) -> Value {
        fn entry(item: &Item) -> Value {
            json!({
                "text": item.text, "header": item.header, "done": item.done, "due": item.due.map(Date::iso),
                "folded": item.folded, "children": item.children.iter().map(entry).collect::<Vec<_>>(),
            })
        }
        json!({ "version": 1, "items": self.items.iter().map(entry).collect::<Vec<_>>() })
    }

    /// The list a file holds; ids are handed out afresh, so the file needs none.
    fn from_json(value: &Value) -> Self {
        fn items(list: &Value, next: &mut u64, top: bool) -> Vec<Item> {
            list.as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    *next += 1;
                    let id = *next;
                    Some(Item {
                        id,
                        text: entry["text"].as_str()?.to_string(),
                        header: top && entry["header"].as_bool().unwrap_or(false),
                        done: entry["done"].as_bool().unwrap_or(false),
                        due: entry["due"].as_str().and_then(Date::from_iso),
                        folded: entry["folded"].as_bool().unwrap_or(false),
                        children: items(&entry["children"], next, false),
                    })
                })
                .collect()
        }
        let mut next = 0;
        let items = items(&value["items"], &mut next, true);
        Self { items, next }
    }
}

fn file() -> PathBuf {
    crate::config::data_dir().join("todos.json")
}

fn save(todos: &Todos) -> std::io::Result<()> {
    let target = file();
    std::fs::create_dir_all(target.parent().ok_or_else(|| std::io::Error::other("no directory"))?)?;
    let temp = target.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temp, serde_json::to_vec_pretty(&todos.to_json()).unwrap_or_default())?;
    std::fs::rename(&temp, &target)
}

/// What the composer is writing: a subtask of this todo or a todo under this header, a header, or
/// a change to a row. New todos at the top come from the add field instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Under(u64),
    Header,
    Edit(u64),
}

pub enum TodoEvent {
    Close,
}

/// The todos window, laid out like a plain todo app: a field to add one at the top, the list with
/// a box to check each off, and a footer with what is left and which to show. The sidebar only has
/// a row that opens it.
pub struct TodoPanel {
    todos: Todos,
    focus: FocusHandle,
    show: Show,
    /// The add field's inputs, always there.
    new_text: Entity<InputState>,
    new_due: Entity<InputState>,
    /// The composer's, for a subtask, a header or an edit.
    text: Entity<InputState>,
    due: Entity<InputState>,
    composer: Option<Target>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TodoEvent> for TodoPanel {}

const DUE_HINT: &str = "Due: tomorrow, fri, nov 2…";

impl TodoPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_text = cx.new(|cx| InputState::new(window, cx).placeholder("What needs to be done?"));
        let new_due = cx.new(|cx| InputState::new(window, cx).placeholder(DUE_HINT));
        let text = cx.new(|cx| InputState::new(window, cx));
        let due = cx.new(|cx| InputState::new(window, cx).placeholder(DUE_HINT));
        let add = |panel: &mut Self, _: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>| match event {
            InputEvent::PressEnter { .. } => panel.add(window, cx),
            _ => cx.notify(),
        };
        let commit = |panel: &mut Self, _: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>| match event {
            InputEvent::PressEnter { .. } => panel.commit(window, cx),
            _ => cx.notify(),
        };
        let subscriptions = vec![
            cx.subscribe_in(&new_text, window, add),
            cx.subscribe_in(&new_due, window, add),
            cx.subscribe_in(&text, window, commit),
            cx.subscribe_in(&due, window, commit),
        ];
        let todos = crate::config::read_json(&file()).map(|value| Todos::from_json(&value)).unwrap_or_default();
        Self { todos, focus: cx.focus_handle(), show: Show::All, new_text, new_due, text, due, composer: None, _subscriptions: subscriptions }
    }

    /// Puts the cursor in the add field, as the window opens.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_text.update(cx, |input, cx| input.focus(window, cx));
    }

    /// How many todos are left, and how many of those are late, for the sidebar's row.
    pub fn summary(&self) -> (usize, usize) {
        (self.todos.counts().0, self.todos.late(today()))
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = save(&self.todos) {
            log::warn!("saving {}: {error}", file().display());
        }
        cx.notify();
    }

    /// Adds what the add field holds, and leaves it empty and focused for the next one.
    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.new_text.read(cx).value().trim().to_string();
        let Some(due) = parse_due(&self.new_due.read(cx).value(), today()) else { return };
        if text.is_empty() {
            return;
        }
        self.todos.add(None, text, due, false);
        // A new todo is one to do, so it shouldn't land out of sight.
        if self.show == Show::Done {
            self.show = Show::All;
        }
        for input in [&self.new_text, &self.new_due] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.new_text.update(cx, |input, cx| input.focus(window, cx));
        self.changed(cx);
    }

    /// Opens the composer for `target`, holding what is there already when it edits.
    fn compose(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let (text, due) = match target {
            Target::Edit(id) => self.todos.get(id).map_or_else(Default::default, |item| (item.text.clone(), item.due.map(Date::iso).unwrap_or_default())),
            _ => Default::default(),
        };
        let placeholder = match target {
            Target::Header => "Header",
            Target::Under(id) if self.todos.items.iter().any(|item| item.id == id && item.header) => "Todo",
            _ => "Subtask",
        };
        if let Target::Under(id) = target
            && let Some(parent) = self.todos.get(id)
        {
            parent.folded = false;
        }
        self.text.update(cx, |input, cx| {
            input.set_placeholder(placeholder, cx);
            input.set_value(text, window, cx);
            input.select_all(window, cx);
            input.focus(window, cx);
        });
        self.due.update(cx, |input, cx| input.set_value(due, window, cx));
        self.composer = Some(target);
        cx.notify();
    }

    fn close_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer = None;
        self.focus(window, cx);
        cx.notify();
    }

    /// Keeps what the composer holds. Adding leaves it open for the next one, under a new header for
    /// the todos that go in it; an edit closes it. Nothing typed closes it as well.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.composer else { return };
        let text = self.text.read(cx).value().trim().to_string();
        if text.is_empty() {
            return self.close_composer(window, cx);
        }
        let Some(due) = parse_due(&self.due.read(cx).value(), today()) else { return };
        match target {
            Target::Edit(id) => {
                if let Some(item) = self.todos.get(id) {
                    item.text = text;
                    if !item.header {
                        item.due = due;
                    }
                }
                self.close_composer(window, cx);
            }
            Target::Under(parent) => {
                self.todos.add(Some(parent), text, due, false);
                for input in [&self.text, &self.due] {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
            }
            Target::Header => {
                let id = self.todos.add(None, text, None, true);
                self.changed(cx);
                return self.compose(Target::Under(id), window, cx);
            }
        }
        self.changed(cx);
    }

    /// A line to type a todo into: its lead icon, the text, then the due date with how it was read.
    fn entry(lead: IconName, text: &Entity<InputState>, due: Option<&Entity<InputState>>, cx: &gpui::App) -> gpui::Div {
        let colors = cx.global::<Palette>();
        let (muted, danger) = (colors.muted_foreground, colors.danger);
        let due = due.map(|due| {
            let hint = match parse_due(&due.read(cx).value(), today()) {
                Some(Some(date)) => div().flex_none().text_xs().text_color(muted).child(due_label(date, today())),
                Some(None) => div(),
                None => div().flex_none().text_xs().text_color(danger).child("?"),
            };
            div()
                .flex_none()
                .w(rems(13.))
                .flex()
                .items_center()
                .gap_2()
                .child(Icon::new(IconName::Calendar).size(IconSize::Small).color(muted))
                .child(div().flex_1().min_w_0().child(Input::new(due)))
                .child(hint)
        });
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(side_centered(Icon::new(lead).size(IconSize::Small).color(muted)))
            .child(div().flex_1().min_w_0().child(Input::new(text)))
            .children(due)
    }

    /// The field at the top: Enter adds what it holds, Esc empties it.
    fn add_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.global::<Palette>();
        let (radius, filled, border) = (colors.radius, colors.input_background, colors.input_border);
        let focus = self.new_text.clone();
        div()
            .id("todo-new")
            .h(TODO_ROW_HEIGHT + rems(0.5))
            .flex()
            .items_center()
            .px(ROW_INSET)
            .rounded(radius)
            .border_1()
            .border_color(border)
            .bg(filled)
            .cursor_text()
            .on_click(move |_, window, cx| focus.update(cx, |input, cx| input.focus(window, cx)))
            .on_key_down(cx.listener(|panel, event: &KeyDownEvent, window, cx| {
                // An empty field lets Esc through to close the window.
                let typed = [&panel.new_text, &panel.new_due].iter().any(|input| !input.read(cx).value().is_empty());
                if event.keystroke.key == "escape" && typed {
                    for input in [&panel.new_text, &panel.new_due] {
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    cx.stop_propagation();
                }
            }))
            .child(Self::entry(IconName::Plus, &self.new_text, Some(&self.new_due), cx))
            .into_any_element()
    }

    /// The composer, at `depth`, in place of the row it edits or where what it adds will go.
    fn composer(&self, target: Target, depth: usize, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.global::<Palette>();
        let (radius, filled) = (colors.radius, colors.input_background);
        let header = match target {
            Target::Header => true,
            Target::Edit(id) => self.todos.items.iter().any(|item| item.id == id && item.header),
            Target::Under(_) => false,
        };
        let lead = if header { IconName::Heading } else { IconName::Square };
        div()
            .id("todo-composer")
            .h(TODO_ROW_HEIGHT)
            .flex()
            .items_center()
            .gap_2()
            .px(ROW_INSET)
            .ml(TODO_INDENT * depth as f32)
            .rounded(radius)
            .bg(filled)
            .on_key_down(cx.listener(|panel, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    panel.close_composer(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(Self::entry(lead, &self.text, (!header).then_some(&self.due), cx))
            .child(
                Button::new("todo-composer-close")
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .tooltip(if matches!(target, Target::Edit(_)) { "Cancel (Esc)" } else { "Done adding (Esc)" })
                    .on_click(cx.listener(|panel, _, window, cx| panel.close_composer(window, cx))),
            )
            .into_any_element()
    }

    /// The rows for `items` at `depth`, each followed by its children unless it is folded, and the
    /// composer where it goes. Only what [`Item::shows`] shows is there.
    fn rows(&self, items: &[Item], depth: usize, out: &mut Vec<AnyElement>, cx: &mut Context<Self>) {
        let today = today();
        let filtering = self.show != Show::All;
        for item in items.iter().filter(|item| item.shows(self.show)) {
            let id = item.id;
            if self.composer == Some(Target::Edit(id)) {
                out.push(self.composer(Target::Edit(id), depth, cx));
            } else if item.header {
                out.push(self.header_row(item, !out.is_empty(), filtering, cx));
            } else {
                out.push(self.todo_row(item, depth, today, filtering, cx));
            }
            let adding = self.composer == Some(Target::Under(id));
            if !item.folded || filtering || adding {
                let below = if item.header { depth } else { depth + 1 };
                self.rows(&item.children, below, out, cx);
                if adding {
                    out.push(self.composer(Target::Under(id), below, cx));
                }
            }
        }
    }

    fn header_row(&self, item: &Item, spaced: bool, filtering: bool, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.global::<Palette>();
        let (hover, muted, foreground, radius) = (colors.list_hover, colors.muted_foreground, colors.foreground, colors.radius);
        let id = item.id;
        let (done, all) = item.progress();
        let group = row_group("todo-header", id);
        side_row(radius, None)
            .id(("todo-header", id as usize))
            .group(group.clone())
            .h(TODO_ROW_HEIGHT)
            .when(spaced, |row| row.mt(rems(0.75)))
            .cursor_pointer()
            .hover(move |row| row.bg(hover))
            .on_click(cx.listener(move |panel, _, _, cx| {
                if let Some(item) = panel.todos.get(id) {
                    item.folded = !item.folded;
                }
                panel.changed(cx);
            }))
            .child(side_chevron(!item.folded || filtering, muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).text_color(foreground).child(item.text.clone()))
                    .when(all > 0, |row| row.child(div().flex_none().text_xs().text_color(muted).child(format!("{done}/{all}")))),
            )
            .child(show_on_hover(group.clone(), self.action(("todo-header-add", id), IconName::Plus, "Add a todo under this header", Target::Under(id), cx)))
            .child(show_on_hover(group.clone(), self.action(("todo-header-edit", id), IconName::Pencil, "Rename header", Target::Edit(id), cx)))
            .child(show_on_hover(group, self.delete(("todo-header-remove", id), "Remove header, keep its todos", cx)))
            .into_any_element()
    }

    fn todo_row(&self, item: &Item, depth: usize, today: Date, filtering: bool, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.global::<Palette>();
        let (hover, muted, foreground, radius) = (colors.list_hover, colors.muted_foreground, colors.foreground, colors.radius);
        let id = item.id;
        let (done, all) = item.progress();
        let group = row_group("todo", id);
        // Late is loud, today is a warning, and anything once it is done is quiet.
        let due = item.due.map(|due| {
            let tint = match due.cmp(&today) {
                _ if item.done => muted,
                std::cmp::Ordering::Less => colors.danger,
                std::cmp::Ordering::Equal => colors.warning,
                std::cmp::Ordering::Greater => muted,
            };
            div().flex_none().text_xs().text_color(tint).child(due_label(due, today))
        });
        let check = if item.done { IconName::SquareCheck } else { IconName::Square };
        // Folding a todo is on its count of subtasks, since its lead column is its box.
        let count = (all > 0).then(|| {
            div()
                .id(("todo-fold", id as usize))
                .flex_none()
                .text_xs()
                .text_color(muted)
                .hover(move |count| count.text_color(foreground))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |panel, _, _, cx| {
                    if let Some(item) = panel.todos.get(id) {
                        item.folded = !item.folded;
                    }
                    panel.changed(cx);
                }))
                .child(format!("{}{done}/{all}", if item.folded && !filtering { "▸ " } else { "" }))
        });
        side_row(radius, None)
            .id(("todo", id as usize))
            .group(group.clone())
            .h(TODO_ROW_HEIGHT)
            .ml(TODO_INDENT * depth as f32)
            .cursor_pointer()
            .hover(move |row| row.bg(hover))
            // A click on the row checks it off, as in any todo list; the pencil edits it.
            .on_click(cx.listener(move |panel, _, _, cx| {
                panel.todos.toggle(id);
                panel.changed(cx);
            }))
            .child(side_centered(Icon::new(check).size(IconSize::Small).color(if item.done { muted } else { foreground })))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(item.done, |text| text.line_through().text_color(muted))
                    .when(!item.done, |text| text.text_color(foreground))
                    .child(item.text.clone()),
            )
            .children(count)
            .children(due)
            .child(show_on_hover(group.clone(), self.action(("todo-add", id), IconName::Plus, "Add a subtask", Target::Under(id), cx)))
            .child(show_on_hover(group.clone(), self.action(("todo-edit", id), IconName::Pencil, "Edit, or set its due date", Target::Edit(id), cx)))
            .child(show_on_hover(group, self.delete(("todo-remove", id), if all > 0 { "Delete, with its subtasks" } else { "Delete" }, cx)))
            .into_any_element()
    }

    /// A small button that opens the composer for `target`.
    fn action(&self, (name, id): (&'static str, u64), icon: IconName, tooltip: &'static str, target: Target, cx: &mut Context<Self>) -> Button {
        Button::new((name, id as usize)).icon(icon).ghost().xsmall().tooltip(tooltip).on_click(cx.listener(move |panel, _, window, cx| panel.compose(target, window, cx)))
    }

    fn delete(&self, (name, id): (&'static str, u64), tooltip: &'static str, cx: &mut Context<Self>) -> Button {
        Button::new((name, id as usize)).icon(IconName::Close).ghost().xsmall().tooltip(tooltip).on_click(cx.listener(move |panel, _, _, cx| {
            panel.todos.remove(id);
            panel.changed(cx);
        }))
    }

    /// What is left, which todos to show, and a way to clear the done ones.
    fn footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.global::<Palette>();
        let (muted, foreground, border) = (colors.muted_foreground, colors.foreground, colors.border);
        let (left, any_done) = self.todos.counts();
        let link = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .cursor_pointer()
                .text_color(if on { foreground } else { muted })
                .when(on, |link| link.font_weight(FontWeight::MEDIUM))
                .hover(move |link| link.text_color(foreground))
                .child(label)
        };
        let tabs = [("todo-show-all", "All", Show::All), ("todo-show-active", "Active", Show::Active), ("todo-show-done", "Done", Show::Done)].map(|(id, label, show)| {
            link(id, label, self.show == show).on_click(cx.listener(move |panel, _, _, cx| {
                panel.show = show;
                cx.notify();
            }))
        });
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_4()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(border)
            .text_sm()
            .text_color(muted)
            .child(div().flex_1().min_w_0().truncate().child(if left == 1 { "1 item left".to_string() } else { format!("{left} items left") }))
            .children(tabs)
            .child(
                div().w(rems(5.5)).flex().justify_end().when(any_done, |slot| {
                    slot.child(link("todo-clear-done", "Clear done", false).on_click(cx.listener(|panel, _, _, cx| {
                        panel.todos.clear_done();
                        panel.changed(cx);
                    })))
                }),
            )
            .into_any_element()
    }
}

impl Render for TodoPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows = vec![];
        self.rows(&self.todos.items, 0, &mut rows, cx);
        if self.composer == Some(Target::Header) {
            rows.push(self.composer(Target::Header, 0, cx));
        }
        let colors = cx.global::<Palette>();
        let (popover, border, muted, radius) = (colors.popover, colors.border, colors.muted_foreground, colors.radius);
        if rows.is_empty() {
            let empty = match self.show {
                _ if self.todos.items.is_empty() => "Nothing to do yet. Add a todo above, or a header to group them.",
                Show::Done => "Nothing done yet",
                _ => "All done",
            };
            rows.push(div().py_6().flex().justify_center().text_sm().text_color(muted).child(empty).into_any_element());
        }
        let has_todos = self.todos.counts() != (0, false);
        div()
            .absolute()
            .inset_0()
            .id("todos-backdrop")
            .occlude()
            .on_click(cx.listener(|_, _: &gpui::ClickEvent, _, cx| cx.emit(TodoEvent::Close)))
            .flex()
            .justify_center()
            .items_start()
            .py_6()
            .child(
                div()
                    .id("todos")
                    .occlude()
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key == "escape" {
                            cx.emit(TodoEvent::Close);
                        }
                    }))
                    .w(px(600.))
                    .max_h_full()
                    .flex()
                    .flex_col()
                    .rounded(radius)
                    .border_1()
                    .border_color(border)
                    .bg(popover)
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_4()
                            .py_2()
                            .border_b_1()
                            .border_color(border)
                            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child("Todos"))
                            .child(Button::new("todo-add-header").icon(IconName::Heading).ghost().small().tooltip("Add a header").on_click(cx.listener(|panel, _, window, cx| panel.compose(Target::Header, window, cx))))
                            .child(Button::new("todos-close").icon(IconName::Close).ghost().small().tooltip("Close (Esc)").on_click(cx.listener(|_, _, _, cx| cx.emit(TodoEvent::Close)))),
                    )
                    .child(div().flex_none().px_4().pt_3().pb_2().child(self.add_row(cx)))
                    .child(div().id("todo-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap(rems(0.125)).px_3().pb_3().children(rows))
                    .when(has_todos, |card| card.child(self.footer(cx))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i64, m: u32, d: u32) -> Date {
        Date::from_ymd(y, m, d).unwrap()
    }

    #[test]
    fn days_round_trip_through_the_calendar() {
        assert_eq!(day(1970, 1, 1), Date(0));
        assert_eq!(day(2026, 10, 26).ymd(), (2026, 10, 26));
        assert_eq!(day(2024, 2, 29).plus(1).ymd(), (2024, 3, 1));
        assert_eq!(Date::from_ymd(2026, 2, 29), None);
        // 2026-10-26 is a Monday.
        assert_eq!(day(2026, 10, 26).weekday(), 1);
    }

    #[test]
    fn due_dates_are_read_the_way_people_say_them() {
        let monday = day(2026, 10, 26);
        assert_eq!(parse_due("", monday), Some(None));
        assert_eq!(parse_due("today", monday), Some(Some(monday)));
        assert_eq!(parse_due("Tomorrow", monday), Some(Some(day(2026, 10, 27))));
        assert_eq!(parse_due("fri", monday), Some(Some(day(2026, 10, 30))));
        // The day it is now means next week's.
        assert_eq!(parse_due("monday", monday), Some(Some(day(2026, 11, 2))));
        assert_eq!(parse_due("next week", monday), Some(Some(day(2026, 11, 2))));
        assert_eq!(parse_due("+3d", monday), Some(Some(day(2026, 10, 29))));
        assert_eq!(parse_due("2w", monday), Some(Some(day(2026, 11, 9))));
        assert_eq!(parse_due("nov 2", monday), Some(Some(day(2026, 11, 2))));
        assert_eq!(parse_due("2 November", monday), Some(Some(day(2026, 11, 2))));
        // A day already gone this year is next year's.
        assert_eq!(parse_due("jan 5", monday), Some(Some(day(2027, 1, 5))));
        assert_eq!(parse_due("mar 1, 2028", monday), Some(Some(day(2028, 3, 1))));
        assert_eq!(parse_due("2026-12-31", monday), Some(Some(day(2026, 12, 31))));
        assert_eq!(parse_due("soon", monday), None);
        assert_eq!(parse_due("feb 30", monday), None);
    }

    #[test]
    fn near_dates_are_labelled_by_how_near() {
        let monday = day(2026, 10, 26);
        assert_eq!(due_label(monday, monday), "Today");
        assert_eq!(due_label(monday.plus(1), monday), "Tomorrow");
        assert_eq!(due_label(monday.plus(-1), monday), "Yesterday");
        assert_eq!(due_label(monday.plus(4), monday), "Fri");
        assert_eq!(due_label(day(2026, 11, 12), monday), "Nov 12");
        assert_eq!(due_label(day(2027, 1, 5), monday), "Jan 5 2027");
    }

    #[test]
    fn todos_nest_and_headers_group_them() {
        let mut todos = Todos::default();
        let work = todos.add(None, "Work".into(), None, true);
        let ship = todos.add(Some(work), "Ship".into(), None, false);
        let notes = todos.add(Some(ship), "Release notes".into(), None, false);
        todos.add(Some(ship), "Tag".into(), None, false);
        // A header is never put under anything.
        todos.add(Some(ship), "Home".into(), None, true);
        assert_eq!(todos.items.len(), 2);
        assert_eq!(todos.items[0].progress(), (0, 3));
        // Checking a todo off checks off what is under it; unchecking leaves them.
        todos.toggle(ship);
        assert_eq!(todos.items[0].progress(), (3, 3));
        todos.toggle(ship);
        assert_eq!(todos.items[0].progress(), (2, 3));
        assert_eq!(todos.counts(), (1, true));
        todos.toggle(notes);
        assert_eq!(todos.counts(), (2, true));
        todos.clear_done();
        assert_eq!(todos.items[0].progress(), (0, 2));
    }

    #[test]
    fn removing_a_header_keeps_its_todos_but_removing_a_todo_takes_its_subtasks() {
        let mut todos = Todos::default();
        let work = todos.add(None, "Work".into(), None, true);
        let ship = todos.add(Some(work), "Ship".into(), None, false);
        todos.add(Some(ship), "Tag".into(), None, false);
        todos.add(None, "Later".into(), None, false);
        todos.remove(work);
        assert_eq!(todos.items.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(), ["Later", "Ship"]);
        todos.remove(ship);
        assert_eq!(todos.items.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(), ["Later"]);
    }

    #[test]
    fn a_list_survives_its_file() {
        let mut todos = Todos::default();
        let work = todos.add(None, "Work".into(), None, true);
        let ship = todos.add(Some(work), "Ship".into(), Some(day(2026, 11, 2)), false);
        todos.add(Some(ship), "Tag".into(), None, false);
        todos.toggle(ship);
        assert_eq!(Todos::from_json(&todos.to_json()), todos);
    }

    #[test]
    fn only_todos_left_to_do_and_past_their_date_are_late() {
        let mut todos = Todos::default();
        let monday = day(2026, 10, 26);
        let a = todos.add(None, "a".into(), Some(monday.plus(-1)), false);
        todos.add(Some(a), "b".into(), Some(monday.plus(-3)), false);
        todos.add(None, "c".into(), Some(monday), false);
        assert_eq!(todos.late(monday), 2);
        todos.toggle(a);
        assert_eq!(todos.late(monday), 0);
    }

    #[test]
    fn a_todo_under_no_header_goes_before_the_headers() {
        let mut todos = Todos::default();
        let work = todos.add(None, "Work".into(), None, true);
        todos.add(Some(work), "Ship".into(), None, false);
        todos.add(None, "Milk".into(), None, false);
        todos.add(None, "Bread".into(), None, false);
        assert_eq!(todos.items.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(), ["Milk", "Bread", "Work"]);
        // A removed header's todos join the others under none.
        todos.remove(work);
        assert_eq!(todos.items.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(), ["Milk", "Bread", "Ship"]);
    }

    #[test]
    fn the_footer_filters_show_what_is_left_or_done_with_what_they_are_under() {
        let mut todos = Todos::default();
        let work = todos.add(None, "Work".into(), None, true);
        let ship = todos.add(Some(work), "Ship".into(), None, false);
        let tag = todos.add(Some(ship), "Tag".into(), None, false);
        let empty = todos.add(None, "Someday".into(), None, true);
        todos.toggle(tag);
        let header = |id| todos.items.iter().find(|item| item.id == id).unwrap().clone();
        let (work, empty) = (header(work), header(empty));
        assert!(work.shows(Show::Active) && work.shows(Show::Done));
        // Ship is still to do, but shows under Done for its done subtask.
        assert!(work.children[0].shows(Show::Done));
        assert!(!work.children[0].children[0].shows(Show::Active));
        // An empty header is only there with nothing filtered.
        assert!(empty.shows(Show::All) && !empty.shows(Show::Active));
    }
}
