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

## Configuration

`po` reads `$XDG_CONFIG_HOME/po/config.json` (default `~/.config/po/`). See
[`config.example.json`](config.example.json) for theme, fonts and agents;
[`agents.json`](agents.json) lists the default agent commands. Any command that
speaks ACP over stdio works (`opencode acp`, `dex acp`, `claude-agent-acp`, …).
Themes are JSON files in `<config>/themes/`; the bundled ones are in
[`themes/`](themes).

## License

Dual-licensed under [MIT](LICENSE) or [Apache-2.0](LICENSE-APACHE), at your
option. Bundled third-party material keeps its own license: `vendor/gpui-pre-wgpu`
(Apache-2.0), `src/viewer/languages` queries (Apache-2.0), and the Lucide icons
in `assets/icons` (ISC).

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).
