//! The row of tabs above the viewer while the agent has views or forms on screen: each of them,
//! with a way to close it, the agent's offer to show one, and the window's controls. [`Snapshot`] is what [`crate::Reader`] publishes; the
//! screen draws it.

use nsh_core::prelude::*;

use super::bus::{Cmd, send};
use super::header::controls;
use super::{Colors, body, scale, text_sm};
use crate::ui::icon::IconName;

pub use crate::model::tabs::*;
pub(crate) use crate::model::tabs::SNAPSHOT;

fn tab_button(label: String, selected: bool, on_press: impl Fn() + 'static) -> View {
    let b = super::smooth(view())
        .role(AccessibilityRole::Button)
        .label(label.clone())
        .flex_none()
        .px(scale::s3())
        .py(scale::s1())
        .rounded(super::pill())
        .cursor_pointer()
        .on_press(on_press)
        .child(body(label).number_of_lines(1));
    if selected { b.bg(Colors::selected()) } else { b.hover(|s| s.bg(Colors::hover())) }
}

#[component]
pub fn Tabs() -> impl IntoElement {
    let state = signal(SNAPSHOT.borrow().clone());
    spawn(async move {
        let mut snapshot = SNAPSHOT.subscribe();
        // What was published before this ran would otherwise never arrive.
        snapshot.mark_changed();
        while snapshot.changed().await.is_ok() {
            let now = snapshot.borrow_and_update().clone();
            state.set(now);
        }
    });

    view()
        .flex_row()
        .items_center()
        .gap(scale::s1())
        .px(scale::s3())
        .py(scale::s1_5())
        .border_b_1()
        .border_color(super::hairline())
        .child(dynamic(move || view().flex_row().items_center().gap(scale::s1()).child(state.with(|s| controls(&s.controls.0).collect::<Vec<_>>()))))
        .child(dynamic(move || {
            let (tabs, shown) = state.with(|s| (s.tabs.clone(), s.shown.clone()));
            view().flex_row().items_center().gap(scale::s1()).child(
                tabs.into_iter()
                    .map(|tab| {
                        let (open, close) = (tab.id.clone(), tab.id.clone());
                        view()
                            .flex_row()
                            .items_center()
                            .child(tab_button(tab.title, shown.as_deref() == Some(&tab.id), move || send(Cmd::ShowView(open.clone()))))
                            .child(super::icon_button(IconName::Close, scale::s2_5(), "Close this view", move || send(Cmd::CloseView(close.clone()))))
                    })
                    .collect::<Vec<_>>(),
            )
        }))
        .child(dynamic(move || {
            state.with(|s| s.offer.clone()).map(|offer| {
                let id = offer.id.clone();
                let label = format!("The agent wants to show “{}”", offer.title);
                super::smooth(view())
                    .role(AccessibilityRole::Button)
                    .label(label.clone())
                    .flex_none()
                    .px(scale::s3())
                    .py(scale::s1())
                    .rounded(super::pill())
                    .cursor_pointer()
                    .bg(Colors::primary())
                    .on_press(move || send(Cmd::ShowView(id.clone())))
                    .child(text(label).number_of_lines(1).text_size(text_sm()).text_color(Colors::primary_foreground()))
            })
        }))
        .child(view().flex_1())
        .child(dynamic(move || view().flex_row().items_center().gap(scale::s1()).child(state.with(|s| controls(&s.controls.1).collect::<Vec<_>>()))))
}

#[cfg(test)]
mod tests {
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;
    use crate::model::header::Control;

    #[test]
    fn the_tabs_switch_close_and_take_up_an_offer() {
        let mut guard = super::super::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        publish(Snapshot {
            shown: Some("a".into()),
            tabs: vec![Tab { id: "a".into(), title: "Notes".into() }],
            offer: Some(Tab { id: "b".into(), title: "Plan".into() }),
            controls: (vec![], vec![Control::Viewer(true)]),
        });
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Tabs>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(600.), Points(100.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app.settle();
        for shown in ["Notes", "The agent wants to show “Plan”"] {
            assert!(app.shows(shown), "{shown} is on screen: {:?}", app.screen().texts());
        }
        for (press, cmd) in [
            ("label:Notes", Cmd::ShowView("a".into())),
            ("label:Close this view", Cmd::CloseView("a".into())),
            ("label:The agent wants to show “Plan”", Cmd::ShowView("b".into())),
            ("label:Viewer (Ctrl+Shift+B)", Cmd::ToggleRight),
        ] {
            app.press(press).unwrap();
            app.settle();
            assert_eq!(commands.try_recv().ok(), Some(cmd));
        }
    }
}
