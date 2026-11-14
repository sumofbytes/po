# Plugins

Goal: anyone can extend po without forking it, and an agent working inside po
can author a plugin that appears in the app. Plugins that draw are
[TSX](https://nsh.dev): no Rust toolchain and no recompile of po.

po already runs on "the app is a shell and the features are programs talking
to it":

- An **agent** is any program speaking [ACP](https://agentclientprotocol.com)
  (or a dex daemon) over stdio, listed in `agents.json` (`src/config/mod.rs:agents`).
- **Tools** are MCP servers handed to every agent session (`mcp_servers`),
  plus po's own control server (`src/ipc/control.rs`).
- **Themes** are JSON files in `<config>/themes/`, watched and applied without
  a restart (`src/theme.rs:watch`).
- **Screens** are NSH apps: po embeds the engine with `nsh::mount`
  (`src/nsh/mod.rs`), one `AppView` per pane.

One manifest covers all four. It does not make them one mechanism: they differ
in where code runs, and that decides what they may be trusted with.

| Kind | Runs | Held back by |
|---|---|---|
| Theme | Nothing runs: data | Nothing needed |
| Agent, MCP server | Its own process, as the user | The **Run it** approval |
| Screen | A QuickJS VM inside po | Capabilities po grants (to be built) |
| Built-in screen | Compiled Rust in po | Code review; it is po |

## What "everything is a plugin" means here

[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) is the
reference for the phrase: on [Cordis](https://github.com/cordiverse/cordis),
every part of it, down to the agent loop, is a plugin in one JS process that
contributes services and events to a shared context.

**Take the composition and lifecycle; leave the in-process execution.**

Two differences decide this:

1. **The harness owns the agent loop; po does not.** Its plugins intercept model
   requests and tool calls because those happen inside it. In po they happen
   inside the ACP agent, so there is no loop to plug into. What po owns is the
   window, the panes, the approvals and which programs are launched, so that is
   what its plugins extend.
2. **Rust has no equivalent of loading a JS package.** Its plugins run in-process,
   outside the workspace sandbox, by its own account. The same thing in Rust is
   a `dylib`, which has no stable ABI, must match po's compiler and GPUI build,
   and is unsandboxed. Out-of-process programs and QuickJS keep extension apart
   from the toolchain.

So built-ins and plugins share registries and precedence, rather than a runtime:

| Harness idea | In po |
|---|---|
| Ordered layers: bundles, profile patch, home patch | One order: bundled, plugins, `config.json`. Add `po --dump-config` to print the result with each entry's source |
| Rows patched by `id` | Already how `agents` and `mcp_servers` merge |
| Services on `ctx`, with `inject` | Typed registries in Rust: agents, themes, panes. Compile-time checked, with no runtime lookup |
| Slots: a feature contributes UI and never imports another's | A pane registry. Native and TSX screens register the same way |
| Registrations are reversible effects | An RAII guard: dropping it unregisters. GPUI's `Subscription` already works so, and on/off and hot reload need nothing more |
| `dsh.manifestVersion`, `engines.dsh` | `manifest_version` and `engines.po`, checked at load, added with the screen key |
| A failed bundle stays listed with its error and can be turned off | The same on the plugins list: never make the user hunt through a log |
| Inspect a spec before installing; confirm its source | The same for `po plugin add` |

Not taken: replaceable core services, an interception pipeline, and npm-style
dependency resolution. The harness's duplicate-module failures come from that
resolution, and po's plugins have no shared in-process libraries to duplicate.

## The plugin unit

`$XDG_CONFIG_HOME/po/plugins/<folder>/` with one `plugin.json`:

```json
{ "id": "lsp-tools",
  "name": "LSP tools",
  "agents":      [{ "id": "mine", "name": "My agent", "command": "my-agent --acp" }],
  "mcp_servers": [{ "name": "lsp", "command": "${PLUGIN_DIR}/bin/lsp-mcp" }],
  "screen": { "title": "Issues Pro", "app": "dist/app.js" } }
```

- `id` is required: letters, digits, `.`, `_`, `-`. It is what the approval card
  shows and what `disabled_plugins` will name.
- `agents` and `mcp_servers` reuse config.json's shapes, read by
  `src/config/plugin.rs`. `${PLUGIN_DIR}` in a command, its arguments or its `env` is
  the plugin's folder. Other relative paths are refused, because commands run
  from the project and would mean something else in each one.
- A plugin's `themes/` folder is searched after the config folder's own and
  before the bundled schemes.
- `screen` is not read yet. It names built JS, not TSX, so installing a plugin
  needs no toolchain.

**Who wins:** `config.json` over a plugin, and a plugin over a later one by
folder name, for agents, servers and themes alike. A plugin can't replace a
bundled agent, and its agents are always asked about, even with a bundled
command.

**A plugin is not only data.** Reading its manifest runs nothing, but it names
programs that run as the user. Run it shows the plugin and the exact command.
The approval is of the command line, not the files behind it, so an update that
keeps the line is not asked about again. `po plugin add` should say so, and
pinning a plugin's commit is the way to close that gap.

## TSX screens

Pieces already in `vendor/nsh-platform-gpui`, behind its `js` feature, which po
turns off today (`Cargo.toml`: "no QuickJS"). Turning it on adds QuickJS's C
build and binary size for every user: measure startup before committing.

- `AppView::load(App::Js(Bundle::source(code)))` runs a bundle in a view.
- `AppView::register_component` makes po's views usable from a plugin.
- `AppView::host()` is the `host.call` / `host.on` channel. Handlers run on the
  engine thread.
- `set_theme` follows po's theme, as `nsh::mount` does for its own screens.

### Three rules for the sandbox

**1. Nothing is granted by default.** `Bundle::source` carries `Policy::default()`,
and in `nsh-engine` no declaration means *everything*. po must set
`Policy::granted` on every plugin bundle. The grant is held by po and given in
its UI. It is never read from the manifest, or an agent could write itself a
plugin with what it was refused.

**2. Plugins call `command.rs`, never `nsh::bus::Cmd`.** The bus is what the UI's
own buttons send, including `Chat(Approve)`, `Chat(Answer)`, `Chat(Grant)`,
`MergeWorktree` and `DiscardWorktree`. A plugin that could send those could press
Run it itself and raise an agent's grant. `Command` is already the vocabulary for
callers outside the UI, so a plugin is one more: `Caller::Plugin(id)` with a
`Grant`, checked the way an agent's is.

**3. Plugins read through the same door.** A `Snapshot` is an in-process Rust
value and is not an API. The `po ctl watch` subscribers already send state
changes through `command`; `host.send` delivers them to a plugin.

Actions go through `host.call`. Only views, like `po/issues`, are components.

## Phases

1. **Shipped.** Plugin folder, `id`, `agents`, `mcp_servers`, `${PLUGIN_DIR}` and
   themes, with the plugin named on the approval card.
2. **Pane registry.** The viewer learns its panes from a registry, and the
   terminal moves onto it. It is a hand-wired special case today: its fields
   on `Project`, its `Cmd`, its toggle and its render branch.
3. **Plugin API.** `Caller::Plugin`, deny-by-default capabilities, the event feed.
4. **The screen mount.** Turn on `js` once measured. `nsh::mount` takes an
   `App`, so a screen is a Rust root or a bundle. Add `manifest_version` and
   `engines.po`. First native component: `po/issues`.
5. **Plugin management.** A list on the settings screen: what each provides,
   where it sits, its error if any, on/off (`"disabled_plugins": []`).
   `po plugin add <git-url>` shows what it would install and asks first.
   Signatures say who published a package and can pre-fill that list, but do not
   skip Run it: they do not say a command is safe.
6. **Agent-authored plugins.** An agent writes the manifest and TSX and runs
   `nsh build`; `show_diff` lets the user read it first. The user gives the grant
   and answers Run it, as for any other plugin.
7. **Built-ins share the registries.** `agents.json` and `schemes.txt` become a
   built-in manifest at the bottom of the order, and native screens register as
   panes. They stay compiled: chat and the sidebar depend on `Reader` and
   in-process state, and moving them to TSX would add serialization and cost
   performance for no gain to users.

## What stays compiled

The window, renderer, theme application, ACP/MCP plumbing, the control server,
the approval flow, and the built-in screens. Everything a user or agent adds is a
folder with a manifest: TSX where it draws, JSON where it declares.
