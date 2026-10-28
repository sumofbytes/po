//! What a screen asks of a [`Tracker`]: which issues to show, how they group, and what a line
//! typed into the add field means.
use super::{Issue, Priority, Status, Tracker};
use crate::todo::Date;

/// Which issues the list shows by how far along they are.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Scope {
    /// Not done and not canceled.
    #[default]
    Open,
    All,
    /// Done or canceled.
    Closed,
}

#[derive(Clone, PartialEq, Default, Debug)]
pub struct Filter {
    pub scope: Scope,
    /// Words in the key, title, description or a label; every word must be.
    pub text: String,
    pub label: Option<String>,
    pub epic: Option<String>,
    pub priority: Option<Priority>,
    /// Open and past due.
    pub late: bool,
}

impl Filter {
    pub fn matches(&self, issue: &Issue, key: &str, today: Date) -> bool {
        let scoped = match self.scope {
            Scope::All => true,
            Scope::Open => issue.status.is_open(),
            Scope::Closed => !issue.status.is_open(),
        };
        scoped
            && self.label.as_ref().is_none_or(|label| issue.labels.iter().any(|l| l == label))
            && self.epic.as_ref().is_none_or(|epic| issue.epic.as_ref() == Some(epic))
            && self.priority.is_none_or(|priority| issue.priority == priority)
            && (!self.late || (issue.status.is_open() && issue.due.is_some_and(|due| due < today)))
            && self.text.split_whitespace().all(|word| {
                let word = word.to_lowercase();
                [key, &issue.title, &issue.description].iter().any(|text| text.to_lowercase().contains(&word)) || issue.labels.iter().any(|l| l.to_lowercase().contains(&word))
            })
    }

    /// Nothing but the default scope is asked for.
    pub fn is_plain(&self) -> bool {
        *self == Self { scope: self.scope, ..Self::default() }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Group {
    #[default]
    Status,
    Priority,
    Epic,
    None,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Status, Group::Priority, Group::Epic, Group::None];

    pub fn name(self) -> &'static str {
        match self {
            Group::Status => "Status",
            Group::Priority => "Priority",
            Group::Epic => "Epic",
            Group::None => "None",
        }
    }
}

/// A run of the list under a heading.
#[derive(Clone, PartialEq, Debug)]
pub struct Section {
    pub title: String,
    /// What a section of this heading stands for, so a new issue in it can take it: a status's
    /// slug, a priority's, an epic's id.
    pub key: Option<String>,
    pub issues: Vec<u64>,
}

/// The issues `filter` lets through, grouped, each group with the most pressing first. Empty
/// groups are left out.
pub fn sections(tracker: &Tracker, filter: &Filter, group: Group, today: Date) -> Vec<Section> {
    let mut shown: Vec<&Issue> = tracker.issues().filter(|issue| filter.matches(issue, &tracker.key(issue.number), today)).collect();
    shown.sort_by_key(|issue| (issue.status, issue.priority.rank(), std::cmp::Reverse(issue.number)));
    let by = |title: String, key: Option<String>, keep: &dyn Fn(&Issue) -> bool| Section { title, key, issues: shown.iter().filter(|issue| keep(issue)).map(|issue| issue.number).collect() };
    let all: Vec<Section> = match group {
        Group::None => vec![by(String::new(), None, &|_| true)],
        Group::Status => Status::ALL.iter().map(|s| by(s.name().into(), Some(s.slug().into()), &|issue| issue.status == *s)).collect(),
        Group::Priority => {
            let mut order = Priority::ALL;
            order.sort_by_key(|p| p.rank());
            order.iter().map(|p| by(p.name().into(), Some(p.slug().into()), &|issue| issue.priority == *p)).collect()
        }
        Group::Epic => {
            let known = |issue: &Issue| issue.epic.as_ref().is_some_and(|id| tracker.epic(id).is_some());
            let mut sections: Vec<Section> = tracker.epics().iter().map(|e| by(e.name.clone(), Some(e.id.clone()), &|issue| issue.epic.as_ref() == Some(&e.id))).collect();
            sections.push(by("No epic".into(), None, &|issue| !known(issue)));
            sections
        }
    };
    all.into_iter().filter(|section| !section.issues.is_empty()).collect()
}

/// A line typed to add an issue, taken apart.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct Quick {
    pub title: String,
    pub priority: Option<Priority>,
    pub labels: Vec<String>,
    pub epic: Option<String>,
}

/// `Fix login !high #bug @launch`: `!` and a priority (its name or the start of it, or 1 to 4 for
/// urgent to low), `#` and a label, `@` and an epic by id or the start of its id. Anything else
/// that starts that way stays in the title.
pub fn parse_quick(text: &str, tracker: &Tracker) -> Quick {
    let mut quick = Quick::default();
    let mut words = vec![];
    for word in text.split_whitespace() {
        let rest = &word[word.char_indices().nth(1).map_or(word.len(), |(at, _)| at)..];
        let taken = match word.chars().next() {
            Some('!') if !rest.is_empty() && quick.priority.is_none() => {
                let rest = rest.to_lowercase();
                let by_number = rest.parse::<usize>().ok().and_then(|n| [Priority::Urgent, Priority::High, Priority::Medium, Priority::Low].get(n.wrapping_sub(1)).copied());
                let by_name = [Priority::Urgent, Priority::High, Priority::Medium, Priority::Low].into_iter().find(|p| p.slug().starts_with(&rest));
                by_number.or(by_name).map(|p| quick.priority = Some(p)).is_some()
            }
            Some('#') => match tracker.config.labels.iter().find(|label| label.name.eq_ignore_ascii_case(rest)) {
                Some(label) => {
                    if !quick.labels.contains(&label.name) {
                        quick.labels.push(label.name.clone());
                    }
                    true
                }
                None => false,
            },
            Some('@') if !rest.is_empty() && quick.epic.is_none() => {
                let rest = rest.to_lowercase();
                let epic = tracker.epics().iter().find(|e| e.id == rest).or_else(|| tracker.epics().iter().find(|e| e.id.starts_with(&rest)));
                epic.map(|e| quick.epic = Some(e.id.clone())).is_some()
            }
            _ => false,
        };
        if !taken {
            words.push(word);
        }
    }
    quick.title = words.join(" ");
    quick
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::Config;

    fn day() -> Date {
        Date::from_ymd(2026, 10, 28).unwrap()
    }

    fn sample() -> Tracker {
        let mut t = Tracker::new(Config::new("PO".into()));
        t.define_label("bug", "");
        t.define_label("ui", "");
        let launch = t.create_epic("Launch", 1);
        let a = t.create("Fix login", 1);
        let b = t.create("Polish board", 1);
        let c = t.create("Write docs", 1);
        t.update(a, 2, |i| (i.status, i.priority, i.labels, i.epic) = (Status::InProgress, Priority::High, vec!["bug".into()], Some(launch.clone())));
        t.update(b, 2, |i| (i.priority, i.labels, i.due) = (Priority::Urgent, vec!["ui".into()], Some(day().plus(-2))));
        t.update(c, 2, |i| i.status = Status::Done);
        t
    }

    #[test]
    fn the_filter_narrows_by_scope_words_label_epic_priority_and_lateness() {
        let t = sample();
        let shown = |filter: &Filter| t.issues().filter(|i| filter.matches(i, &t.key(i.number), day())).map(|i| i.number).collect::<Vec<_>>();
        assert_eq!(shown(&Filter::default()), [1, 2]);
        assert_eq!(shown(&Filter { scope: Scope::All, ..Default::default() }), [1, 2, 3]);
        assert_eq!(shown(&Filter { scope: Scope::Closed, ..Default::default() }), [3]);
        assert_eq!(shown(&Filter { text: "LOGIN fix".into(), ..Default::default() }), [1]);
        assert_eq!(shown(&Filter { text: "po-2".into(), ..Default::default() }), [2]);
        assert_eq!(shown(&Filter { text: "bug".into(), ..Default::default() }), [1], "labels count as words");
        assert_eq!(shown(&Filter { label: Some("ui".into()), ..Default::default() }), [2]);
        assert_eq!(shown(&Filter { epic: Some("launch".into()), ..Default::default() }), [1]);
        assert_eq!(shown(&Filter { priority: Some(Priority::Urgent), ..Default::default() }), [2]);
        assert_eq!(shown(&Filter { late: true, scope: Scope::All, ..Default::default() }), [2]);
        assert!(Filter::default().is_plain() && !Filter { late: true, ..Default::default() }.is_plain());
    }

    #[test]
    fn issues_group_by_status_priority_or_epic_most_pressing_first() {
        let t = sample();
        let all = Filter { scope: Scope::All, ..Default::default() };
        let titles = |group| sections(&t, &all, group, day()).iter().map(|s| (s.title.clone(), s.issues.clone())).collect::<Vec<_>>();
        assert_eq!(titles(Group::Status), [("Backlog".into(), vec![2]), ("In Progress".into(), vec![1]), ("Done".into(), vec![3])]);
        assert_eq!(titles(Group::Priority), [("Urgent".into(), vec![2]), ("High".into(), vec![1]), ("No priority".into(), vec![3])]);
        assert_eq!(titles(Group::Epic), [("Launch".to_string(), vec![1]), ("No epic".to_string(), vec![2, 3])]);
        assert_eq!(titles(Group::None), [(String::new(), vec![2, 1, 3])]);
        // A section carries what it stands for.
        assert_eq!(sections(&t, &all, Group::Status, day())[1].key.as_deref(), Some("in-progress"));
    }

    #[test]
    fn a_typed_line_gives_a_title_and_what_was_tagged_on() {
        let t = sample();
        let q = parse_quick("Fix the thing !h #BUG @lau #ui", &t);
        assert_eq!(q, Quick { title: "Fix the thing".into(), priority: Some(Priority::High), labels: vec!["bug".into(), "ui".into()], epic: Some("launch".into()) });
        assert_eq!(parse_quick("a !2", &t).priority, Some(Priority::High));
        assert_eq!(parse_quick("a !1", &t).priority, Some(Priority::Urgent));
        // What matches nothing is part of the title.
        let q = parse_quick("Use #hashtags !! @nobody 100% !", &t);
        assert_eq!(q.title, "Use #hashtags !! @nobody 100% !");
        assert_eq!((q.priority, q.epic, q.labels.len()), (None, None, 0));
        // Only the first priority counts; the second is left in the title.
        assert_eq!(parse_quick("x !low !high", &t).title, "x !high");
        assert_eq!(parse_quick("   ", &t).title, "");
    }
}
