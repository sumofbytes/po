//! Issues and epics: a small tracker that lives in the repo, under `.po/`. This is the model, with
//! no I/O: [`Tracker`] holds what a repo has and answers questions about it, and [`store`] reads
//! and writes the files; [`api`] is what agents ask, [`shared`] the copy the app follows.
pub mod api;
pub mod import;
pub mod query;
pub mod shared;
pub mod store;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use crate::date::Date;

/// Where an issue is, in the order work moves through them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Status {
    Backlog,
    Todo,
    InProgress,
    InReview,
    Done,
    Canceled,
}

/// What a status means, which is all that progress and the late count look at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Unstarted,
    Started,
    Done,
    Canceled,
}

impl Status {
    pub const ALL: [Status; 6] = [Status::Backlog, Status::Todo, Status::InProgress, Status::InReview, Status::Done, Status::Canceled];

    /// As the files spell it.
    pub fn slug(self) -> &'static str {
        match self {
            Status::Backlog => "backlog",
            Status::Todo => "todo",
            Status::InProgress => "in-progress",
            Status::InReview => "in-review",
            Status::Done => "done",
            Status::Canceled => "canceled",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Status::Backlog => "Backlog",
            Status::Todo => "Todo",
            Status::InProgress => "In Progress",
            Status::InReview => "In Review",
            Status::Done => "Done",
            Status::Canceled => "Canceled",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.slug() == slug)
    }

    pub fn category(self) -> Category {
        match self {
            Status::Backlog | Status::Todo => Category::Unstarted,
            Status::InProgress | Status::InReview => Category::Started,
            Status::Done => Category::Done,
            Status::Canceled => Category::Canceled,
        }
    }

    /// Still to be done: neither finished nor dropped.
    pub fn is_open(self) -> bool {
        matches!(self.category(), Category::Unstarted | Category::Started)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Priority {
    None,
    Urgent,
    High,
    Medium,
    Low,
}

impl Priority {
    pub const ALL: [Priority; 5] = [Priority::None, Priority::Urgent, Priority::High, Priority::Medium, Priority::Low];

    pub fn slug(self) -> &'static str {
        match self {
            Priority::None => "none",
            Priority::Urgent => "urgent",
            Priority::High => "high",
            Priority::Medium => "medium",
            Priority::Low => "low",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Priority::None => "No priority",
            Priority::Urgent => "Urgent",
            Priority::High => "High",
            Priority::Medium => "Medium",
            Priority::Low => "Low",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|priority| priority.slug() == slug)
    }

    /// For sorting: urgent first, no priority last.
    pub fn rank(self) -> u8 {
        match self {
            Priority::Urgent => 0,
            Priority::High => 1,
            Priority::Medium => 2,
            Priority::Low => 3,
            Priority::None => 4,
        }
    }
}

/// Something an issue is tied to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Link {
    /// A conversation, by its session id.
    Thread(String),
    Branch(String),
    Pr(u64),
}

impl Link {
    fn to_json(&self) -> Value {
        match self {
            Link::Thread(session) => json!({ "thread": session }),
            Link::Branch(name) => json!({ "branch": name }),
            Link::Pr(number) => json!({ "pr": number }),
        }
    }

    pub(crate) fn from_json(value: &Value) -> Option<Self> {
        if let Some(session) = value["thread"].as_str() {
            Some(Link::Thread(session.to_string()))
        } else if let Some(name) = value["branch"].as_str() {
            Some(Link::Branch(name.to_string()))
        } else {
            value["pr"].as_u64().map(Link::Pr)
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Issue {
    /// The number in its key, `PO-12`. It is the file's name, so it is not kept in the file.
    pub number: u64,
    pub title: String,
    /// Markdown.
    pub description: String,
    pub status: Status,
    pub priority: Priority,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    /// The issue this is a sub-issue of.
    pub parent: Option<u64>,
    /// An [`Epic`]'s id, and one of its milestones' ids.
    pub epic: Option<String>,
    pub milestone: Option<String>,
    pub due: Option<Date>,
    pub estimate: Option<u32>,
    /// Milliseconds since the epoch.
    pub created: u64,
    pub updated: u64,
    pub links: Vec<Link>,
}

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|text| !text.is_empty()).map(str::to_string)
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().into_iter().flatten().filter_map(text).collect()
}

impl Issue {
    fn new(number: u64, title: String, now: u64) -> Self {
        Self {
            number,
            title,
            description: String::new(),
            status: Status::Backlog,
            priority: Priority::None,
            labels: vec![],
            assignee: None,
            parent: None,
            epic: None,
            milestone: None,
            due: None,
            estimate: None,
            created: now,
            updated: now,
            links: vec![],
        }
    }

    /// What goes in the file. What is empty is left out and the keys come in a fixed order, so a
    /// change to one field is a one-line diff.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("version".into(), json!(1));
        out.insert("title".into(), json!(self.title));
        out.insert("status".into(), json!(self.status.slug()));
        out.insert("created".into(), json!(self.created));
        out.insert("updated".into(), json!(self.updated));
        if !self.description.is_empty() {
            out.insert("description".into(), json!(self.description));
        }
        if self.priority != Priority::None {
            out.insert("priority".into(), json!(self.priority.slug()));
        }
        if !self.labels.is_empty() {
            out.insert("labels".into(), json!(self.labels));
        }
        if !self.links.is_empty() {
            out.insert("links".into(), Value::Array(self.links.iter().map(Link::to_json).collect()));
        }
        for (key, value) in [("assignee", &self.assignee), ("epic", &self.epic), ("milestone", &self.milestone)] {
            if let Some(value) = value {
                out.insert(key.into(), json!(value));
            }
        }
        if let Some(parent) = self.parent {
            out.insert("parent".into(), json!(parent));
        }
        if let Some(due) = self.due {
            out.insert("due".into(), json!(due.iso()));
        }
        if let Some(estimate) = self.estimate {
            out.insert("estimate".into(), json!(estimate));
        }
        Value::Object(out)
    }

    /// The issue a file holds as number `number`. Only a title is needed; what is missing or not
    /// understood takes its default.
    pub fn from_json(number: u64, value: &Value) -> Option<Self> {
        Some(Self {
            number,
            title: value["title"].as_str()?.to_string(),
            description: value["description"].as_str().unwrap_or_default().to_string(),
            status: value["status"].as_str().and_then(Status::from_slug).unwrap_or(Status::Backlog),
            priority: value["priority"].as_str().and_then(Priority::from_slug).unwrap_or(Priority::None),
            labels: strings(&value["labels"]),
            assignee: text(&value["assignee"]),
            parent: value["parent"].as_u64(),
            epic: text(&value["epic"]),
            milestone: text(&value["milestone"]),
            due: value["due"].as_str().and_then(Date::from_iso),
            estimate: value["estimate"].as_u64().and_then(|n| u32::try_from(n).ok()),
            created: value["created"].as_u64().unwrap_or(0),
            updated: value["updated"].as_u64().unwrap_or(0),
            links: value["links"].as_array().into_iter().flatten().filter_map(Link::from_json).collect(),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EpicStatus {
    Planned,
    Active,
    Paused,
    Done,
}

impl EpicStatus {
    pub const ALL: [EpicStatus; 4] = [EpicStatus::Planned, EpicStatus::Active, EpicStatus::Paused, EpicStatus::Done];

    pub fn slug(self) -> &'static str {
        match self {
            EpicStatus::Planned => "planned",
            EpicStatus::Active => "active",
            EpicStatus::Paused => "paused",
            EpicStatus::Done => "done",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            EpicStatus::Planned => "Planned",
            EpicStatus::Active => "Active",
            EpicStatus::Paused => "Paused",
            EpicStatus::Done => "Done",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.slug() == slug)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Milestone {
    /// Unique within its epic.
    pub id: String,
    pub name: String,
    pub target: Option<Date>,
}

/// A body of work that issues belong to: what Linear calls a project.
#[derive(Clone, PartialEq, Debug)]
pub struct Epic {
    /// Its file's name: lowercase letters, digits and dashes.
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: EpicStatus,
    pub lead: Option<String>,
    pub start: Option<Date>,
    pub target: Option<Date>,
    pub milestones: Vec<Milestone>,
    pub created: u64,
    pub updated: u64,
}

/// `Launch v2!` as `launch-v2`, or `fallback` for a name with nothing to go on.
pub fn slug(name: &str, fallback: &str) -> String {
    let words: Vec<String> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if words.is_empty() { fallback.to_string() } else { words.join("-") }
}

/// Whether `id` is something that can be a file's name here.
pub(crate) fn is_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `base`, or `base-2`, `base-3`… for the first that `taken` does not hold.
fn unique(base: String, taken: impl Fn(&str) -> bool) -> String {
    if !taken(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|id| !taken(id)).expect("an unbounded range")
}

impl Epic {
    fn new(id: String, name: String, now: u64) -> Self {
        Self {
            id,
            name,
            description: String::new(),
            status: EpicStatus::Planned,
            lead: None,
            start: None,
            target: None,
            milestones: vec![],
            created: now,
            updated: now,
        }
    }

    /// Adds a milestone at the end, and returns its id.
    pub fn add_milestone(&mut self, name: &str, target: Option<Date>) -> String {
        let id = unique(slug(name, "milestone"), |id| self.milestones.iter().any(|m| m.id == id));
        self.milestones.push(Milestone { id: id.clone(), name: name.to_string(), target });
        id
    }

    pub fn milestone(&self, id: &str) -> Option<&Milestone> {
        self.milestones.iter().find(|m| m.id == id)
    }

    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("version".into(), json!(1));
        out.insert("name".into(), json!(self.name));
        out.insert("status".into(), json!(self.status.slug()));
        out.insert("created".into(), json!(self.created));
        out.insert("updated".into(), json!(self.updated));
        if !self.description.is_empty() {
            out.insert("description".into(), json!(self.description));
        }
        if let Some(lead) = &self.lead {
            out.insert("lead".into(), json!(lead));
        }
        for (key, date) in [("start", self.start), ("target", self.target)] {
            if let Some(date) = date {
                out.insert(key.into(), json!(date.iso()));
            }
        }
        if !self.milestones.is_empty() {
            let milestones = self.milestones.iter().map(|m| {
                let mut out = Map::new();
                out.insert("id".into(), json!(m.id));
                out.insert("name".into(), json!(m.name));
                if let Some(target) = m.target {
                    out.insert("target".into(), json!(target.iso()));
                }
                Value::Object(out)
            });
            out.insert("milestones".into(), Value::Array(milestones.collect()));
        }
        Value::Object(out)
    }

    pub fn from_json(id: String, value: &Value) -> Option<Self> {
        let milestones = value["milestones"].as_array().into_iter().flatten().filter_map(|m| {
            let id = text(&m["id"]).filter(|id| is_id(id))?;
            Some(Milestone { id, name: m["name"].as_str().unwrap_or_default().to_string(), target: m["target"].as_str().and_then(Date::from_iso) })
        });
        Some(Self {
            id,
            name: value["name"].as_str()?.to_string(),
            description: value["description"].as_str().unwrap_or_default().to_string(),
            status: value["status"].as_str().and_then(EpicStatus::from_slug).unwrap_or(EpicStatus::Planned),
            lead: text(&value["lead"]),
            start: value["start"].as_str().and_then(Date::from_iso),
            target: value["target"].as_str().and_then(Date::from_iso),
            milestones: milestones.collect(),
            created: value["created"].as_u64().unwrap_or(0),
            updated: value["updated"].as_u64().unwrap_or(0),
        })
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Label {
    pub name: String,
    /// `#rrggbb`, or empty for the default.
    pub color: String,
}

/// `.po/config.json`: what the repo's issues share.
#[derive(Clone, PartialEq, Debug)]
pub struct Config {
    /// The letters in a key: `PO` in `PO-12`.
    pub prefix: String,
    /// The number the next issue gets.
    pub next: u64,
    pub labels: Vec<Label>,
}

/// Keys for a repo called `name`: its initials, or the start of its name if it is one word.
pub fn default_prefix(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|word| !word.is_empty()).collect();
    let prefix: String = match words.as_slice() {
        [] => String::new(),
        [one] => one.chars().take(3).collect(),
        many => many.iter().filter_map(|word| word.chars().next()).take(4).collect(),
    };
    if prefix.is_empty() { "ISS".into() } else { prefix.to_ascii_uppercase() }
}

impl Config {
    pub fn new(prefix: String) -> Self {
        Self { prefix, next: 1, labels: vec![] }
    }

    pub fn to_json(&self) -> Value {
        let labels = self.labels.iter().map(|label| {
            let mut out = Map::new();
            out.insert("name".into(), json!(label.name));
            if !label.color.is_empty() {
                out.insert("color".into(), json!(label.color));
            }
            Value::Object(out)
        });
        json!({ "version": 1, "prefix": self.prefix, "next": self.next, "labels": labels.collect::<Vec<_>>() })
    }

    /// The config a file holds, with `fallback`'s prefix if it has none that will do as one.
    pub fn from_json(value: &Value, fallback: &str) -> Self {
        let prefix = value["prefix"].as_str().filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()));
        Self {
            prefix: prefix.unwrap_or(fallback).to_string(),
            next: value["next"].as_u64().unwrap_or(1).max(1),
            labels: value["labels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|label| Some(Label { name: text(&label["name"])?, color: label["color"].as_str().unwrap_or_default().to_string() }))
                .collect(),
        }
    }
}

/// How much of a body of issues is done. Canceled ones are not counted at all.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Progress {
    pub done: usize,
    /// In progress or in review.
    pub started: usize,
    pub all: usize,
}

impl Progress {
    fn of<'a>(issues: impl Iterator<Item = &'a Issue>) -> Self {
        issues.fold(Self::default(), |mut progress, issue| {
            match issue.status.category() {
                Category::Canceled => return progress,
                Category::Done => progress.done += 1,
                Category::Started => progress.started += 1,
                Category::Unstarted => {}
            }
            progress.all += 1;
            progress
        })
    }
}

/// What changed since the store last wrote, so it writes only that.
#[derive(Default, Clone, PartialEq, Debug)]
pub(crate) struct Dirty {
    pub config: bool,
    pub issues: BTreeSet<u64>,
    /// Issues made since the last write, whose files must not be there already: another window
    /// may have used the number.
    pub created: BTreeSet<u64>,
    pub removed: BTreeSet<u64>,
    pub epics: BTreeSet<String>,
    pub removed_epics: BTreeSet<String>,
}

impl Dirty {
    pub fn is_empty(&self) -> bool {
        !self.config && self.issues.is_empty() && self.created.is_empty() && self.removed.is_empty() && self.epics.is_empty() && self.removed_epics.is_empty()
    }
}

/// A repo's issues and epics. Every change goes through a method here, which keeps what refers to
/// what consistent and notes what the store must write; nothing is written until it does.
#[derive(Clone, PartialEq, Debug)]
pub struct Tracker {
    pub config: Config,
    issues: BTreeMap<u64, Issue>,
    epics: Vec<Epic>,
    pub(crate) dirty: Dirty,
}

impl Tracker {
    pub fn new(config: Config) -> Self {
        Self { config, issues: BTreeMap::new(), epics: vec![], dirty: Dirty::default() }
    }

    /// A tracker over what was read from files. Anything a merge or a hand edit left inconsistent
    /// is mended in memory, and told in the returned list; it is written only when it changes.
    pub fn from_parts(mut config: Config, issues: Vec<Issue>, mut epics: Vec<Epic>) -> (Self, Vec<String>) {
        let mut problems = vec![];
        let issues: BTreeMap<u64, Issue> = issues.into_iter().map(|issue| (issue.number, issue)).collect();
        epics.sort_by(|a, b| (a.created, &a.id).cmp(&(b.created, &b.id)));
        let mut dirty = Dirty::default();
        // Another branch may have used numbers this one has not seen yet.
        if let Some(highest) = issues.keys().next_back().filter(|highest| **highest >= config.next) {
            problems.push(format!("next number was {}, but {}-{highest} exists; now {}", config.next, config.prefix, highest + 1));
            config.next = highest + 1;
            dirty.config = true;
        }
        let mut tracker = Self { config, issues, epics, dirty };
        let numbers: Vec<u64> = tracker.issues.keys().copied().collect();
        for number in numbers {
            let parent = tracker.issues[&number].parent;
            if parent.is_some_and(|parent| parent == number || !tracker.issues.contains_key(&parent) || tracker.is_under(parent, number)) {
                problems.push(format!("{}: parent {} is missing or loops back; made a top-level issue", tracker.key(number), parent.unwrap_or(0)));
                tracker.issues.get_mut(&number).expect("listed above").parent = None;
            }
        }
        (tracker, problems)
    }

    pub fn key(&self, number: u64) -> String {
        format!("{}-{number}", self.config.prefix)
    }

    /// The number in `PO-12`, `po-12` or `12`.
    pub fn parse_key(&self, text: &str) -> Option<u64> {
        let text = text.trim();
        let number = match text.rsplit_once('-') {
            Some((prefix, number)) if prefix.eq_ignore_ascii_case(&self.config.prefix) => number,
            Some(_) => return None,
            None => text,
        };
        number.parse().ok().filter(|number| *number > 0)
    }

    pub fn issues(&self) -> impl Iterator<Item = &Issue> {
        self.issues.values()
    }

    pub fn issue(&self, number: u64) -> Option<&Issue> {
        self.issues.get(&number)
    }

    pub fn epics(&self) -> &[Epic] {
        &self.epics
    }

    pub fn epic(&self, id: &str) -> Option<&Epic> {
        self.epics.iter().find(|epic| epic.id == id)
    }

    /// The sub-issues of `number`, by number.
    pub fn children(&self, number: u64) -> impl Iterator<Item = &Issue> {
        self.issues.values().filter(move |issue| issue.parent == Some(number))
    }

    /// Everything under `number`, at any depth.
    pub fn descendants(&self, number: u64) -> Vec<u64> {
        let mut found = vec![];
        let mut queue = vec![number];
        while let Some(at) = queue.pop() {
            for child in self.children(at) {
                if !found.contains(&child.number) {
                    found.push(child.number);
                    queue.push(child.number);
                }
            }
        }
        found.sort_unstable();
        found
    }

    /// Whether making `parent` the parent of `number` would make a loop.
    pub fn would_loop(&self, number: u64, parent: u64) -> bool {
        parent == number || self.is_under(parent, number)
    }

    /// Whether `ancestor` is `number`'s parent, or its parent's, and so on.
    fn is_under(&self, number: u64, ancestor: u64) -> bool {
        let mut at = number;
        // A loop in bad data ends at the number of issues there are.
        for _ in 0..=self.issues.len() {
            if at == ancestor {
                return true;
            }
            match self.issues.get(&at).and_then(|issue| issue.parent) {
                Some(parent) => at = parent,
                None => return false,
            }
        }
        false
    }

    /// The sub-issues of `number` at every depth, done and in all.
    pub fn progress(&self, number: u64) -> Progress {
        Progress::of(self.descendants(number).iter().filter_map(|n| self.issues.get(n)))
    }

    pub fn epic_progress(&self, epic: &str) -> Progress {
        Progress::of(self.issues.values().filter(|issue| issue.epic.as_deref() == Some(epic)))
    }

    pub fn milestone_progress(&self, epic: &str, milestone: &str) -> Progress {
        Progress::of(self.issues.values().filter(|issue| issue.epic.as_deref() == Some(epic) && issue.milestone.as_deref() == Some(milestone)))
    }

    /// How many issues are neither done nor canceled.
    pub fn open_count(&self) -> usize {
        self.issues.values().filter(|issue| issue.status.is_open()).count()
    }

    /// How many open issues were due before `today`.
    pub fn late(&self, today: Date) -> usize {
        self.issues.values().filter(|issue| issue.status.is_open() && issue.due.is_some_and(|due| due < today)).count()
    }

    /// Adds an issue to the backlog, and returns its number.
    pub fn create(&mut self, title: &str, now: u64) -> u64 {
        let number = self.config.next;
        self.config.next += 1;
        self.issues.insert(number, Issue::new(number, title.trim().to_string(), now));
        self.dirty.config = true;
        self.dirty.issues.insert(number);
        self.dirty.created.insert(number);
        number
    }

    /// Gives a made issue that has not been written a new number, from `floor` or the counter,
    /// whichever is higher, with its sub-issues following it. For when its number turns out to be
    /// taken on disk by an issue this tracker has not seen.
    pub(crate) fn renumber(&mut self, old: u64, floor: u64) -> u64 {
        let new = self.config.next.max(floor);
        self.config.next = new + 1;
        self.dirty.config = true;
        let Some(mut issue) = self.issues.remove(&old) else { return old };
        issue.number = new;
        self.issues.insert(new, issue);
        let kids: Vec<u64> = self.children(old).map(|child| child.number).collect();
        for kid in kids {
            if let Some(kid) = self.issues.get_mut(&kid) {
                kid.parent = Some(new);
            }
            self.dirty.issues.insert(kid);
        }
        self.dirty.issues.remove(&old);
        self.dirty.created.remove(&old);
        self.dirty.issues.insert(new);
        self.dirty.created.insert(new);
        new
    }

    /// An epic and milestone that exist, and the milestone in that epic: what an issue may refer to.
    fn mend_epic(&self, epic: &mut Option<String>, milestone: &mut Option<String>) {
        let found = epic.as_deref().and_then(|id| self.epic(id));
        if found.is_none() {
            *epic = None;
        }
        if milestone.as_deref().is_some_and(|id| found.is_none_or(|epic| epic.milestone(id).is_none())) {
            *milestone = None;
        }
    }

    /// Lets `change` edit an issue, and keeps the result if it differs, with `now` as its time of
    /// update. A parent that doesn't exist or would make a loop, and an epic or milestone that
    /// doesn't exist, are put back as they were. Returns whether anything changed.
    pub fn update(&mut self, number: u64, now: u64, change: impl FnOnce(&mut Issue)) -> bool {
        let Some(old) = self.issues.get(&number) else { return false };
        let mut new = old.clone();
        change(&mut new);
        new.number = number;
        new.created = old.created;
        new.updated = old.updated;
        new.title = new.title.trim().to_string();
        let mut seen = BTreeSet::new();
        new.labels.retain(|label| !label.is_empty() && seen.insert(label.clone()));
        if new.parent != old.parent && new.parent.is_some_and(|parent| !self.issues.contains_key(&parent) || self.is_under(parent, number)) {
            new.parent = old.parent;
        }
        let (mut epic, mut milestone) = (new.epic.take(), new.milestone.take());
        self.mend_epic(&mut epic, &mut milestone);
        (new.epic, new.milestone) = (epic, milestone);
        if new == *old {
            return false;
        }
        new.updated = now;
        self.issues.insert(number, new);
        self.dirty.issues.insert(number);
        true
    }

    /// Deletes an issue. Its sub-issues stay, as top-level ones.
    pub fn remove(&mut self, number: u64, now: u64) {
        if self.issues.remove(&number).is_none() {
            return;
        }
        self.dirty.issues.remove(&number);
        // One never written has no file of its own to delete: a file of that name is another
        // window's.
        if !self.dirty.created.remove(&number) {
            self.dirty.removed.insert(number);
        }
        let orphans: Vec<u64> = self.children(number).map(|issue| issue.number).collect();
        for child in orphans {
            self.update(child, now, |issue| issue.parent = None);
        }
    }

    /// Adds an epic, and returns its id.
    pub fn create_epic(&mut self, name: &str, now: u64) -> String {
        let id = unique(slug(name, "epic"), |id| self.epic(id).is_some());
        self.epics.push(Epic::new(id.clone(), name.trim().to_string(), now));
        self.dirty.epics.insert(id.clone());
        id
    }

    /// Lets `change` edit an epic, as [`Tracker::update`] does an issue. A milestone it takes away
    /// is taken off the issues that were in it.
    pub fn update_epic(&mut self, id: &str, now: u64, change: impl FnOnce(&mut Epic)) -> bool {
        let Some(old) = self.epic(id) else { return false };
        let mut new = old.clone();
        change(&mut new);
        new.id = old.id.clone();
        new.created = old.created;
        new.updated = old.updated;
        new.name = new.name.trim().to_string();
        let mut seen = BTreeSet::new();
        new.milestones.retain(|m| is_id(&m.id) && seen.insert(m.id.clone()));
        if new == *old {
            return false;
        }
        new.updated = now;
        // Only a milestone that went can leave an issue pointing at nothing; looking at every
        // issue for each letter typed in an epic's name would be wasted work.
        let lost_milestone = old.milestones.iter().any(|before| new.milestones.iter().all(|after| after.id != before.id));
        let at = self.epics.iter().position(|epic| epic.id == id).expect("found above");
        self.epics[at] = new;
        self.dirty.epics.insert(id.to_string());
        if lost_milestone {
            self.mend_issues(now);
        }
        true
    }

    /// Deletes an epic, and takes its issues out of it.
    pub fn remove_epic(&mut self, id: &str, now: u64) {
        let Some(at) = self.epics.iter().position(|epic| epic.id == id) else { return };
        self.epics.remove(at);
        self.dirty.epics.remove(id);
        self.dirty.removed_epics.insert(id.to_string());
        self.mend_issues(now);
    }

    /// Takes the epic or milestone off every issue that names one that is gone.
    fn mend_issues(&mut self, now: u64) {
        let numbers: Vec<u64> = self.issues.keys().copied().collect();
        for number in numbers {
            // An update mends what an issue refers to, so changing nothing is enough.
            self.update(number, now, |_| {});
        }
    }

    /// Adds a label, or sets the color of the one by that name.
    pub fn define_label(&mut self, name: &str, color: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        match self.config.labels.iter_mut().find(|label| label.name == name) {
            Some(label) if label.color == color => return,
            Some(label) => label.color = color.to_string(),
            None => self.config.labels.push(Label { name: name.to_string(), color: color.to_string() }),
        }
        self.dirty.config = true;
    }

    /// Deletes a label, and takes it off the issues that have it.
    pub fn remove_label(&mut self, name: &str, now: u64) {
        let before = self.config.labels.len();
        self.config.labels.retain(|label| label.name != name);
        self.dirty.config |= self.config.labels.len() != before;
        let tagged: Vec<u64> = self.issues.values().filter(|issue| issue.labels.iter().any(|label| label == name)).map(|issue| issue.number).collect();
        for number in tagged {
            self.update(number, now, |issue| issue.labels.retain(|label| label != name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i64, m: u32, d: u32) -> Date {
        Date::from_ymd(y, m, d).unwrap()
    }

    fn tracker() -> Tracker {
        Tracker::new(Config::new("PO".into()))
    }

    #[test]
    fn statuses_and_priorities_round_trip_through_their_slugs() {
        for status in Status::ALL {
            assert_eq!(Status::from_slug(status.slug()), Some(status));
        }
        for priority in Priority::ALL {
            assert_eq!(Priority::from_slug(priority.slug()), Some(priority));
        }
        assert_eq!(Status::from_slug("shipped"), None);
        assert_eq!(Status::InReview.category(), Category::Started);
        assert!(Status::Todo.is_open() && !Status::Done.is_open() && !Status::Canceled.is_open());
        assert!(Priority::Urgent.rank() < Priority::Low.rank() && Priority::Low.rank() < Priority::None.rank());
    }

    #[test]
    fn issues_are_numbered_in_order_and_keys_read_back() {
        let mut t = tracker();
        assert_eq!((t.create("a", 1), t.create(" b ", 2)), (1, 2));
        assert_eq!(t.issue(2).unwrap().title, "b");
        assert_eq!(t.config.next, 3);
        assert_eq!(t.key(2), "PO-2");
        assert_eq!(t.parse_key("PO-12"), Some(12));
        assert_eq!(t.parse_key(" po-12 "), Some(12));
        assert_eq!(t.parse_key("12"), Some(12));
        assert_eq!(t.parse_key("XX-12"), None);
        assert_eq!(t.parse_key("PO-0"), None);
        // A number once used is not handed out again after the issue goes.
        t.remove(2, 3);
        assert_eq!(t.create("c", 4), 3);
    }

    #[test]
    fn an_update_that_changes_nothing_writes_nothing() {
        let mut t = tracker();
        let a = t.create("a", 1);
        t.dirty = Dirty::default();
        assert!(!t.update(a, 5, |issue| issue.title = "a".into()));
        assert!(t.update(a, 5, |issue| issue.labels = vec!["x".into(), "x".into(), String::new()]));
        assert_eq!(t.issue(a).unwrap().labels, ["x"]);
        t.dirty = Dirty::default();
        assert!(t.update(a, 9, |issue| issue.status = Status::Todo));
        assert_eq!(t.issue(a).unwrap().updated, 9);
        assert_eq!(t.issue(a).unwrap().created, 1);
        assert_eq!(t.dirty.issues.iter().collect::<Vec<_>>(), [&a]);
        // The number and creation time are not the caller's to change.
        t.update(a, 10, |issue| (issue.number, issue.created, issue.priority) = (99, 99, Priority::High));
        assert_eq!((t.issue(a).unwrap().number, t.issue(a).unwrap().created), (a, 1));
    }

    #[test]
    fn sub_issues_cannot_loop_and_outlive_their_parent() {
        let mut t = tracker();
        let (a, b, c) = (t.create("a", 1), t.create("b", 1), t.create("c", 1));
        assert!(t.update(b, 2, |issue| issue.parent = Some(a)));
        assert!(t.update(c, 2, |issue| issue.parent = Some(b)));
        // a under c would loop; a under itself, or under nothing, is refused too.
        assert!(!t.update(a, 3, |issue| issue.parent = Some(c)));
        assert!(!t.update(a, 3, |issue| issue.parent = Some(a)));
        assert!(!t.update(a, 3, |issue| issue.parent = Some(77)));
        assert_eq!(t.descendants(a), [b, c]);
        t.update(c, 4, |issue| issue.status = Status::Done);
        t.update(b, 4, |issue| issue.status = Status::InProgress);
        assert_eq!(t.progress(a), Progress { done: 1, started: 1, all: 2 });
        // As if all were written, so the deletion has a file to delete.
        t.dirty = Dirty::default();
        t.remove(a, 5);
        assert_eq!(t.issue(b).unwrap().parent, None);
        assert_eq!(t.issue(c).unwrap().parent, Some(b));
        assert!(t.dirty.removed.contains(&a) && t.dirty.issues.contains(&b));
    }

    #[test]
    fn canceled_issues_are_not_counted_in_progress() {
        let mut t = tracker();
        let e = t.create_epic("Launch", 1);
        let numbers: Vec<u64> = (0..4).map(|n| t.create(&format!("i{n}"), 1)).collect();
        for n in &numbers {
            t.update(*n, 2, |issue| issue.epic = Some(e.clone()));
        }
        t.update(numbers[0], 3, |issue| issue.status = Status::Done);
        t.update(numbers[1], 3, |issue| issue.status = Status::Canceled);
        t.update(numbers[2], 3, |issue| issue.status = Status::InReview);
        let progress = t.epic_progress(&e);
        assert_eq!(progress, Progress { done: 1, started: 1, all: 3 });
    }

    #[test]
    fn epics_get_unique_ids_and_milestones_belong_to_one() {
        let mut t = tracker();
        let (a, b) = (t.create_epic("Launch v2!", 1), t.create_epic("launch v2", 2));
        assert_eq!((a.as_str(), b.as_str()), ("launch-v2", "launch-v2-2"));
        assert_eq!(t.create_epic("!!!", 3), "epic");
        let mut beta = String::new();
        t.update_epic(&a, 4, |epic| {
            beta = epic.add_milestone("Beta", Some(day(2026, 12, 1)));
            assert_eq!(epic.add_milestone("Beta", None), "beta-2");
        });
        assert_eq!(beta, "beta");
        let n = t.create("x", 5);
        // A milestone must be of the issue's own epic, and an epic must exist.
        t.update(n, 6, |issue| (issue.epic, issue.milestone) = (Some(b.clone()), Some(beta.clone())));
        assert_eq!((t.issue(n).unwrap().epic.as_ref(), t.issue(n).unwrap().milestone.as_ref()), (Some(&b), None));
        t.update(n, 7, |issue| (issue.epic, issue.milestone) = (Some(a.clone()), Some(beta.clone())));
        assert_eq!(t.milestone_progress(&a, &beta).all, 1);
        t.update(n, 8, |issue| issue.epic = Some("nope".into()));
        assert_eq!((t.issue(n).unwrap().epic.as_ref(), t.issue(n).unwrap().milestone.as_ref()), (None, None));
    }

    #[test]
    fn removing_a_milestone_or_an_epic_takes_its_issues_out_of_it() {
        let mut t = tracker();
        let e = t.create_epic("Launch", 1);
        let mut beta = String::new();
        t.update_epic(&e, 2, |epic| beta = epic.add_milestone("Beta", None));
        let n = t.create("x", 3);
        t.update(n, 4, |issue| (issue.epic, issue.milestone) = (Some(e.clone()), Some(beta.clone())));
        t.update_epic(&e, 5, |epic| epic.milestones.clear());
        let issue = t.issue(n).unwrap();
        assert_eq!((issue.epic.as_deref(), issue.milestone.as_deref()), (Some("launch"), None));
        assert_eq!(issue.updated, 5);
        t.remove_epic(&e, 6);
        assert_eq!(t.issue(n).unwrap().epic, None);
        assert!(t.epics().is_empty() && t.dirty.removed_epics.contains("launch"));
    }

    #[test]
    fn labels_are_defined_once_and_removed_from_issues() {
        let mut t = tracker();
        let n = t.create("x", 1);
        t.define_label("bug", "#e5484d");
        t.define_label("bug", "#e5484d");
        t.define_label("  ", "");
        assert_eq!(t.config.labels.len(), 1);
        t.update(n, 2, |issue| issue.labels = vec!["bug".into(), "ui".into()]);
        t.remove_label("bug", 3);
        assert!(t.config.labels.is_empty());
        assert_eq!(t.issue(n).unwrap().labels, ["ui"]);
    }

    #[test]
    fn only_open_issues_past_their_date_are_late() {
        let mut t = tracker();
        let today = day(2026, 10, 28);
        let (a, b, c) = (t.create("a", 1), t.create("b", 1), t.create("c", 1));
        for n in [a, b] {
            t.update(n, 2, |issue| issue.due = Some(today.plus(-1)));
        }
        t.update(c, 2, |issue| issue.due = Some(today));
        assert_eq!((t.late(today), t.open_count()), (2, 3));
        t.update(a, 3, |issue| issue.status = Status::Done);
        assert_eq!((t.late(today), t.open_count()), (1, 2));
    }

    #[test]
    fn an_issue_survives_its_json_and_leaves_out_what_is_empty() {
        let mut issue = Issue::new(12, "Fix it".into(), 5);
        let mut keys: Vec<_> = issue.to_json().as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["created", "status", "title", "updated", "version"]);
        assert_eq!(Issue::from_json(12, &issue.to_json()), Some(issue.clone()));
        issue.description = "Because.".into();
        issue.status = Status::InReview;
        issue.priority = Priority::High;
        issue.labels = vec!["bug".into()];
        issue.assignee = Some("arpit".into());
        issue.parent = Some(3);
        issue.epic = Some("launch".into());
        issue.milestone = Some("beta".into());
        issue.due = Some(day(2026, 11, 2));
        issue.estimate = Some(3);
        issue.links = vec![Link::Thread("s-1".into()), Link::Branch("po-12-fix-it".into()), Link::Pr(40)];
        issue.updated = 9;
        assert_eq!(Issue::from_json(12, &issue.to_json()), Some(issue));
        // Only a title is needed; what is not understood takes its default.
        let odd = Issue::from_json(1, &json!({ "title": "t", "status": "shipped", "due": "soon", "links": [{ "x": 1 }] })).unwrap();
        assert_eq!((odd.status, odd.due, odd.links.len()), (Status::Backlog, None, 0));
        assert_eq!(Issue::from_json(1, &json!({ "status": "done" })), None);
    }

    #[test]
    fn an_epic_and_a_config_survive_their_json() {
        let mut epic = Epic::new("launch".into(), "Launch".into(), 5);
        assert_eq!(Epic::from_json("launch".into(), &epic.to_json()), Some(epic.clone()));
        epic.description = "Ship it".into();
        epic.status = EpicStatus::Active;
        epic.lead = Some("arpit".into());
        epic.start = Some(day(2026, 11, 1));
        epic.target = Some(day(2026, 12, 15));
        epic.add_milestone("Beta", Some(day(2026, 12, 1)));
        epic.add_milestone("GA", None);
        assert_eq!(Epic::from_json("launch".into(), &epic.to_json()), Some(epic));
        let mut config = Config::new("PO".into());
        config.next = 8;
        config.labels = vec![Label { name: "bug".into(), color: "#e5484d".into() }, Label { name: "ui".into(), color: String::new() }];
        assert_eq!(Config::from_json(&config.to_json(), "XX"), config);
        // A prefix that couldn't be in a key is replaced.
        assert_eq!(Config::from_json(&json!({ "prefix": "a-b" }), "XX").prefix, "XX");
    }

    #[test]
    fn prefixes_come_from_a_repos_name() {
        assert_eq!(default_prefix("po"), "PO");
        assert_eq!(default_prefix("frontend"), "FRO");
        assert_eq!(default_prefix("my-cool_app"), "MCA");
        assert_eq!(default_prefix("one two three four five"), "OTTF");
        assert_eq!(default_prefix("日本"), "ISS");
        assert_eq!(slug("Launch v2!", "x"), "launch-v2");
    }

    #[test]
    fn what_a_merge_left_inconsistent_is_mended_on_load() {
        let mut config = Config::new("PO".into());
        config.next = 3;
        let issue = |n: u64, parent: Option<u64>| Issue { parent, ..Issue::new(n, format!("i{n}"), 1) };
        // Numbers beyond `next`, a parent that is gone, and two that are each other's parent.
        let issues = vec![issue(1, Some(9)), issue(5, Some(6)), issue(6, Some(5)), issue(7, None)];
        let (t, problems) = Tracker::from_parts(config, issues, vec![]);
        assert_eq!(t.config.next, 8);
        assert!(t.dirty.config && t.dirty.issues.is_empty(), "mended in memory; written when something changes");
        // Of the two in a loop, the first found is let go and the other is left under it.
        assert_eq!((t.issue(1).unwrap().parent, t.issue(5).unwrap().parent, t.issue(6).unwrap().parent), (None, None, Some(5)));
        assert_eq!(problems.len(), 3, "{problems:?}");
    }
}
