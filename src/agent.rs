//! The agent behind a thread: an ACP process, or a dex daemon that can also take steering.
use std::path::Path;

use anyhow::Result;
use futures::channel::mpsc::UnboundedReceiver;
use serde_json::Value;

use crate::{
    acp::{self, Event},
    config,
    dex::{self, Queue},
};

pub enum Client {
    Acp(acp::Client),
    Dex(dex::Client),
}

impl Client {
    /// Starts `agent` in `cwd`. With `resume`, lists its sessions rather than opening a new one.
    pub fn spawn(agent: &config::Agent, cwd: &Path, resume: bool) -> Result<(Self, UnboundedReceiver<Event>)> {
        match &agent.daemon {
            Some(url) => {
                let (client, events) = dex::Client::spawn(url, &agent.command, cwd, resume)?;
                Ok((Self::Dex(client), events))
            }
            None => {
                let (client, events) = acp::Client::spawn(&agent.command, cwd, resume)?;
                Ok((Self::Acp(client), events))
            }
        }
    }

    pub fn prompt(&self, text: &str) -> bool {
        match self {
            Self::Acp(client) => client.prompt(text),
            Self::Dex(client) => client.prompt(text),
        }
    }

    pub fn load(&self, id: &str) -> bool {
        match self {
            Self::Acp(client) => client.load(id),
            Self::Dex(client) => client.load(id),
        }
    }

    pub fn cancel(&self) {
        match self {
            Self::Acp(client) => client.cancel(),
            Self::Dex(client) => client.cancel(),
        }
    }

    pub fn answer(&self, request: Value, option: Option<&str>) {
        match self {
            Self::Acp(client) => client.answer(request, option),
            Self::Dex(client) => client.answer(request, option),
        }
    }

    /// Whether messages can be added to a running turn, rather than only sent after it.
    pub fn queues(&self) -> bool {
        matches!(self, Self::Dex(_))
    }

    pub fn enqueue(&self, content: &str, queue: Queue) {
        if let Self::Dex(client) = self {
            client.enqueue(content, queue);
        }
    }

    pub fn recall(&self, content: &str, queue: Queue) {
        if let Self::Dex(client) = self {
            client.recall(content, queue);
        }
    }
}
