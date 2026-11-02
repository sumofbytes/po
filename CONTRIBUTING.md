# Contributing to po

Thanks for helping improve `po` — bug reports, docs fixes, and small focused
code changes are all welcome. If you have a larger change in mind, please open
an issue first so we can agree on scope before you write code.

## Build and check

```sh
cargo build --release   # -> target/release/po
cargo test
cargo clippy --all-targets
```

Requires a stable Rust toolchain (edition 2024). On Linux you need a C
toolchain plus the Wayland/X11, xkbcommon, fontconfig and Vulkan development
packages; `.cargo/config.toml` links with `clang` and `mold` (remove it or
override it if you don't have them).

To find where time and memory go, build with `--features hotpath` (or
`hotpath-alloc`); see [docs/profiling.md](docs/profiling.md).

## Pull requests

- Target `main`. Keep diffs minimal and focused; reuse existing helpers.
- Make sure the build, tests and clippy (no new warnings) pass — CI runs them.
- Conventional-commit style subjects (`fix:`, `feat:`, `chore:`) are preferred.
- New dependencies need a clear reason.

## License

By contributing, you agree that your contributions are licensed under
`MIT OR Apache-2.0`, the same as the project.
