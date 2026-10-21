//! `po mcp`: the commands of [`crate::command`] as an MCP server, so an agent finds them among its
//! tools rather than having to know about `po ctl`. Po hands it to each agent it starts, with the
//! socket and token of that agent's thread; every tool call becomes one request to the window.
use serde_json::{Value, json};

/// What the server tells the agent it is for.
const INSTRUCTIONS: &str = "You are running inside Po, a desktop app that shows the user your \
conversation next to a viewer of the project's files and changes. Use these tools to put in front \
of the user what you are talking about: open the file or diff you refer to, or show a plan or \
summary as a markdown view. The user decides what they look at: while they are typing or \
scrolling, a move of the viewer waits for a pause (the reply says `deferred`) and a new view is \
offered rather than shown. Don't fight that by asking again.";

/// Protocol versions this server speaks, newest first.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Each tool, the command it sends, what it is for and its arguments.
fn tools() -> Vec<(&'static str, &'static str, &'static str, Value)> {
    let string = |description: &str| json!({ "type": "string", "description": description });
    vec![
        (
            "show_view",
            "view.show",
            "Show the user a markdown document in Po's viewer, as a tab of its own: a plan, a \
             summary, a review. Giving the `id` of one of your views replaces it.",
            json!({
                "type": "object",
                "properties": {
                    "title": string("A short title for the tab."),
                    "body": string("The document, in markdown."),
                    "id": string("The view to replace; leave out to add a new one."),
                },
                "required": ["title", "body"],
            }),
        ),
        (
            "update_view",
            "view.update",
            "Change the title or body of one of your views without moving the user to it.",
            json!({
                "type": "object",
                "properties": {
                    "id": string("The view, as show_view returned it."),
                    "title": string("A new title."),
                    "body": string("A new body, in markdown."),
                },
                "required": ["id"],
            }),
        ),
        (
            "close_view",
            "view.close",
            "Close one of your views.",
            json!({ "type": "object", "properties": { "id": string("The view to close.") }, "required": ["id"] }),
        ),
        (
            "open_file",
            "file.open",
            "Open a file of the project in Po's viewer, at a line if given, to show the user the \
             code you are talking about.",
            json!({
                "type": "object",
                "properties": {
                    "path": string("The file, relative to the project root or absolute inside it."),
                    "line": { "type": "integer", "minimum": 1, "description": "The line to put in view, from 1." },
                },
                "required": ["path"],
            }),
        ),
        (
            "show_diff",
            "diff.show",
            "Show the project's uncommitted changes in Po's viewer, at one file's diff if given.",
            json!({
                "type": "object",
                "properties": { "path": string("A changed file, relative to the project root.") },
            }),
        ),
        (
            "close_viewer",
            "viewer.close",
            "Close Po's viewer, so the conversation has the window to itself.",
            json!({ "type": "object", "properties": {} }),
        ),
    ]
}

/// The answer to one JSON-RPC message, or none for a notification. `send` carries a command to
/// the window and returns its reply.
fn handle(message: &Value, send: &dyn Fn(Value) -> anyhow::Result<Value>) -> Option<Value> {
    let id = message.get("id")?.clone();
    let params = &message["params"];
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = VERSIONS.iter().find(|version| **version == asked).unwrap_or(&VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "po", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => {
            let tools: Vec<Value> = tools()
                .into_iter()
                .map(|(name, _, description, schema)| json!({ "name": name, "description": description, "inputSchema": schema }))
                .collect();
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => call(params, send),
        method => Err((-32601, format!("no method `{method}`"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    })
}

/// Runs a tool. What the window refuses is the tool's error for the agent to read, not the
/// protocol's.
fn call(params: &Value, send: &dyn Fn(Value) -> anyhow::Result<Value>) -> Result<Value, (i64, String)> {
    let name = params["name"].as_str().unwrap_or_default();
    let command = tools().into_iter().find(|tool| tool.0 == name).map(|tool| tool.1).ok_or((-32602, format!("no tool `{name}`")))?;
    let mut request = match &params["arguments"] {
        Value::Object(arguments) => Value::Object(arguments.clone()),
        Value::Null => json!({}),
        _ => return Err((-32602, "arguments must be an object".into())),
    };
    request["command"] = command.into();
    let (text, error) = match send(request) {
        Ok(reply) => (reply.to_string(), false),
        Err(error) => (format!("{error:#}"), true),
    };
    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": error }))
}

/// Serves MCP on standard input and output until the agent closes it.
pub fn serve() -> anyhow::Result<()> {
    use std::io::{BufRead, Write};
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(&message, &crate::control::send),
            Err(error) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": error.to_string() } })),
        };
        if let Some(reply) = reply {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// The entry for Po's own server in ACP's `mcpServers`, carrying `env` (the thread's socket and
/// token). None if Po can't name its own binary.
pub fn server(env: &[(String, String)]) -> Option<Value> {
    let exe = std::env::current_exe().ok()?;
    let env: Vec<Value> = env.iter().map(|(name, value)| json!({ "name": name, "value": value })).collect();
    Some(json!({ "name": "po", "command": exe.to_string_lossy(), "args": ["mcp"], "env": env }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn refuse(_: Value) -> anyhow::Result<Value> {
        panic!("must not reach the window")
    }

    #[test]
    fn it_introduces_itself_in_the_version_asked_for() {
        let reply = handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } }), &refuse).unwrap();
        assert_eq!(reply["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(reply["result"]["serverInfo"]["name"], "po");
        let reply = handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "1999-01-01" } }), &refuse).unwrap();
        assert_eq!(reply["result"]["protocolVersion"], VERSIONS[0]);
    }

    #[test]
    fn notifications_get_no_answer() {
        assert!(handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), &refuse).is_none());
    }

    #[test]
    fn every_tool_is_listed_with_a_schema_and_names_a_command_po_reads() {
        let reply = handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }), &refuse).unwrap();
        let listed = reply["result"]["tools"].as_array().unwrap();
        assert_eq!(listed.len(), tools().len());
        assert!(listed.iter().all(|tool| tool["inputSchema"]["type"] == "object"));
        for (_, command, ..) in tools() {
            // Refused for missing arguments, perhaps, but never as an unknown command.
            let parsed = crate::command::Command::parse(&json!({ "command": command }));
            assert!(parsed.as_ref().err().is_none_or(|error| !error.starts_with("no command")), "{command}: {parsed:?}");
        }
    }

    #[test]
    fn a_call_becomes_one_request_and_a_refusal_is_the_tools_error() {
        let sent = RefCell::new(vec![]);
        let send = |request: Value| {
            sent.borrow_mut().push(request.clone());
            if request["path"] == "nope.rs" { anyhow::bail!("no file `nope.rs` in the project") } else { Ok(json!({ "deferred": true })) }
        };
        let call = |arguments| json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "open_file", "arguments": arguments } });
        let reply = handle(&call(json!({ "path": "src/main.rs", "line": 4, "command": "viewer.close" })), &send).unwrap();
        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(reply["result"]["content"][0]["text"], r#"{"deferred":true}"#);
        // The tool decides the command, whatever the arguments say.
        assert_eq!(sent.borrow()[0], json!({ "command": "file.open", "path": "src/main.rs", "line": 4 }));
        let reply = handle(&call(json!({ "path": "nope.rs" })), &send).unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["text"], "no file `nope.rs` in the project");
    }

    #[test]
    fn unknown_methods_and_tools_are_protocol_errors() {
        let reply = handle(&json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/list" }), &refuse).unwrap();
        assert_eq!(reply["error"]["code"], -32601);
        let reply = handle(&json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": { "name": "rm" } }), &refuse).unwrap();
        assert_eq!(reply["error"]["code"], -32602);
    }
}
