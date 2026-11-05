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
        // Nothing in a message can end its entry early.
        let text = text.replace("</po-", "<\\/po-");
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
        let entry = entry.replace("<\\/po-", "</po-");
        said.push(if tag == "user" { Said::User(entry) } else { Said::Agent(entry) });
        rest = after;
    }
}

/// The first entry, which names the thread, and as many of the latest as fit.
fn shortened(said: &[Said]) -> Vec<Said> {
    let Some((first, others)) = said.split_first() else { return vec![] };
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
