//! Plugins: folders under `<config>/po/plugins/`, each with a `plugin.json` manifest.
//!
//! A plugin adds to what po reads from its config, without editing that file (the settings
//! page rewrites `config.json` whole, so plugins could not live there):
//!
//! - `agents` — like config.json's own list, merged over the bundled one for the picker
//! - `mcp_servers` — stdio servers handed to every ACP agent session
//! - a `themes/` folder — colour schemes, searched after the config's own and before the bundled
//!
//! The manifest takes the keys config.json does, plus a required `id`. A command, its
//! arguments and its `env` may say `${PLUGIN_DIR}` for the plugin's own folder:
//!
//! ```json
//! { "id": "lsp-tools",
//!   "agents": [{ "id": "mine", "name": "My agent", "command": "my-agent --acp" }],
//!   "mcp_servers": [{ "name": "lsp", "command": "${PLUGIN_DIR}/bin/lsp-mcp" }] }
//! ```
//!
//! Reading a manifest runs nothing, and what a plugin names goes through the same "Run it"
//! approval as an entry typed by hand, recorded by what will run and shown with the plugin
//! that asks. An approval is of the command line, not of the files behind it: a plugin's
//! update that keeps its command line is not asked about again.
//!
//! Who wins: config.json over a plugin, a plugin over a later plugin (by folder name). A
//! plugin can't replace a bundled agent, nor run a relative path, which would be whatever the
//! project on screen has there.
//!
//! The `screen` key of the plan's manifest is not read yet: one there is only logged.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::{self, read_json};

/// What a plugin may write for its own folder.
const PLUGIN_DIR: &str = "${PLUGIN_DIR}";

/// An installed plugin, as its manifest declares it.
pub struct Plugin {
    pub id: String,
    pub dir: PathBuf,
    manifest: Value,
}

/// The plugins installed, sorted by folder so that which of two wins is the same each run. A
/// folder without a `plugin.json` is not a plugin; one that doesn't read, has no good `id` or
/// repeats another's is logged and left out, with whatever it provides.
pub fn discover(config_dir: &Path) -> Vec<Plugin> {
    let Ok(entries) = std::fs::read_dir(config_dir.join("plugins")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("plugin.json").is_file())
        .collect();
    dirs.sort_unstable();
    let mut plugins: Vec<Plugin> = Vec::new();
    for dir in dirs {
        let folder = dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(manifest) = read_json(&dir.join("plugin.json")) else {
            log::warn!("plugins/{folder}: plugin.json did not read, so the plugin is skipped");
            continue;
        };
        let Some(id) = manifest["id"].as_str().filter(|id| valid_id(id)).map(String::from) else {
            log::warn!("plugins/{folder}: `id` must be letters, digits, `.`, `_` or `-`, so the plugin is skipped");
            continue;
        };
        if plugins.iter().any(|known| known.id == id) {
            log::warn!("plugins/{folder}: another plugin already has the id `{id}`, so this one is skipped");
            continue;
        }
        if !manifest["screen"].is_null() {
            log::warn!("plugins/{folder}: `screen` is not supported yet and is ignored");
        }
        plugins.push(Plugin { id, dir, manifest });
    }
    plugins
}

/// An `id` is shown on the approval card, so it can't carry text of its own making.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `config.json` with what the plugins provide folded in: for what starts agents and lists
/// them. Everything else reads [`config::load`], which needs no plugin folder read.
pub fn effective_config() -> Value {
    match config::dir() {
        Some(dir) => merge(config::load(), &dir),
        None => Value::Null,
    }
}

/// The config with the plugins' `agents` and `mcp_servers` on the side where the file a person
/// owns still wins the merge: ahead of its `agents` (`config::agents` keeps the last agent of
/// an `id`), behind its `mcp_servers` (`mcp_entries` keeps the first server of a `name`). A
/// config that is something else than an object is left as it is; a plugin can fold into
/// nothing there.
fn merge(config: Value, config_dir: &Path) -> Value {
    let (mut agents, mut servers) = (Vec::new(), Vec::new());
    for plugin in discover(config_dir) {
        plugin.provide("agents", "id", true, &mut agents);
        plugin.provide("mcp_servers", "name", false, &mut servers);
    }
    // Nothing to fold in: the config leaves exactly as it came, whatever it is.
    if agents.is_empty() && servers.is_empty() {
        return config;
    }
    let mut config = match config {
        Value::Object(map) => map,
        // A machine with no config.json yet: plugins come in just the same.
        Value::Null => serde_json::Map::new(),
        other => return other,
    };
    let own = |config: &serde_json::Map<String, Value>, key: &str| config.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
    if !agents.is_empty() {
        agents.extend(own(&config, "agents"));
        config.insert("agents".into(), Value::Array(agents));
    }
    if !servers.is_empty() {
        let mut combined = own(&config, "mcp_servers");
        combined.extend(servers);
        config.insert("mcp_servers".into(), Value::Array(combined));
    }
    Value::Object(config)
}

impl Plugin {
    /// Adds the manifest's list `key` to `out`, each entry resolved and marked as this plugin's.
    /// What can't be used is logged and left out, so one typo doesn't take the others away. The
    /// first plugin to use a name keeps it. An agent's `command` is split on whitespace when it
    /// is run, which `split_command` says.
    fn provide(&self, key: &str, name_field: &str, split_command: bool, out: &mut Vec<Value>) {
        let Some(entries) = self.manifest[key].as_array() else {
            return;
        };
        for entry in entries {
            let id = &self.id;
            let (Some(name), Some(command)) = (entry[name_field].as_str(), entry["command"].as_str()) else {
                log::warn!("plugin {id}: a {key} entry needs `{name_field}` and `command` as strings: {entry}");
                continue;
            };
            if out.iter().any(|known| known[name_field] == name) {
                log::warn!("plugin {id}: another plugin already provides `{name}` in {key}, so this one is left out");
                continue;
            }
            let Some(root) = self.dir.to_str() else {
                log::warn!("plugin {id}: its folder's path isn't UTF-8, so `{name}` in {key} is left out");
                continue;
            };
            if split_command && command.contains(PLUGIN_DIR) && root.contains(char::is_whitespace) {
                log::warn!("plugin {id}: `{name}` can't run from a folder with a space in its path: {root}");
                continue;
            }
            let mut entry = entry.clone();
            for field in ["command", "args", "env"] {
                if let Some(value) = entry.get_mut(field) {
                    expand(value, root);
                }
            }
            let command = entry["command"].as_str().unwrap_or_default();
            let program = if split_command { command.split_whitespace().next().unwrap_or_default() } else { command };
            if relative_path(program) {
                log::warn!("plugin {id}: `{name}` runs `{program}`, which would depend on the project; use {PLUGIN_DIR}");
                continue;
            }
            entry[config::FROM_PLUGIN] = Value::from(id.as_str());
            out.push(entry);
        }
    }
}

/// Says the plugin's folder in place of [`PLUGIN_DIR`], in every string under `value`.
fn expand(value: &mut Value, root: &str) {
    match value {
        Value::String(text) => *text = text.replace(PLUGIN_DIR, root),
        Value::Array(list) => list.iter_mut().for_each(|item| expand(item, root)),
        Value::Object(map) => map.values_mut().for_each(|item| expand(item, root)),
        _ => {}
    }
}

/// A path that means something else in each folder it is run from: commands run in the project.
/// A bare name is found on `PATH` instead, and is not one.
fn relative_path(program: &str) -> bool {
    let path = Path::new(program);
    !path.is_absolute() && path.components().count() > 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("po-plugin-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_plugin_adds_agents_and_servers_behind_the_configs_own() {
        let dir = scratch("merge");
        write(
            &dir.join("plugins/lrs/plugin.json"),
            r#"{ "id": "lrs", "agents": [{ "id": "eo", "name": "Everything", "command": "eo acp" }],
                 "mcp_servers": [{ "name": "lsp", "command": "lsp-mcp" }] }"#,
        );
        let config = merge(
            serde_json::json!({ "theme": "Rose Pine", "agents": [{ "id": "eo", "command": "mine-eo" }] }),
            &dir,
        );
        // The plugin's entry goes first, so config.json's win the merges by id.
        assert_eq!(config["agents"][0]["command"], "eo acp");
        assert_eq!(config["agents"][1]["command"], "mine-eo");
        assert_eq!(config["mcp_servers"][0]["name"], "lsp");
        assert_eq!(config["theme"], "Rose Pine");
        // And each says where it came from, which the file's own don't.
        assert_eq!(config["agents"][0][config::FROM_PLUGIN], "lrs");
        assert_eq!(config["mcp_servers"][0][config::FROM_PLUGIN], "lrs");
        assert!(config["agents"][1][config::FROM_PLUGIN].is_null());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn what_does_not_parse_is_left_out_and_the_rest_kept() {
        let dir = scratch("bad");
        write(
            &dir.join("plugins/odd/plugin.json"),
            r#"{ "id": "odd",
                 "agents": [{ "id": "no-command" }, { "command": "no-id" }, { "id": "ok", "command": "run" }],
                 "mcp_servers": [{ "name": "no-command" }, { "name": "fs", "command": "fs-mcp" }] }"#,
        );
        let config = merge(Value::Null, &dir);
        assert_eq!(config["agents"][0]["command"], "run");
        assert_eq!(config["agents"].as_array().unwrap().len(), 1);
        assert_eq!(config["mcp_servers"][0]["command"], "fs-mcp");
        assert_eq!(config.as_object().unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_a_folder_with_a_good_manifest_is_a_plugin() {
        let dir = scratch("no-manifest");
        std::fs::create_dir_all(dir.join("plugins/empty")).unwrap();
        write(&dir.join("plugins/broken/plugin.json"), "not json");
        write(&dir.join("plugins/no-id/plugin.json"), r#"{ "agents": [{ "id": "x", "command": "run" }] }"#);
        write(&dir.join("plugins/odd-id/plugin.json"), r#"{ "id": "Approved by Po", "agents": [{ "id": "x", "command": "run" }] }"#);
        assert!(discover(&dir).is_empty());
        assert_eq!(merge(Value::Null, &dir), Value::Null);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_repeated_plugin_id_is_left_out() {
        let dir = scratch("same-id");
        write(&dir.join("plugins/a/plugin.json"), r#"{ "id": "same" }"#);
        write(&dir.join("plugins/b/plugin.json"), r#"{ "id": "same" }"#);
        let plugins = discover(&dir);
        assert_eq!(plugins.len(), 1);
        assert!(plugins[0].dir.ends_with("a"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_first_plugin_wins_a_name_for_agents_servers_and_themes_alike() {
        let dir = scratch("clash");
        for name in ["a", "b"] {
            write(
                &dir.join(format!("plugins/{name}/plugin.json")),
                &format!(
                    r#"{{ "id": "{name}", "agents": [{{ "id": "x", "command": "{name}-agent" }}],
                         "mcp_servers": [{{ "name": "y", "command": "{name}-mcp" }}] }}"#
                ),
            );
            write(&dir.join(format!("plugins/{name}/themes/t.json")), "{}");
        }
        let config = merge(Value::Null, &dir);
        assert_eq!(config["agents"].as_array().unwrap().len(), 1);
        assert_eq!(config["agents"][0]["command"], "a-agent");
        assert_eq!(config["mcp_servers"].as_array().unwrap().len(), 1);
        assert_eq!(config["mcp_servers"][0]["command"], "a-mcp");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_plugin_dir_is_said_in_commands_arguments_and_env() {
        let dir = scratch("plugin-dir");
        write(
            &dir.join("plugins/p/plugin.json"),
            r#"{ "id": "p",
                 "agents": [{ "id": "a", "command": "${PLUGIN_DIR}/bin/agent --acp" }],
                 "mcp_servers": [{ "name": "s", "command": "${PLUGIN_DIR}/bin/mcp",
                                   "args": ["--data", "${PLUGIN_DIR}/data"], "env": { "HOME_OF": "${PLUGIN_DIR}" } }] }"#,
        );
        let root = dir.join("plugins/p");
        let root = root.to_str().unwrap();
        let config = merge(Value::Null, &dir);
        assert_eq!(config["agents"][0]["command"], format!("{root}/bin/agent --acp"));
        assert_eq!(config["mcp_servers"][0]["command"], format!("{root}/bin/mcp"));
        assert_eq!(config["mcp_servers"][0]["args"][1], format!("{root}/data"));
        assert_eq!(config["mcp_servers"][0]["env"]["HOME_OF"], root);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_plugin_cant_run_what_the_project_has_at_a_relative_path() {
        let dir = scratch("relative");
        write(
            &dir.join("plugins/p/plugin.json"),
            r#"{ "id": "p",
                 "agents": [{ "id": "near", "command": "./agent --acp" }, { "id": "up", "command": "tools/agent" },
                            { "id": "path", "command": "agent --flag ./here" }],
                 "mcp_servers": [{ "name": "near", "command": "./mcp" }, { "name": "path", "command": "mcp" }] }"#,
        );
        let config = merge(Value::Null, &dir);
        assert_eq!(config["agents"].as_array().unwrap().len(), 1);
        assert_eq!(config["agents"][0]["id"], "path");
        assert_eq!(config["mcp_servers"].as_array().unwrap().len(), 1);
        assert_eq!(config["mcp_servers"][0]["name"], "path");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_agent_cant_be_split_by_a_space_in_the_plugins_path() {
        let dir = scratch("with space");
        write(
            &dir.join("plugins/p/plugin.json"),
            r#"{ "id": "p",
                 "agents": [{ "id": "own", "command": "${PLUGIN_DIR}/agent" }, { "id": "path", "command": "agent" }],
                 "mcp_servers": [{ "name": "s", "command": "${PLUGIN_DIR}/mcp" }] }"#,
        );
        let config = merge(Value::Null, &dir);
        assert_eq!(config["agents"].as_array().unwrap().len(), 1);
        assert_eq!(config["agents"][0]["id"], "path");
        // A server's command is one program, whatever is in it.
        assert_eq!(config["mcp_servers"].as_array().unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_configs_own_server_name_wins_over_a_plugins() {
        let dir = scratch("mcp-file-first");
        write(
            &dir.join("plugins/p/plugin.json"),
            r#"{ "id": "p", "mcp_servers": [{ "name": "fs", "command": "plugin-mcp" }] }"#,
        );
        let config = merge(
            serde_json::json!({ "mcp_servers": [{ "name": "fs", "command": "config-mcp" }] }),
            &dir,
        );
        // `mcp_entries` keeps the first server of a name, and the file's own is first.
        assert_eq!(config["mcp_servers"][0]["command"], "config-mcp");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_config_that_is_not_an_object_is_left_alone() {
        let dir = scratch("not-object");
        write(&dir.join("plugins/a/plugin.json"), r#"{ "id": "a", "agents": [{ "id": "x", "command": "run" }] }"#);
        let config = merge(serde_json::json!([1]), &dir);
        assert_eq!(config, serde_json::json!([1]));
        std::fs::remove_dir_all(&dir).ok();
    }
}
