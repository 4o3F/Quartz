//! Pending-away row with inline label buttons (07 section 3).

use gpui_kit::{IntoElement, ParentElement as _, Styled as _, assets::IconName, div, px};

use super::AppShell;
use crate::app::{
    tokens::Palette,
    widgets::{Density, Ramp, RampExt as _, icon, space},
};

const ACTIONS: [&str; 5] = [
    "today.pending.rest",
    "today.pending.meeting",
    "today.pending.reading",
    "today.pending.dismiss",
    "today.pending.other",
];

impl AppShell {
    pub(super) fn render_pending(&self, p: &Palette) -> Option<impl IntoElement> {
        let away = self.day.pending_away()?;
        let mut buttons = div().flex().flex_row().gap(px(space::S));
        for (index, key) in ACTIONS.iter().enumerate() {
            buttons = buttons.child(p.button(
                ("pending", index),
                Density::Compact,
                Some(self.fmt.text(key)),
                None,
            ));
        }
        Some(
            p.card()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(space::S))
                .px(px(space::L))
                .py(px(space::S))
                .child(icon(IconName::Clock, 16., p.text_primary))
                .child(
                    div()
                        .ramp(Ramp::BodyStrong)
                        .child(self.fmt.text("today.pending.title")),
                )
                .child(
                    div()
                        .flex_1()
                        .ramp(Ramp::Caption)
                        .text_color(p.text_secondary)
                        .child(self.fmt.text_with(
                            "today.pending.range",
                            &[
                                ("range", &away.span.to_string()),
                                ("duration", &self.fmt.duration(away.span.duration())),
                            ],
                        )),
                )
                .child(buttons),
        )
    }
}
