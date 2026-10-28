//! The issue and epic commands agents and scripts use, run against a [`Tracker`]. Nothing here
//! touches the window: a request is read, applied and answered, and the caller saves.
use serde_json::{Value, json};

use super::query::{self, Filter, Group, Scope};
use super::{Issue, Link, Priority, Status, Tracker};
use crate::todo::{Date, parse_due};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    IssueList,
    IssueGet,
    IssueCreate,
    IssueUpdate,
    EpicList,
    EpicCreate,
    EpicUpdate,
}

impl Op {
    pub const ALL: [Op; 7] = [Op::IssueList, Op::IssueGet, Op::IssueCreate, Op::IssueUpdate, Op::EpicList, Op::EpicCreate, Op::EpicUpdate];

    pub fn name(self) -> &'static str {
        match self {
            Op::IssueList => "issue.list",
            Op::IssueGet => "issue.get",
            Op::IssueCreate => "issue.create",
            Op::IssueUpdate => "issue.update",
            Op::EpicList => "epic.list",
            Op::EpicCreate => "epic.create",
            Op::EpicUpdate => "epic.update",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.name() == name)
    }

    pub fn writes(self) -> bool {
        matches!(self, Op::IssueCreate | Op::IssueUpdate | Op::EpicCreate | Op::EpicUpdate)
    }
}

/// The most issues one `issue.list` returns.
const MAX_LIST: usize = 200;
/// The longest title and description taken, in characters.
const MAX_TITLE: usize = 300;
const MAX_TEXT: usize = 50_000;

fn date(date: Option<Date>) -> Value {
    date.map_or(Value::Null, |date| json!(date.iso()))
}

/// An issue as the API shows it, with its relations by key.
pub fn issue_json(tracker: &Tracker, issue: &Issue, full: bool) -> Value {
    let mut out = json!({
        "key": tracker.key(issue.number),
        "title": issue.title,
        "status": issue.status.slug(),
        "priority": issue.priority.slug(),
        "labels": issue.labels,
        "epic": issue.epic,
        "milestone": issue.milestone,
        "parent": issue.parent.map(|parent| tracker.key(parent)),
        "assignee": issue.assignee,
        "due": date(issue.due),
    });
    if full {
        let progress = tracker.progress(issue.number);
        out["description"] = json!(issue.description);
        out["estimate"] = json!(issue.estimate);
        out["created"] = json!(issue.created);
        out["updated"] = json!(issue.updated);
        out["links"] = Value::Array(
            issue
                .links
                .iter()
                .map(|link| match link {
                    Link::Thread(session) => json!({ "thread": session }),
                    Link::Branch(branch) => json!({ "branch": branch }),
                    Link::Pr(pr) => json!({ "pr": pr }),
                })
                .collect(),
        );
        out["sub_issues"] = json!(tracker.children(issue.number).map(|child| tracker.key(child.number)).collect::<Vec<_>>());
        out["progress"] = json!({ "done": progress.done, "started": progress.started, "all": progress.all });
    }
    out
}

fn epic_json(tracker: &Tracker, epic: &super::Epic) -> Value {
    let progress = |p: super::Progress| json!({ "done": p.done, "started": p.started, "all": p.all });
    json!({
        "id": epic.id,
        "name": epic.name,
        "description": epic.description,
        "status": epic.status.slug(),
        "lead": epic.lead,
        "start": date(epic.start),
        "target": date(epic.target),
        "progress": progress(tracker.epic_progress(&epic.id)),
        "milestones": epic.milestones.iter().map(|m| json!({
            "id": m.id, "name": m.name, "target": date(m.target), "progress": progress(tracker.milestone_progress(&epic.id, &m.id)),
        })).collect::<Vec<_>>(),
    })
}

/// A field that is a string, if it is present: `Ok(None)` when absent, an error when it is
/// something else.
fn string(args: &Value, key: &str) -> Result<Option<String>, String> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.clone())),
        _ => Err(format!("`{key}` is text")),
    }
}

/// A field that can be cleared: absent leaves it, null or empty clears it.
fn clearable(args: &Value, key: &str) -> Result<Option<Option<String>>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(Some(None)),
        Some(Value::String(text)) => Ok(Some(Some(text.trim().to_string()))),
        Some(_) => Err(format!("`{key}` is text, or null to clear it")),
    }
}

fn strings(args: &Value, key: &str) -> Result<Option<Vec<String>>, String> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::Array(list) => list.iter().map(|item| item.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or_else(|| format!("`{key}` is a list of names"))).collect::<Result<_, _>>().map(Some),
        _ => Err(format!("`{key}` is a list of names")),
    }
}

fn status(text: &str) -> Result<Status, String> {
    let slug = text.trim().to_lowercase().replace([' ', '_'], "-");
    Status::from_slug(&slug).ok_or_else(|| format!("`{text}` is not a status; one of {}", Status::ALL.map(Status::slug).join(", ")))
}

fn priority(text: &str) -> Result<Priority, String> {
    Priority::from_slug(&text.trim().to_lowercase()).ok_or_else(|| format!("`{text}` is not a priority; one of {}", Priority::ALL.map(Priority::slug).join(", ")))
}

/// A date: `2026-11-02` or the words people use (`fri`, `nov 2`); null or empty clears it.
fn when(args: &Value, key: &str, today: Date) -> Result<Option<Option<Date>>, String> {
    clearable(args, key)?.map(|text| text.map(|text| parse_due(&text, today).flatten().ok_or_else(|| format!("`{text}` is not a date"))).transpose()).transpose()
}

fn issue_number(tracker: &Tracker, args: &Value, key: &str) -> Result<u64, String> {
    let text = string(args, key)?.ok_or_else(|| format!("needs `{key}`, an issue's key such as {}-1", tracker.config.prefix))?;
    tracker.parse_key(&text).filter(|n| tracker.issue(*n).is_some()).ok_or_else(|| format!("no issue `{text}`"))
}

fn limited(text: String, key: &str, max: usize) -> Result<String, String> {
    if text.chars().count() > max { Err(format!("`{key}` is at most {max} characters")) } else { Ok(text) }
}

/// What `issue.create` and `issue.update` may set, read once so both apply it alike. A field not
/// given is `None`, one given to clear is `Some(None)`.
#[derive(Default)]
struct Edit {
    title: Option<String>,
    description: Option<String>,
    note: Option<String>,
    status: Option<Status>,
    priority: Option<Priority>,
    labels: Option<Vec<String>>,
    add_labels: Vec<String>,
    remove_labels: Vec<String>,
    assignee: Option<Option<String>>,
    parent: Option<Option<u64>>,
    epic: Option<Option<String>>,
    milestone: Option<Option<String>>,
    due: Option<Option<Date>>,
    estimate: Option<Option<u32>>,
    link: Option<Link>,
}

impl Edit {
    fn read(tracker: &Tracker, args: &Value, today: Date) -> Result<Self, String> {
        let parent = match clearable(args, "parent")? {
            None => None,
            Some(None) => Some(None),
            Some(Some(key)) => Some(Some(tracker.parse_key(&key).filter(|n| tracker.issue(*n).is_some()).ok_or_else(|| format!("no issue `{key}` to be the parent"))?)),
        };
        let estimate = match args.get("estimate") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(value) => Some(Some(value.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or("`estimate` is a whole number")?)),
        };
        let link = match &args["link"] {
            Value::Null => None,
            link => Some(Link::from_json_value(link).ok_or("`link` is {\"thread\": session}, {\"branch\": name} or {\"pr\": number}")?),
        };
        Ok(Self {
            title: string(args, "title")?.map(|t| limited(t.trim().to_string(), "title", MAX_TITLE)).transpose()?,
            description: string(args, "description")?.map(|t| limited(t, "description", MAX_TEXT)).transpose()?,
            note: string(args, "note")?.map(|t| limited(t, "note", MAX_TEXT)).transpose()?,
            status: string(args, "status")?.map(|s| status(&s)).transpose()?,
            priority: string(args, "priority")?.map(|p| priority(&p)).transpose()?,
            labels: strings(args, "labels")?,
            add_labels: strings(args, "add_labels")?.unwrap_or_default(),
            remove_labels: strings(args, "remove_labels")?.unwrap_or_default(),
            assignee: clearable(args, "assignee")?,
            parent,
            epic: clearable(args, "epic")?,
            milestone: clearable(args, "milestone")?,
            due: when(args, "due", today)?,
            estimate,
            link,
        })
    }

    /// Makes `number` what this says. Labels it names that the repo lacks are defined.
    fn apply(self, tracker: &mut Tracker, number: u64, now: u64, today: Date) {
        let named: Vec<String> = self.labels.iter().flatten().chain(&self.add_labels).cloned().collect();
        for label in named {
            if !tracker.config.labels.iter().any(|l| l.name == label) {
                tracker.define_label(&label, "");
            }
        }
        tracker.update(number, now, |issue| {
            if let Some(title) = self.title.filter(|t| !t.is_empty()) {
                issue.title = title;
            }
            if let Some(description) = self.description {
                issue.description = description;
            }
            if let Some(note) = self.note.filter(|n| !n.trim().is_empty()) {
                if !issue.description.is_empty() {
                    issue.description.push_str("\n\n");
                }
                issue.description.push_str(&format!("### Note, {}\n{}", today.iso(), note.trim()));
            }
            issue.status = self.status.unwrap_or(issue.status);
            issue.priority = self.priority.unwrap_or(issue.priority);
            if let Some(labels) = self.labels {
                issue.labels = labels;
            }
            issue.labels.extend(self.add_labels);
            issue.labels.retain(|label| !self.remove_labels.contains(label));
            if let Some(assignee) = self.assignee {
                issue.assignee = assignee;
            }
            if let Some(parent) = self.parent {
                issue.parent = parent;
            }
            if let Some(epic) = self.epic {
                issue.epic = epic;
            }
            if let Some(milestone) = self.milestone {
                issue.milestone = milestone;
            }
            if let Some(due) = self.due {
                issue.due = due;
            }
            if let Some(estimate) = self.estimate {
                issue.estimate = estimate;
            }
            if let Some(link) = self.link
                && !issue.links.contains(&link)
            {
                issue.links.push(link);
            }
        });
    }
}

impl Link {
    pub fn from_json_value(value: &Value) -> Option<Self> {
        let keys = ["thread", "branch", "pr"].iter().filter(|key| value.get(**key).is_some()).count();
        (keys == 1).then(|| Self::from_json(value)).flatten()
    }
}

/// Runs `op` with `args` on `tracker`. Anything it changes is noted in the tracker for the store
/// to write.
pub fn run(op: Op, args: &Value, tracker: &mut Tracker, now: u64, today: Date) -> Result<Value, String> {
    match op {
        Op::IssueGet => {
            let number = issue_number(tracker, args, "issue")?;
            let issue = tracker.issue(number).expect("checked above");
            Ok(issue_json(tracker, issue, true))
        }
        Op::IssueList => {
            let scope = match string(args, "scope")?.as_deref() {
                None | Some("open") => Scope::Open,
                Some("all") => Scope::All,
                Some("closed") => Scope::Closed,
                Some(other) => return Err(format!("`{other}` is not a scope; open, closed or all")),
            };
            let one_status = string(args, "status")?.map(|s| status(&s)).transpose()?;
            let filter = Filter {
                // Naming a status asks for it, whatever its category.
                scope: if one_status.is_some() { Scope::All } else { scope },
                text: string(args, "text")?.unwrap_or_default(),
                label: string(args, "label")?,
                epic: string(args, "epic")?,
                priority: string(args, "priority")?.map(|p| priority(&p)).transpose()?,
                late: args["late"].as_bool().unwrap_or(false),
            };
            let parent = string(args, "parent")?.map(|key| tracker.parse_key(&key).ok_or_else(|| format!("no issue `{key}`"))).transpose()?;
            let limit = args["limit"].as_u64().map_or(50, |n| (n as usize).clamp(1, MAX_LIST));
            let all: Vec<u64> = query::sections(tracker, &filter, Group::None, today)
                .into_iter()
                .flat_map(|section| section.issues)
                .filter(|n| tracker.issue(*n).is_some_and(|issue| one_status.is_none_or(|s| issue.status == s) && parent.is_none_or(|p| issue.parent == Some(p))))
                .collect();
            let issues: Vec<Value> = all.iter().take(limit).filter_map(|n| tracker.issue(*n)).map(|issue| issue_json(tracker, issue, false)).collect();
            Ok(json!({ "issues": issues, "total": all.len() }))
        }
        Op::IssueCreate => {
            let edit = Edit::read(tracker, args, today)?;
            let title = edit.title.clone().filter(|title| !title.is_empty()).ok_or("issue.create needs a `title`")?;
            let number = tracker.create(&title, now);
            // A new issue to be worked is a todo unless it says otherwise.
            tracker.update(number, now, |issue| issue.status = Status::Todo);
            edit.apply(tracker, number, now, today);
            let issue = tracker.issue(number).expect("just made");
            Ok(issue_json(tracker, issue, true))
        }
        Op::IssueUpdate => {
            let number = issue_number(tracker, args, "issue")?;
            let edit = Edit::read(tracker, args, today)?;
            if let Some(Some(parent)) = edit.parent
                && tracker.would_loop(number, parent)
            {
                return Err(if parent == number { "an issue can't be its own parent".into() } else { format!("{} can't go under {}: that would make a loop", tracker.key(number), tracker.key(parent)) });
            }
            edit.apply(tracker, number, now, today);
            Ok(issue_json(tracker, tracker.issue(number).expect("checked above"), true))
        }
        Op::EpicList => Ok(json!({ "epics": tracker.epics().iter().map(|epic| epic_json(tracker, epic)).collect::<Vec<_>>() })),
        Op::EpicCreate => {
            let name = string(args, "name")?.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).ok_or("epic.create needs a `name`")?;
            let id = tracker.create_epic(&limited(name, "name", MAX_TITLE)?, now);
            update_epic(tracker, &id, args, now, today)?;
            Ok(epic_json(tracker, tracker.epic(&id).expect("just made")))
        }
        Op::EpicUpdate => {
            let id = string(args, "epic")?.ok_or("needs `epic`, an epic's id")?;
            if tracker.epic(&id).is_none() {
                return Err(format!("no epic `{id}`; epic.list has them"));
            }
            update_epic(tracker, &id, args, now, today)?;
            Ok(epic_json(tracker, tracker.epic(&id).expect("checked above")))
        }
    }
}

fn update_epic(tracker: &mut Tracker, id: &str, args: &Value, now: u64, today: Date) -> Result<(), String> {
    let name = string(args, "name")?.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let description = string(args, "description")?;
    let state = match string(args, "status")? {
        Some(text) => Some(super::EpicStatus::from_slug(&text.trim().to_lowercase()).ok_or_else(|| format!("`{text}` is not an epic status; one of {}", super::EpicStatus::ALL.map(super::EpicStatus::slug).join(", ")))?),
        None => None,
    };
    let lead = clearable(args, "lead")?;
    let (start, target) = (when(args, "start", today)?, when(args, "target", today)?);
    let add = match &args["add_milestone"] {
        Value::Null => None,
        Value::String(name) if !name.trim().is_empty() => Some((name.trim().to_string(), None)),
        milestone @ Value::Object(_) => {
            let name = string(milestone, "name")?.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).ok_or("a milestone needs a `name`")?;
            Some((name, when(milestone, "target", today)?.flatten()))
        }
        _ => return Err("`add_milestone` is a name, or {name, target}".into()),
    };
    let remove = string(args, "remove_milestone")?;
    if let Some(remove) = &remove
        && tracker.epic(id).is_some_and(|epic| epic.milestone(remove).is_none())
    {
        return Err(format!("no milestone `{remove}` in this epic"));
    }
    tracker.update_epic(id, now, |epic| {
        if let Some(name) = name {
            epic.name = name;
        }
        if let Some(description) = description {
            epic.description = description;
        }
        epic.status = state.unwrap_or(epic.status);
        if let Some(lead) = lead {
            epic.lead = lead;
        }
        if let Some(start) = start {
            epic.start = start;
        }
        if let Some(target) = target {
            epic.target = target;
        }
        if let Some((name, target)) = add {
            epic.add_milestone(&name, target);
        }
        if let Some(remove) = remove {
            epic.milestones.retain(|m| m.id != remove);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::Config;

    fn today() -> Date {
        Date::from_ymd(2026, 10, 28).unwrap()
    }

    fn run_on(t: &mut Tracker, op: Op, args: Value) -> Result<Value, String> {
        run(op, &args, t, 100, today())
    }

    fn tracker() -> Tracker {
        Tracker::new(Config::new("PO".into()))
    }

    #[test]
    fn an_agent_creates_updates_and_reads_an_issue() {
        let mut t = tracker();
        let made = run_on(&mut t, Op::IssueCreate, json!({ "title": "Fix login", "priority": "high", "labels": ["bug"], "due": "2026-11-02", "description": "It breaks." })).unwrap();
        assert_eq!((made["key"].as_str(), made["status"].as_str(), made["priority"].as_str()), (Some("PO-1"), Some("todo"), Some("high")));
        assert_eq!(made["due"], "2026-11-02");
        // The label it used is now the repo's.
        assert_eq!(t.config.labels[0].name, "bug");
        let updated = run_on(
            &mut t,
            Op::IssueUpdate,
            json!({ "issue": "po-1", "status": "In Progress", "add_labels": ["ui"], "remove_labels": ["bug"], "note": "Found the cause.", "link": { "branch": "po/abc" }, "due": null, "assignee": "claude" }),
        )
        .unwrap();
        assert_eq!(updated["status"], "in-progress");
        assert_eq!(updated["labels"], json!(["ui"]));
        assert_eq!((updated["due"].is_null(), updated["assignee"].as_str()), (true, Some("claude")));
        assert_eq!(updated["links"], json!([{ "branch": "po/abc" }]));
        assert_eq!(updated["description"], "It breaks.\n\n### Note, 2026-10-28\nFound the cause.");
        // The same link twice is one.
        let again = run_on(&mut t, Op::IssueUpdate, json!({ "issue": "1", "link": { "branch": "po/abc" } })).unwrap();
        assert_eq!(again["links"].as_array().unwrap().len(), 1);
        assert_eq!(run_on(&mut t, Op::IssueGet, json!({ "issue": "PO-1" })).unwrap()["title"], "Fix login");
    }

    #[test]
    fn a_bad_request_says_what_is_wrong_and_changes_nothing() {
        let mut t = tracker();
        run_on(&mut t, Op::IssueCreate, json!({ "title": "a" })).unwrap();
        t.dirty = Default::default();
        for (args, wanted) in [
            (json!({ "issue": "PO-9" }), "no issue"),
            (json!({}), "needs `issue`"),
            (json!({ "issue": "PO-1", "status": "shipped" }), "not a status"),
            (json!({ "issue": "PO-1", "priority": "asap" }), "not a priority"),
            (json!({ "issue": "PO-1", "due": "soon" }), "not a date"),
            (json!({ "issue": "PO-1", "labels": "bug" }), "list of names"),
            (json!({ "issue": "PO-1", "parent": "PO-1" }), "own parent"),
            (json!({ "issue": "PO-1", "parent": "PO-7" }), "no issue"),
            (json!({ "issue": "PO-1", "link": { "pr": 1, "branch": "x" } }), "`link`"),
            (json!({ "issue": "PO-1", "estimate": -1 }), "whole number"),
            (json!({ "issue": "PO-1", "title": "x".repeat(400) }), "at most"),
        ] {
            let error = run_on(&mut t, Op::IssueUpdate, args.clone()).unwrap_err();
            assert!(error.contains(wanted), "{args}: {error}");
        }
        assert!(t.dirty.is_empty(), "nothing was written");
        assert!(run_on(&mut t, Op::IssueCreate, json!({ "status": "done" })).unwrap_err().contains("needs a `title`"));
    }

    #[test]
    fn sub_issues_cannot_be_made_into_a_loop_through_the_api() {
        let mut t = tracker();
        run_on(&mut t, Op::IssueCreate, json!({ "title": "a" })).unwrap();
        run_on(&mut t, Op::IssueCreate, json!({ "title": "b", "parent": "PO-1" })).unwrap();
        let error = run_on(&mut t, Op::IssueUpdate, json!({ "issue": "PO-1", "parent": "PO-2" })).unwrap_err();
        assert!(error.contains("loop"), "{error}");
        assert_eq!(t.issue(1).unwrap().parent, None);
        let got = run_on(&mut t, Op::IssueGet, json!({ "issue": "PO-1" })).unwrap();
        assert_eq!((got["sub_issues"].clone(), got["progress"]["all"].clone()), (json!(["PO-2"]), json!(1)));
    }

    #[test]
    fn issues_are_listed_by_what_the_caller_asks_for() {
        let mut t = tracker();
        for (title, extra) in [("a", json!({ "priority": "low" })), ("b", json!({ "priority": "urgent", "epic": null })), ("c", json!({ "status": "done" }))] {
            let mut args = json!({ "title": title });
            args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            run_on(&mut t, Op::IssueCreate, args).unwrap();
        }
        let keys = |t: &mut Tracker, args: Value| run_on(t, Op::IssueList, args).unwrap()["issues"].as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(keys(&mut t, json!({})), ["PO-2", "PO-1"], "open ones, the most pressing first");
        assert_eq!(keys(&mut t, json!({ "scope": "all" })).len(), 3);
        assert_eq!(keys(&mut t, json!({ "status": "done" })), ["PO-3"], "a status asked for is shown whatever its scope");
        assert_eq!(keys(&mut t, json!({ "priority": "low" })), ["PO-1"]);
        assert_eq!(keys(&mut t, json!({ "text": "b" })), ["PO-2"]);
        let limited = run_on(&mut t, Op::IssueList, json!({ "scope": "all", "limit": 1 })).unwrap();
        assert_eq!((limited["issues"].as_array().unwrap().len(), limited["total"].clone()), (1, json!(3)));
        assert!(run_on(&mut t, Op::IssueList, json!({ "scope": "some" })).is_err());
    }

    #[test]
    fn epics_and_their_milestones_are_made_and_changed() {
        let mut t = tracker();
        let epic = run_on(&mut t, Op::EpicCreate, json!({ "name": "Launch", "status": "active", "target": "2026-12-15", "add_milestone": { "name": "Beta", "target": "2026-12-01" } })).unwrap();
        assert_eq!((epic["id"].as_str(), epic["status"].as_str(), epic["target"].as_str()), (Some("launch"), Some("active"), Some("2026-12-15")));
        assert_eq!(epic["milestones"][0]["id"], "beta");
        let made = run_on(&mut t, Op::IssueCreate, json!({ "title": "Notes", "epic": "launch", "milestone": "beta", "status": "done" })).unwrap();
        assert_eq!((made["epic"].as_str(), made["milestone"].as_str()), (Some("launch"), Some("beta")));
        let epic = run_on(&mut t, Op::EpicList, json!({})).unwrap()["epics"][0].clone();
        assert_eq!(epic["progress"], json!({ "done": 1, "started": 0, "all": 1 }));
        assert_eq!(epic["milestones"][0]["progress"]["done"], 1);
        // Taking a milestone away takes it off the issues in it.
        let epic = run_on(&mut t, Op::EpicUpdate, json!({ "epic": "launch", "remove_milestone": "beta", "add_milestone": "GA", "lead": "arpit" })).unwrap();
        assert_eq!((epic["milestones"][0]["id"].as_str(), epic["lead"].as_str()), (Some("ga"), Some("arpit")));
        assert_eq!(t.issue(1).unwrap().milestone, None);
        assert!(run_on(&mut t, Op::EpicUpdate, json!({ "epic": "launch", "remove_milestone": "nope" })).is_err());
        assert!(run_on(&mut t, Op::EpicUpdate, json!({ "epic": "nope" })).unwrap_err().contains("no epic"));
        assert!(run_on(&mut t, Op::EpicCreate, json!({})).is_err());
        assert!(run_on(&mut t, Op::EpicUpdate, json!({ "epic": "launch", "status": "later" })).is_err());
    }

    #[test]
    fn op_names_round_trip_and_only_some_write() {
        for op in Op::ALL {
            assert_eq!(Op::from_name(op.name()), Some(op));
        }
        assert_eq!(Op::ALL.iter().filter(|op| op.writes()).count(), 4);
        assert_eq!(Op::from_name("issue.delete"), None);
    }
}
