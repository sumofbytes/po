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
    /// Starts `agent` in `cwd`. The child's pid is known once it is started, and `control` is
    /// then asked for the environment Po's MCP server for this thread gets (a dex daemon gets
    /// no MCP servers, and is shared by every thread that uses it, so it names no one's agent).
    /// With `resume`, lists its sessions rather than opening a new one.
    pub fn spawn(agent: &config::Agent, cwd: &Path, resume: bool, control: impl FnOnce(Option<u32>) -> Vec<(String, String)>) -> Result<(Self, UnboundedReceiver<Event>)> {
        match &agent.daemon {
            Some(url) => {
                let (client, events) = dex::Client::spawn(url, &agent.command, cwd, resume)?;
                Ok((Self::Dex(client), events))
            }
            None => {
                let (client, events) = acp::Client::spawn(&agent.command, cwd, resume, |pid| control(Some(pid)))?;
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

    /// The session this client has open, once it has one.
    pub fn session(&self) -> Option<String> {
        match self {
            Self::Acp(client) => client.session(),
            Self::Dex(client) => client.session(),
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

    /// Changes a session setting, like its model.
    pub fn set_option(&self, id: &str, value: &str) -> bool {
        match self {
            Self::Acp(client) => client.set_option(id, value),
            Self::Dex(client) => client.set_option(id, value),
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
