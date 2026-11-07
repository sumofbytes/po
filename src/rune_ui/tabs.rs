//! The agent's views as tabs, with the prompt for one it offered while the user was busy.
//! Counterpart of `src/nsh/tabs.rs`.
use rune::tok::TextStyle;
use rune::{El, row, text};
use rune_kit::{Button, ButtonVariant, Kit};

use crate::model::bus::{Cmd, send};
use crate::model::tabs::Snapshot;

const CAPTION: TextStyle = TextStyle::new(12.0, 400.0);

pub fn view<S: 'static>(snapshot: &Snapshot, k: &Kit) -> El {
    let mut strip = row().w_full().items_center().gap(4.0).px(8.0).py(4.0).child(super::header::controls::<S>(k, &snapshot.controls.0));
    for tab in &snapshot.tabs {
        let (open, close) = (tab.id.clone(), tab.id.clone());
        let selected = snapshot.shown.as_deref() == Some(tab.id.as_str());
        let variant = if selected { ButtonVariant::Neutral } else { ButtonVariant::Ghost };
        strip = strip
            .child(Button::new(tab.title.clone()).variant(variant).on_press(move |_: &mut S| send(Cmd::ShowView(open.clone()))).build(k).key(format!("tab-{}", tab.id)))
            .child(Button::new("×").variant(ButtonVariant::Ghost).help("Close this view").on_press(move |_: &mut S| send(Cmd::CloseView(close.clone()))).build(k).key(format!("close-{}", tab.id)));
    }
    if let Some(offer) = &snapshot.offer {
        let id = offer.id.clone();
        strip = strip
            .child(text(format!("The agent wants to show “{}”", offer.title)).font(CAPTION).color(k.c.text_muted))
            .child(Button::new("Show").variant(ButtonVariant::Submit).on_press(move |_: &mut S| send(Cmd::ShowView(id.clone()))).build(k).key("offer"));
    }
    strip.child(row().grow(1.0)).child(super::header::controls::<S>(k, &snapshot.controls.1))
}
