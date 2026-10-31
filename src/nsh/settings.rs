//! The settings page: the options of `config.json` as fields, written back to it on Save. What
//! the fields hold and how it is written is [`crate::settings`]; this is the screen.
use std::sync::Mutex;

use nsh_core::prelude::*;

use super::bus::{Cmd, send};
use super::overlay::{self, Height};
use super::{Colors, body, button, caption, field, field_height, scale, text_xs};
use crate::schemes;
use crate::settings::{AgentForm, Form};

/// The height the screen needs, which the overlay around it takes.
pub static HEIGHT: Height = overlay::height();

/// Where the viewer is as the page opens, which is what its "Viewer position" starts from.
static VIEWER_LEFT: Mutex<bool> = Mutex::new(false);

/// Sets what the next page starts from.
pub fn start(viewer_left: bool) {
    *VIEWER_LEFT.lock().unwrap_or_else(|e| e.into_inner()) = viewer_left;
}

/// An agent's fields, known by a number the page gives it so a row keeps its input while others
/// come and go.
#[derive(Clone, PartialEq)]
struct Agent {
    key: u64,
    form: AgentForm,
}

/// A group's eyebrow: small, upper-case and quiet, with room above it to set the groups apart.
fn section(title: &'static str) -> View {
    view().pt(scale::s8()).pb(scale::s1()).child(caption(title.to_uppercase()).font_weight(FontWeight::MEDIUM))
}

/// The rows of a group: bare, set apart by hairlines alone, so the page is one surface and not
/// cards holding rows holding boxes.
fn group() -> View {
    view().flex_col()
}

/// The line between two rows of a group.
fn divider() -> View {
    view().h(Points(1.)).flex_none().bg(super::hairline())
}

/// A label on the left and its control on the right.
fn row(label: &'static str, hint: &'static str, control: impl IntoElement) -> View {
    view()
        .flex_row()
        .items_center()
        .gap(scale::s6())
        .py(scale::s3())
        .child(view().flex_1().min_w_0().gap(scale::s0_5()).child(body(label)).child(caption(hint)))
        .child(view().w(Points(260.)).flex_none().child(control))
}

/// An on / off switch, which says which in its label.
fn switch(on: Signal<bool>, label: &'static str) -> View {
    let state = move || format!("{label}, {}", if on.get() { "on" } else { "off" });
    let knob = super::smooth(view())
        .w(scale::s4())
        .h(scale::s4())
        .flex_none()
        .rounded(super::pill())
        .bg(Colors::popover())
        .style_when(move || on.get(), |s| s.bg(Colors::primary_foreground()));
    let track = super::smooth(view())
        .role(AccessibilityRole::Switch)
        .label(state)
        .flex_none()
        .w(scale::s8())
        .h(scale::s5())
        .px(scale::s0_5())
        .flex_row()
        .items_center()
        .rounded(super::pill())
        .cursor_pointer()
        .bg(Colors::input_border())
        .style_when(move || on.get(), |s| s.bg(Colors::primary()).justify_end())
        .on_press(move || on.update(|on| *on = !*on))
        .child(knob);
    view().flex_row().justify_end().child(track)
}

/// A choice of two, the chosen one lifted: `true` is the first.
fn choice(first: Signal<bool>, labels: (&'static str, &'static str)) -> View {
    let option = move |name: &'static str, value: bool| {
        view()
            .role(AccessibilityRole::Button)
            .label(name)
            .px(scale::s4())
            .py(scale::s1())
            .rounded(super::pill())
            .cursor_pointer()
            .style_when(move || first.get() == value, |s| s.bg(Colors::selected()))
            .hover(|s| s.bg(Colors::hover()))
            .on_press(move || first.set(value))
            .child(body(name))
    };
    view().flex_row().justify_end().child(view().flex_row().gap(scale::s0_5()).child(option(labels.0, true)).child(option(labels.1, false)))
}

/// What the columns of the agent rows hold, once above them, since a placeholder is gone as soon
/// as the field has text. The fills and the gap are those of a row.
fn agent_columns() -> View {
    let column = |grow: f32, name: &'static str| view().flex_grow(grow).basis(Points(0.)).min_w_0().px(scale::s3()).child(caption(name));
    view()
        .flex_row()
        .gap(scale::s2())
        .pt(scale::s1())
        .child(column(2., "Name"))
        .child(column(2., "ID"))
        .child(column(5., "Command"))
        // The room of the remove button.
        .child(view().w(scale::s6()).flex_none())
}

#[component]
pub fn Settings() -> impl IntoElement {
    let loaded = Form::load(*VIEWER_LEFT.lock().unwrap_or_else(|e| e.into_inner()));
    let theme = signal(loaded.theme.clone());
    let font_family = signal(loaded.font_family.clone());
    let font_size = signal(loaded.font_size.clone());
    let mono_family = signal(loaded.mono_family.clone());
    let mono_size = signal(loaded.mono_size.clone());
    let idle_minutes = signal(loaded.idle_minutes.clone());
    let max_live = signal(loaded.max_live.clone());
    let notifications = signal(loaded.notifications);
    let glass = signal(loaded.glass);
    let viewer_left = signal(loaded.viewer_left);
    let next_key = stored(loaded.agents.len() as u64);
    let agents = signal(loaded.agents.iter().cloned().enumerate().map(|(key, form)| Agent { key: key as u64, form }).collect::<Vec<_>>());
    let error = signal(None::<String>);

    let save = move || {
        let form = Form {
            theme: theme.get_untracked(),
            font_family: font_family.get_untracked(),
            font_size: font_size.get_untracked(),
            mono_family: mono_family.get_untracked(),
            mono_size: mono_size.get_untracked(),
            idle_minutes: idle_minutes.get_untracked(),
            max_live: max_live.get_untracked(),
            notifications: notifications.get_untracked(),
            glass: glass.get_untracked(),
            viewer_left: viewer_left.get_untracked(),
            agents: agents.get_untracked().into_iter().map(|agent| agent.form).collect(),
        };
        match form.save() {
            Ok(()) => {
                error.set(None);
                send(Cmd::SettingsSaved);
            }
            Err(message) => error.set(Some(message)),
        }
    };
    on_key_with("escape", KeyOptions { when: nsh_core::keymap::When::Always, repeat: false }, |_| send(Cmd::CloseSettings));

    // The frame around the screen takes the height its parts need, as far as the window has room.
    let parts: [Signal<f32>; 3] = [signal(0.), signal(0.), signal(0.)];
    effect(move || {
        let height: f32 = parts.iter().map(|part| part.get()).sum();
        HEIGHT.send_if_modified(|now| std::mem::replace(now, height) != height);
    });
    let measure = move |part: usize| move |bounds: &Bounds<Points>| parts[part].set(bounds.size.height.0);

    // The theme's matches follow what is typed.
    let matches = move || {
        let typed = theme.get().trim().to_string();
        if typed.is_empty() || schemes::names().any(|name| name.eq_ignore_ascii_case(&typed)) {
            return vec![];
        }
        let needle = typed.to_lowercase();
        schemes::names().filter(|name| name.to_lowercase().contains(&needle)).take(6).collect::<Vec<_>>()
    };

    let edit = move |key: u64, change: fn(&mut AgentForm, String)| {
        move |text: &str| {
            agents.update(|list| {
                if let Some(agent) = list.iter_mut().find(|agent| agent.key == key) {
                    change(&mut agent.form, text.to_string());
                }
            })
        }
    };
    let agent_row = move |agent: ReadSignal<Agent>, _: ReadSignal<usize>| {
        let key = agent.get_untracked().key;
        // One bordered box holding the bare input; its placeholder says what it is.
        let input = move |grow: f32, id: &str, placeholder: &'static str, value: fn(&AgentForm) -> String, change: fn(&mut AgentForm, String)| {
            view().flex_grow(grow).basis(Points(0.)).min_w_0().h(field_height()).flex_row().items_center().px(scale::s3()).rounded(super::card_radius()).border_1().border_color(Colors::input_border()).bg(Colors::input_background()).child(
                nsh_core::element::input()
                    .test_id(&format!("agent-{key}-{id}"))
                    .value(move || value(&agent.get().form))
                    .placeholder(placeholder)
                    .placeholder_color(Colors::one(Colors::muted()))
                    .selection_color(Colors::one(Colors::caret()))
                    .text_color(Colors::foreground())
                    .auto_correct(false)
                    .on_change(edit(key, change))
                    .flex_1()
                    .min_w(Points(0.)),
            )
        };
        view()
            .flex_row()
            .items_center()
            .gap(scale::s2())
            .py(scale::s2())
            .child(input(2., "name", "Name", |a| a.name.clone(), |a, t| a.name = t))
            .child(input(2., "id", "ID", |a| a.id.clone(), |a, t| a.id = t))
            .child(input(5., "command", "command --acp", |a| a.command.clone(), |a, t| a.command = t))
            .child(super::icon_button(crate::ui::icon::IconName::Close, scale::s3(), "Remove", move || agents.update(|list| list.retain(|a| a.key != key))))
    };

    view()
        .flex_1()
        .bg(Colors::popover())
        .child(
            view()
                .px(scale::s8())
                .pt(scale::s6())
                .pb(scale::s4())
                .gap(scale::s0_5())
                .border_b_1()
                .border_color(super::hairline())
                .on_layout(measure(0))
                .child(text("Settings").text_size(scale::s5()).line_height(super::leading(scale::s5())).font_weight(FontWeight::SEMIBOLD).text_color(Colors::foreground()))
                .child(caption("How Po looks and behaves")),
        )
        .child(
            scroll().flex_shrink(1.).child(
                view()
                    .px(scale::s8())
                    .pb(scale::s6())
                    .on_layout(measure(1))
                    .child(section("Appearance"))
                    .child(group()
                    .child(row("Theme", "A colour scheme name or theme file", field("settings-theme", theme, "Rose Pine", || {})))
                    .child(dynamic(move || {
                        let names = matches();
                        (!names.is_empty()).then(|| {
                            view().pb(scale::s1()).child(
                                names
                                    .into_iter()
                                    .map(|name| {
                                        view()
                                            .role(AccessibilityRole::Button)
                                            .label(name)
                                            .px(scale::s3())
                                            .py(scale::s1())
                                            .rounded(Colors::radius())
                                            .cursor_pointer()
                                            .hover(|s| s.bg(Colors::hover()))
                                            .on_press(move || theme.set(name.to_string()))
                                            .child(body(name))
                                    })
                                    .collect::<Vec<_>>(),
                            )
                        })
                    }))
                    .child(divider())
                    .child(row("Glass", "Translucent panes over the desktop", switch(glass, "Glass")))
                    .child(divider())
                    .child(row("UI font", "Names tried in order, comma separated", field("settings-font-family", font_family, "Adwaita Sans", || {})))
                    .child(divider())
                    .child(row("UI font size", "Also scales spacing", field("settings-font-size", font_size, "14", || {})))
                    .child(divider())
                    .child(row("Monospace font", "Names tried in order, comma separated", field("settings-mono-family", mono_family, "system monospace", || {})))
                    .child(divider())
                    .child(row("Monospace font size", "Code and diffs", field("settings-mono-size", mono_size, "13", || {}))))
                    .child(section("Layout"))
                    .child(group().child(row("Viewer position", "Which side of the conversation (Ctrl+Shift+S)", choice(viewer_left, ("Left", "Right")))))
                    .child(section("Behavior"))
                    .child(
                        group()
                            .child(row("Notifications", "When a thread needs you in the background", switch(notifications, "Notifications")))
                            .child(divider())
                            .child(row("Idle minutes", "Stop an idle agent after this long; 0 never", field("settings-idle", idle_minutes, "15", || {})))
                            .child(divider())
                            .child(row("Max live agents", "Longest idle agents stop beyond this many", field("settings-max-live", max_live, "8", || {}))),
                    )
                    .child(section("Agents"))
                    .child(caption("Added to the bundled agents; the same id replaces one. A new command asks before it first runs.").pb(scale::s2()))
                    .child(
                        view()
                            .flex_col()
                            .child(agent_columns())
                            .child(each(agents, |agent: &Agent| agent.key, agent_row))
                            .child(view().pt(scale::s2()).flex_row().child(button("Add agent", false, move || {
                                let key = next_key.replace(next_key.get() + 1);
                                agents.update(|list| list.push(Agent { key, form: AgentForm { id: String::new(), name: String::new(), command: String::new(), extra: Default::default() } }));
                            }))),
                    ),
            ),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(scale::s2())
                .px(scale::s8())
                .py(scale::s3())
                .border_t_1()
                .border_color(super::hairline())
                .on_layout(measure(2))
                .child(
                    text(move || error.get().unwrap_or_else(|| "Saved to config.json; other keys in it are kept. MCP servers are edited there.".into()))
                        .flex_1()
                        .text_size(text_xs())
                        .text_color(move || if error.get().is_some() { Colors::danger()() } else { Colors::muted()() }),
                )
                .child(button("Close", false, || send(Cmd::CloseSettings)))
                .child(button("Save", true, save)),
        )
}

#[cfg(test)]
mod tests {
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;

    /// Never saves: a valid Save would write the real `config.json`.
    #[test]
    fn the_page_checks_its_fields_adds_agents_and_closes() {
        let mut guard = super::super::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Settings>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(680.), Points(900.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        for shown in ["Settings", "APPEARANCE", "Theme", "UI font size", "Notifications", "AGENTS", "Add agent", "Close", "Save"] {
            assert!(app.shows(shown), "{shown} is on screen: {:?}", app.screen().texts());
        }

        // A number field that is not a number is refused, and nothing is written.
        app.type_text("id:settings-font-size", "big").unwrap();
        app.press("label:Save").unwrap();
        app.settle();
        assert!(app.shows("Font size needs a number"), "{:?}", app.screen().texts());
        assert!(commands.try_recv().is_err(), "nothing saved, so nothing to apply");

        // A scheme typed in part offers its matches; pressing one fills the field.
        app.clear("id:settings-theme").unwrap();
        app.type_text("id:settings-theme", "rose").unwrap();
        app.settle();
        let first = schemes::names().find(|name| name.to_lowercase().contains("rose")).expect("a bundled rose scheme");
        app.press(&format!("label:{first}")).unwrap();
        app.settle();
        assert_eq!(app.screen().input_text("id:settings-theme").unwrap(), Some(first));

        // A toggle flips, and an agent row comes and goes.
        app.press("label:Left").ok();
        let before = app.screen().count("role:button[Remove]").unwrap();
        app.press("label:Add agent").unwrap();
        app.settle();
        assert_eq!(app.screen().count("role:button[Remove]").unwrap(), before + 1);
        app.type_text(&format!("id:agent-{before}-id"), "mine").unwrap();
        app.settle();
        assert_eq!(app.screen().input_text(&format!("id:agent-{before}-id")).unwrap(), Some("mine"));
        app.press(&format!("role:button[Remove] >> nth={before}")).unwrap();
        app.settle();
        assert_eq!(app.screen().count("role:button[Remove]").unwrap(), before);

        // Close, and Esc, both ask the frame to close it.
        app.press("label:Close").unwrap();
        app.settle();
        assert_eq!(commands.try_recv().ok(), Some(Cmd::CloseSettings));
        app.press_key("escape").unwrap();
        app.settle();
        assert_eq!(commands.try_recv().ok(), Some(Cmd::CloseSettings));
    }
}
