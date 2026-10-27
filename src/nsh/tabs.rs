//! The row of tabs above the viewer while the agent has views or forms on screen: the files (or
//! the changes) and each view of the agent's, with a way to close it, and the agent's offer to
//! show one. [`Snapshot`] is what [`crate::Reader`] publishes; the screen draws it.
use std::sync::LazyLock;

use nsh_core::prelude::*;
use tokio::sync::watch;

use super::bus::{Cmd, send};
use super::{Colors, rems, text_sm};
use crate::ui::icon::IconName;

#[derive(Clone, PartialEq, Debug)]
pub struct Tab {
    pub id: String,
    pub title: String,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    /// What the first tab is called: the files, or the changes.
    pub home: String,
    /// The tab on screen; none is the first.
    pub shown: Option<String>,
    pub tabs: Vec<Tab>,
    /// A view the agent offers to show, by its title.
    pub offer: Option<Tab>,
}

static SNAPSHOT: LazyLock<watch::Sender<Snapshot>> = LazyLock::new(|| watch::Sender::new(Snapshot::default()));

/// Shows `snapshot`; nothing is redrawn when it is what the screen already has.
pub fn publish(snapshot: Snapshot) {
    SNAPSHOT.send_if_modified(|now| {
        let changed = *now != snapshot;
        if changed {
            *now = snapshot;
        }
        changed
    });
}

fn tab_button(label: String, selected: bool, on_press: impl Fn() + 'static) -> View {
    let b = view()
        .role(AccessibilityRole::Button)
        .label(label.clone())
        .flex_none()
        .px(rems(0.5))
        .py(rems(0.25))
        .rounded(Colors::radius())
        .cursor_pointer()
        .on_press(on_press)
        .child(text(label).number_of_lines(1).text_size(text_sm()).text_color(Colors::foreground()));
    if selected { b.bg(Colors::selected()) } else { b.hover(|s| s.bg(Colors::hover())) }
}

#[component]
pub fn Tabs() -> impl IntoElement {
    let state = signal(SNAPSHOT.borrow().clone());
    spawn(async move {
        let mut snapshot = SNAPSHOT.subscribe();
        while snapshot.changed().await.is_ok() {
            let now = snapshot.borrow_and_update().clone();
            state.set(now);
        }
    });

    view()
        .flex_row()
        .items_center()
        .gap(rems(0.25))
        .px(rems(0.5))
        .py(rems(0.25))
        .border_b_1()
        .border_color(Colors::border())
        .child(dynamic(move || {
            let (home, shown) = state.with(|s| (s.home.clone(), s.shown.is_none()));
            tab_button(home, shown, || send(Cmd::ShowHome))
        }))
        .child(dynamic(move || {
            let (tabs, shown) = state.with(|s| (s.tabs.clone(), s.shown.clone()));
            view().flex_row().items_center().gap(rems(0.25)).child(
                tabs.into_iter()
                    .map(|tab| {
                        let (open, close) = (tab.id.clone(), tab.id.clone());
                        view()
                            .flex_row()
                            .items_center()
                            .child(tab_button(tab.title, shown.as_deref() == Some(&tab.id), move || send(Cmd::ShowView(open.clone()))))
                            .child(super::icon_button(IconName::Close, rems(0.625), "Close this view", move || send(Cmd::CloseView(close.clone()))))
                    })
                    .collect::<Vec<_>>(),
            )
        }))
        .child(dynamic(move || {
            state.with(|s| s.offer.clone()).map(|offer| {
                let id = offer.id.clone();
                let label = format!("The agent wants to show “{}”", offer.title);
                view()
                    .role(AccessibilityRole::Button)
                    .label(label.clone())
                    .flex_none()
                    .px(rems(0.5))
                    .py(rems(0.25))
                    .rounded(Colors::radius())
                    .cursor_pointer()
                    .bg(Colors::primary())
                    .on_press(move || send(Cmd::ShowView(id.clone())))
                    .child(text(label).number_of_lines(1).text_size(text_sm()).text_color(Colors::primary_foreground()))
            })
        }))
}

#[cfg(test)]
mod tests {
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;

    #[test]
    fn the_tabs_switch_close_and_take_up_an_offer() {
        let mut guard = super::super::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        publish(Snapshot {
            home: "Files".into(),
            shown: Some("a".into()),
            tabs: vec![Tab { id: "a".into(), title: "Notes".into() }],
            offer: Some(Tab { id: "b".into(), title: "Plan".into() }),
        });
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Tabs>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(600.), Points(100.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app.settle();
        for shown in ["Files", "Notes", "The agent wants to show “Plan”"] {
            assert!(app.shows(shown), "{shown} is on screen: {:?}", app.screen().texts());
        }
        for (press, cmd) in [
            ("label:Files", Cmd::ShowHome),
            ("label:Notes", Cmd::ShowView("a".into())),
            ("label:Close this view", Cmd::CloseView("a".into())),
            ("label:The agent wants to show “Plan”", Cmd::ShowView("b".into())),
        ] {
            app.press(press).unwrap();
            app.settle();
            assert_eq!(commands.try_recv().ok(), Some(cmd));
        }
    }
}
