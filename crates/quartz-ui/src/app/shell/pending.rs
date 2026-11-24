//! Pending-away row with inline label buttons (07 section 3).

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, assets::IconName, div,
};

use super::AppShell;
use crate::{
    app::{
        metrics::{Density, Ramp},
        widgets::{ButtonContent, RampExt as _, Widgets},
    },
    format::{Arg, Msg},
};

/// How the user can label an away span (07 section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PendingAction {
    Rest,
    Meeting,
    Reading,
    Dismiss,
    Other,
}

impl PendingAction {
    const ALL: [Self; 5] = [
        Self::Rest,
        Self::Meeting,
        Self::Reading,
        Self::Dismiss,
        Self::Other,
    ];

    const fn label(self) -> Msg {
        match self {
            Self::Rest => Msg::PendingRest,
            Self::Meeting => Msg::PendingMeeting,
            Self::Reading => Msg::PendingReading,
            Self::Dismiss => Msg::PendingDismiss,
            Self::Other => Msg::PendingOther,
        }
    }

    const fn element_id(self) -> &'static str {
        match self {
            Self::Rest => "pending-rest",
            Self::Meeting => "pending-meeting",
            Self::Reading => "pending-reading",
            Self::Dismiss => "pending-dismiss",
            Self::Other => "pending-other",
        }
    }
}

impl AppShell {
    pub(super) fn render_pending(
        &self,
        ui: &Widgets<'_>,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement> {
        if matches!(self.day.source(), super::DaySource::Live(_)) {
            let pending = self.day.pending_ranges();
            if pending.is_empty() {
                return None;
            }
            let rows = pending.iter().enumerate().map(|(index, range)| {
                let buttons = div().flex().flex_wrap().gap(ui.metrics.space.s).children(
                    PendingAction::ALL.map(|action| {
                        let range = *range;
                        let label = ButtonContent::Label(self.formatter.text(action.label()));
                        if action == PendingAction::Other
                            || self.day.utc_range(range.span).is_some_and(|range| {
                                self.live
                                    .as_ref()
                                    .is_some_and(|live| live.actions.contains(range))
                            })
                        {
                            return ui.disabled_button(
                                action.element_id(),
                                Density::Compact,
                                label,
                            ); // TODO(M4): category picker.
                        }
                        ui.button(action.element_id(), Density::Compact, label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let action = match action {
                                    PendingAction::Rest => crate::link::AwayAction::Rest,
                                    PendingAction::Meeting => crate::link::AwayAction::Meeting,
                                    PendingAction::Reading => crate::link::AwayAction::Reading,
                                    PendingAction::Dismiss => crate::link::AwayAction::Ignore,
                                    PendingAction::Other => return,
                                };
                                this.annotate(action, range, cx);
                            }))
                    }),
                );
                let span = range.span;
                let range_text = format!(
                    "{} – {}",
                    self.clock_label(span.start()),
                    self.clock_label(span.end())
                );
                div()
                    .id(("pending-away", index))
                    .flex()
                    .flex_col()
                    .gap(ui.metrics.space.s)
                    .child(self.formatter.text_with(
                        Msg::PendingRange,
                        &[
                            Arg::Range.with(&range_text),
                            Arg::Duration.with(&self.formatter.duration(range.duration)),
                        ],
                    ))
                    .child(buttons)
            });
            return Some(
                ui.card()
                    .p(ui.metrics.space.l)
                    .flex()
                    .flex_col()
                    .gap(ui.metrics.space.m)
                    .child(self.formatter.text(Msg::PendingTitle))
                    .children(rows),
            );
        }
        let away = self.day.pending_away()?;
        let space = &ui.metrics.space;
        let buttons = div()
            .flex()
            .flex_row()
            .gap(space.s)
            .children(PendingAction::ALL.map(|action| {
                ui.button(
                    action.element_id(),
                    Density::Compact,
                    ButtonContent::Label(self.formatter.text(action.label())),
                )
            }));
        Some(
            ui.card()
                .flex()
                .flex_row()
                .items_center()
                .gap(space.s)
                .px(space.l)
                .py(space.s)
                .child(ui.icon(IconName::Clock, Density::Compact, ui.palette.text_primary))
                .child(
                    div()
                        .ramp(ui, Ramp::BodyStrong)
                        .child(self.formatter.text(Msg::PendingTitle)),
                )
                .child(
                    div()
                        .flex_1()
                        .ramp(ui, Ramp::Caption)
                        .text_color(ui.palette.text_secondary)
                        .child(self.formatter.text_with(
                            Msg::PendingRange,
                            &[
                                Arg::Range.with(&away.span.to_string()),
                                Arg::Duration.with(&self.formatter.duration(away.span.duration())),
                            ],
                        )),
                )
                .child(buttons),
        )
    }
}
