//! The todos window, opened from the sidebar: a list of things to do, each with a date it is due by, nested under one
//! another and grouped under headers. There is one list for all windows, kept in `todos.json`.
use std::path::PathBuf;
use std::sync::LazyLock;

use futures::FutureExt as _;
use gpui::{Context, Entity, EventEmitter, Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px};
use nsh_platform_gpui::{AppView, Root};
use serde_json::{Value, json};
use tokio::sync::{Notify, watch};

use crate::theme::Palette;

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
        if !(1..=9999).contains(&year) || !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
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

/// A day of `month` with no year given: the first one from today on, which for Feb 29 may be years off.
fn coming(today: Date, month: u32, day: u32) -> Option<Date> {
    let year = today.ymd().0;
    (year..=year + 8).filter_map(|year| Date::from_ymd(year, month, day)).find(|date| *date >= today)
}

/// `number` days or weeks from today. A span of more than a century is a typo.
fn after(today: Date, number: &str, unit: &str) -> Option<Date> {
    let number: i64 = number.trim_start_matches('+').parse().ok().filter(|n| *n <= 36_500)?;
    match unit {
        "d" | "day" | "days" => Some(today.plus(number)),
        "w" | "wk" | "wks" | "week" | "weeks" => Some(today.plus(number * 7)),
        _ => None,
    }
}

fn is_span(unit: &str) -> bool {
    matches!(unit, "d" | "day" | "days" | "w" | "wk" | "wks" | "week" | "weeks")
}

/// What the user typed for a due date, read the way people say it: `today`, `tomorrow`, a weekday
/// (the next one), `next week` (its Monday), `+3d`, `2w` or `in 3 days` from today, `nov 2` or `2 nov`, or
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
            after(today, number, unit)
        }
        [number, unit] | ["in", number, unit] if is_span(unit) => after(today, number, unit),
        [first, second] | [first, second, _] => {
            let (name, day) = if first.chars().all(|c| c.is_ascii_digit()) { (second, first) } else { (first, second) };
            let (month, day) = (month(name)?, day.parse().ok()?);
            match words.get(2) {
                Some(year) => {
                    let year: i64 = year.parse().ok()?;
                    // Two digits are this century's.
                    Date::from_ymd(if year < 100 { 2000 + year } else { year }, month, day)
                }
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
    pub fn shows(&self, show: Show) -> bool {
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

#[derive(Clone, Default, Debug, PartialEq)]
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

/// Drops the done todos from `items`. A done todo with subtasks still to do stays, so they aren't
/// lost with it.
fn clear_done(items: &mut Vec<Item>) {
    items.iter_mut().for_each(|item| clear_done(&mut item.children));
    items.retain(|item| item.header || !item.done || !item.children.is_empty());
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

#[cfg(not(test))]
fn file() -> PathBuf {
    crate::config::data_dir().join("todos.json")
}

/// Tests keep their list out of the user's.
#[cfg(test)]
pub(crate) fn file() -> PathBuf {
    std::env::temp_dir().join(format!("po-test-{}", std::process::id())).join("todos.json")
}

/// The saved list. A file that can't be read is moved aside as `todos.json.bak` rather than
/// written over by the first change.
fn load() -> Todos {
    let target = file();
    let Ok(text) = std::fs::read_to_string(&target) else { return Todos::default() };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => Todos::from_json(&value),
        Err(error) => {
            log::error!("{}: {error}; kept as a .bak", target.display());
            if let Err(error) = std::fs::rename(&target, target.with_extension("json.bak")) {
                log::warn!("keeping {}: {error}", target.display());
            }
            Todos::default()
        }
    }
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
pub enum Target {
    Under(u64),
    Header,
    Edit(u64),
}

/// The list, shared by the window that edits it and the sidebar that counts it. Every change is
/// saved as it is made.
static STORE: LazyLock<watch::Sender<Todos>> = LazyLock::new(|| watch::Sender::new(load()));

/// The list now and after every change.
pub fn watch() -> watch::Receiver<Todos> {
    STORE.subscribe()
}

/// How many todos are left, and how many of those are late, for the sidebar's row.
pub fn summary() -> (usize, usize) {
    let todos = STORE.borrow();
    (todos.counts().0, todos.late(today()))
}

/// Changes the list with `f`, saves it, and tells whoever follows it.
pub fn change<R>(f: impl FnOnce(&mut Todos) -> R) -> R {
    let mut out = None;
    STORE.send_modify(|todos| out = Some(f(todos)));
    if let Err(error) = save(&STORE.borrow()) {
        log::warn!("saving {}: {error}", file().display());
    }
    out.expect("send_modify runs its closure")
}

/// What the window's screen asks of the frame around it: to close, and the height its content
/// needs, which the frame takes as far as the window has room.
pub(crate) static CLOSE: Notify = Notify::const_new();
pub(crate) static HEIGHT: LazyLock<watch::Sender<f32>> = LazyLock::new(|| watch::Sender::new(0.));

pub enum TodoEvent {
    Close,
    /// The list changed, which the sidebar's count follows.
    Changed,
}

/// The todos window, laid out like a plain todo app: a field to add one at the top, the list with
/// a box to check each off, and a footer with what is left and which to show. Its screen is NSH's
/// ([`crate::nsh::todos`]); this is the frame Po puts it in, over the app, and closes it from. The
/// sidebar only has a row that opens it.
pub struct TodoPanel {
    /// Made as the window opens and dropped as it closes, which stops its engine.
    view: Option<(Entity<AppView>, Subscription)>,
    height: f32,
    _follow: Task<()>,
}

impl EventEmitter<TodoEvent> for TodoPanel {}

impl TodoPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut todos = watch();
        let mut height = HEIGHT.subscribe();
        let follow = cx.spawn(async move |panel, cx| {
            loop {
                let event = futures::select_biased! {
                    _ = CLOSE.notified().fuse() => TodoEvent::Close,
                    changed = todos.changed().fuse() => match changed {
                        Ok(()) => TodoEvent::Changed,
                        Err(_) => return,
                    },
                    changed = height.changed().fuse() => match changed {
                        Ok(()) => {
                            let now = *height.borrow_and_update();
                            if panel.update(cx, |panel, cx| { panel.height = now; cx.notify() }).is_err() {
                                return;
                            }
                            continue;
                        }
                        Err(_) => return,
                    },
                };
                if panel.update(cx, |_, cx| cx.emit(event)).is_err() {
                    return;
                }
            }
        });
        Self { view: None, height: 0., _follow: follow }
    }

    /// Starts the screen and gives it the keyboard, as the window opens.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (view, theme) = crate::nsh::mount(Root::of::<crate::nsh::todos::Todos>(), window, cx);
        window.focus(&view.focus_handle(cx), cx);
        self.view = Some((view, theme));
        cx.notify();
    }

    /// Stops the screen, as the window closes.
    pub fn close(&mut self) {
        self.view = None;
    }
}

impl Render for TodoPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.global::<Palette>();
        let (popover, border, radius) = (colors.popover, colors.border, colors.radius);
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
                    // A click inside is the screen's, not the backdrop's.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .w(px(600.))
                    // Until the screen has measured itself, it takes the room it has.
                    .when(self.height > 0., |card| card.h(px(self.height + 2.)))
                    .when(self.height <= 0., |card| card.h_full())
                    .max_h_full()
                    .rounded(radius)
                    .border_1()
                    .border_color(border)
                    .bg(popover)
                    .overflow_hidden()
                    .children(self.view.as_ref().map(|(view, _)| view.clone())),
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
    fn spans_years_and_leap_days_are_read_sensibly() {
        let monday = day(2026, 10, 26);
        assert_eq!(parse_due("3 days", monday), Some(Some(day(2026, 10, 29))));
        assert_eq!(parse_due("in 2 weeks", monday), Some(Some(day(2026, 11, 9))));
        assert_eq!(parse_due("nov 2 27", monday), Some(Some(day(2027, 11, 2))));
        // Feb 29 waits for the next leap year, 2028.
        assert_eq!(parse_due("feb 29", monday), Some(Some(day(2028, 2, 29))));
        // A typo that would overflow is unreadable, not a panic.
        assert_eq!(parse_due("9000000000000000000d", monday), None);
        assert_eq!(parse_due("9000000000000000000w", monday), None);
        assert_eq!(parse_due("nov 2 99999999999999999", monday), None);
        assert_eq!(parse_due("99999999999999999-01-01", monday), None);
    }

    #[test]
    fn clearing_done_keeps_subtasks_still_to_do() {
        let mut todos = Todos::default();
        let ship = todos.add(None, "Ship".into(), None, false);
        todos.add(Some(ship), "Notes".into(), None, false);
        todos.add(None, "Milk".into(), None, false);
        let milk = todos.items[1].id;
        todos.toggle(milk);
        // Checked off with its subtask, then the subtask is brought back.
        todos.toggle(ship);
        let notes = todos.items[0].children[0].id;
        todos.toggle(notes);
        todos.clear_done();
        assert_eq!(todos.items.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(), ["Ship"]);
        assert_eq!(todos.items[0].children.len(), 1);
        // With nothing under it, a done todo goes.
        todos.toggle(notes);
        todos.clear_done();
        assert!(todos.items.is_empty());
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
