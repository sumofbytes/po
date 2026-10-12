# po

A native desktop GUI (built on [GPUI](https://crates.io/crates/gpui-pre)) for
coding agents that speak the [Agent Client Protocol](https://agentclientprotocol.com).
Chat with an agent, browse the project's files with a syntax-highlighted viewer,
and review the agent's plans and edit diffs side by side.

## Build

Requires a stable Rust toolchain (edition 2024). On Linux, install a C
toolchain plus Wayland/X11, xkbcommon, fontconfig and Vulkan development
packages. `.cargo/config.toml` links with `clang` + `mold`; delete it if you
don't have them.

```sh
cargo run --release
```

## Usage

```sh
po [path]                 # open a project (default: current directory)
po --agent "dex acp" .    # pick the agent command for this launch
po --pr 123               # check out a GitHub PR (number or URL) and review it
po -f [path]              # stay in the foreground instead of detaching
```

By default `po` detaches from the terminal, like `code .`; output goes to
`$XDG_CACHE_HOME/po/po.log` (default `~/.cache/po/po.log`). Environment
variables: `PO_AGENT` (default agent command, same as `--agent`), `PO_THEME`
(theme name, overrides the config), `PO_TRACE=1` (print startup timings).
Detaching is Unix-only for now.

## Configuration

Everything lives in `$XDG_CONFIG_HOME/po/` (default `~/.config/po/`). All of it
is optional. `config.json` looks like
[`config.example.json`](config.example.json):

```json
{
  "theme": "main",
  "font": { "family": "Adwaita Sans", "size": 14, "mono_family": "Noto Sans Mono", "mono_size": 13 },
  "agents": [{ "id": "mine", "name": "My agent", "command": "my-agent --acp" }]
}
```

### Coding agents

Any program that speaks ACP over stdio works. `po` ships with entries for
`opencode acp`, `dex acp`, `exsa-acp` and `claude-agent-acp`
([`agents.json`](agents.json)); install the one you want and it must be on your
`PATH`. Add your own under `agents` in `config.json`: `id` and `command` are
required, `name` defaults to the `id`. An entry whose `id` matches a bundled
one replaces it, so you can change flags or the binary path. `command` is split
on whitespace into program and arguments (no shell quoting). Choose the agent
in the app, or pass `--agent` / `PO_AGENT`.

### Themes

Set `"theme"` to a theme name. On first start `po` copies the bundled themes
(`main`, `moon`, `dawn`, from [`themes/`](themes)) into `<config>/themes/`;
existing files are never overwritten, so edit them freely. A name is looked up
as `<config>/themes/<name>.json`, then `<config>/<name>.json`, then the bundled
theme; an absolute path to a JSON file also works. To make your own, copy one
of the bundled files, rename it, and set `"theme"` to its name. An unknown
theme falls back to `main`. The `font` block overrides the theme's UI and
monospace fonts and sizes.

## License

Dual-licensed under [MIT](LICENSE) or [Apache-2.0](LICENSE-APACHE), at your
option. Bundled third-party material keeps its own license: `vendor/gpui-pre-wgpu`
(Apache-2.0), `src/viewer/languages` queries (Apache-2.0), and the Lucide icons
in `assets/icons` (ISC).

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).
