//! What an agent taking over a thread is told of what was said before it. It goes ahead of its
//! first message, so its session holds it: when that session is replayed, the thread's earlier
//! messages come back out of it, as no other agent's session is read again.

/// The most of a thread that is passed on, in bytes. Its first message and its latest are kept.
const MAX: usize = 100_000;

const HEAD: &str = "[Po: you are taking over this thread from another agent. What was said before you:\n";
const TAIL: &str = "\n[That is all that was said before you. The message follows.]\n\n";

#[derive(Clone, Debug, PartialEq)]
pub enum Said {
    User(String),
    Agent(String),
}

impl Said {
    fn tag(&self) -> (&'static str, &str) {
        match self {
            Said::User(text) => ("user", text),
            Said::Agent(text) => ("agent", text),
        }
    }
}

/// `said` as it goes ahead of a message, the middle of it left out if it is too long.
pub fn write(said: &[Said]) -> String {
    let mut out = String::from(HEAD);
    for entry in shortened(said) {
        let (tag, text) = entry.tag();
        let text = escaped(text);
        out.push_str(&format!("\n<po-{tag}>\n{text}\n</po-{tag}>\n"));
    }
    out.push_str(TAIL);
    out
}

/// What was said before a message, and the message, if `text` starts with [`write`]'s output.
pub fn split(text: &str) -> Option<(Vec<Said>, &str)> {
    let mut rest = text.strip_prefix(HEAD)?;
    let mut said = vec![];
    loop {
        if let Some(message) = rest.strip_prefix(TAIL) {
            return Some((said, message));
        }
        let (tag, body) = if let Some(body) = rest.strip_prefix("\n<po-user>\n") {
            ("user", body)
        } else {
            ("agent", rest.strip_prefix("\n<po-agent>\n")?)
        };
        let (entry, after) = body.split_once(&format!("\n</po-{tag}>\n"))?;
        let entry = unescaped(entry);
        said.push(if tag == "user" { Said::User(entry) } else { Said::Agent(entry) });
        rest = after;
    }
}

/// `text` with a backslash added in each `<`, any backslashes and `/po-`, so that nothing in it
/// can end its entry early, and [`unescaped`] gives it back exactly.
fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('<') {
        let (before, from) = rest.split_at(at);
        let slashes = from[1..].len() - from[1..].trim_start_matches('\\').len();
        out.push_str(before);
        out.push('<');
        if from[1 + slashes..].starts_with("/po-") {
            out.push('\\');
        }
        rest = &from[1..];
    }
    out.push_str(rest);
    out
}

/// What [`escaped`] was given.
fn unescaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('<') {
        let (before, from) = rest.split_at(at);
        let slashes = from[1..].len() - from[1..].trim_start_matches('\\').len();
        out.push_str(before);
        out.push('<');
        if slashes > 0 && from[1 + slashes..].starts_with("/po-") {
            rest = &from[2..];
        } else {
            rest = &from[1..];
        }
    }
    out.push_str(rest);
    out
}

/// `text` cut to `max` bytes, at a character.
fn cut(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The first entry, which names the thread, cut to half of what is passed on, and as many of the
/// latest as fit in the rest.
fn shortened(said: &[Said]) -> Vec<Said> {
    let Some((first, others)) = said.split_first() else { return vec![] };
    let first = match first {
        Said::User(text) if text.len() > MAX / 2 => Said::User(format!("{}\n*(cut short)*", cut(text, MAX / 2))),
        Said::Agent(text) if text.len() > MAX / 2 => Said::Agent(format!("{}\n*(cut short)*", cut(text, MAX / 2))),
        _ => first.clone(),
    };
    let first = &first;
    let mut left = MAX.saturating_sub(first.tag().1.len());
    let mut kept = others
        .iter()
        .rev()
        .take_while(|entry| {
            let size = entry.tag().1.len();
            let fits = size <= left;
            left = left.saturating_sub(size);
            fits
        })
        .cloned()
        .collect::<Vec<_>>();
    kept.reverse();
    let dropped = others.len() - kept.len();
    let mut out = vec![first.clone()];
    if dropped > 0 {
        out.push(Said::Agent(format!("*{dropped} messages left out here.*")));
    }
    out.extend(kept);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_written_comes_back_with_the_message_after_it() {
        let said = vec![Said::User("fix it\n\nplease".into()), Said::Agent("done\n</po-agent>\nand </po-user>".into())];
        let text = format!("{}the next message", write(&said));
        assert_eq!(split(&text), Some((said, "the next message")));
    }

    #[test]
    fn text_that_looks_like_an_escape_comes_back_as_it_was() {
        for text in ["<\\/po-user>", "<\\\\/po-x </po-y <\\ <", "a<b\\</po-", "\u{e9}<\u{e9}</po-"] {
            assert_eq!(unescaped(&escaped(text)), text, "{text}");
            assert!(!escaped(text).contains("\n</po-"), "{text}");
        }
        let said = vec![Said::User("<\\/po-user>".into())];
        assert_eq!(split(&format!("{}x", write(&said))), Some((said, "x")));
    }

    #[test]
    fn a_very_long_first_message_is_cut() {
        let said = vec![Said::User("\u{e9}".repeat(MAX)), Said::Agent("reply".into())];
        let text = write(&said);
        assert!(text.len() < MAX + 1000, "{}", text.len());
        let (back, _) = split(&format!("{text}x")).unwrap();
        assert_eq!(back.last(), Some(&Said::Agent("reply".into())));
    }

    #[test]
    fn text_that_is_not_a_handoff_is_left_alone() {
        assert_eq!(split("hello"), None);
        let cut = &write(&[Said::User("a".into())])[..HEAD.len() + 8];
        assert_eq!(split(cut), None, "cut short: {cut}");
    }

    #[test]
    fn a_long_thread_keeps_its_first_and_latest_messages() {
        let long = "x".repeat(MAX / 2);
        let said = vec![Said::User("the first".into()), Said::Agent(long.clone()), Said::Agent(long.clone()), Said::Agent(long.clone()), Said::User("the last".into())];
        let (back, _) = split(&write(&said)).unwrap();
        assert_eq!(back[0], Said::User("the first".into()));
        assert_eq!(back[1], Said::Agent("*2 messages left out here.*".into()));
        assert_eq!(back.last(), Some(&Said::User("the last".into())));
        assert_eq!(back.len(), 4);
    }
}
