//! Foundation of the Rune port: an empty window in Po's Rose Pine colours.
//! `cargo run --bin po-rune` opens it; `-- --snap po.png` renders it headless.

use rune::{AppSpec, Cx, El, col, color, text};
use rune::tok::type_;

#[derive(Default)]
struct State;

/// Rose Pine, Po's default theme.
const BASE: &str = "#191724";
const TEXT: &str = "#e0def4";

fn view(_: &State, _: &mut Cx<State>) -> El {
    let c = |hex: &str| color::hex(hex).expect("a valid colour");
    col().w_full().h_full().center().fill(c(BASE))
        .child(text("Po").font(type_::TITLE).color(c(TEXT)))
}

fn main() {
    rune::run(AppSpec::<State>::new("Po", (1200.0, 800.0), State::default, view));
}
