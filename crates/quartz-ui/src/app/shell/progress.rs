//! Shared determinate progress indicator for multi-day queries exceeding 250 ms.

use gpui_kit::{
    Div, ParentElement as _, Styled as _, div, prelude::FluentBuilder as _, px, relative,
};

use crate::{
    app::{
        metrics::Ramp,
        tokens::ContrastMode,
        widgets::{RampExt as _, Widgets},
    },
    format::Formatter,
    model::LoadProgress,
};

/// Height of the determinate progress bar track in standard contrast mode (logical pixels).
const PROGRESS_BAR_HEIGHT: f32 = 6.;
/// Height of the determinate progress bar track in high contrast mode (logical pixels).
const HIGH_CONTRAST_BAR_HEIGHT: f32 = 8.;

/// Renders a determinate multi-day query progress bar and localized label once the first core
/// progress update has arrived. Returns `None` while [`LoadProgress::Waiting`] so fast queries
/// keep their existing loading placeholder without flashing a progress bar.
#[must_use]
pub(super) fn query_progress_view(
    ui: &Widgets<'_>,
    formatter: Formatter,
    progress: LoadProgress,
) -> Option<Div> {
    let current = progress.current()?;
    let space = &ui.metrics.space;
    let palette = ui.palette;
    let high_contrast = palette.contrast == ContrastMode::High;

    let label = formatter.query_progress(current);
    let fraction = current.fraction();

    let container_bg = if high_contrast {
        palette.bg_base
    } else {
        palette.fill_card_secondary
    };
    let track_bg = if high_contrast {
        palette.bg_base
    } else {
        palette.fill_subtle_hover
    };
    let track_height = if high_contrast {
        px(HIGH_CONTRAST_BAR_HEIGHT)
    } else {
        px(PROGRESS_BAR_HEIGHT)
    };
    let text_color = if high_contrast {
        palette.text_primary
    } else {
        palette.text_secondary
    };

    let fill = div()
        .h_full()
        .w(relative(fraction))
        .rounded_full()
        .bg(palette.accent_fill);

    let track = div()
        .w_full()
        .h(track_height)
        .rounded_full()
        .overflow_hidden()
        .bg(track_bg)
        .when(high_contrast, |bar| {
            bar.border(ui.metrics.layout.divider)
                .border_color(palette.stroke_control)
        })
        .child(fill);

    Some(
        div()
            .flex()
            .flex_col()
            .min_w(px(0.))
            .w_full()
            .gap(space.s)
            .p(space.m)
            .rounded(ui.metrics.radius.control)
            .bg(container_bg)
            .border_1()
            .border_color(palette.stroke_card)
            .child(
                div()
                    .min_w(px(0.))
                    .whitespace_normal()
                    .ramp(ui, Ramp::Body)
                    .tabular_figures()
                    .text_color(text_color)
                    .child(label),
            )
            .child(track),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{
            runtime::ThemeBundle,
            tokens::{Palette, SystemColors},
        },
        text_scale::TextScale,
    };
    use gpui_kit::component::ThemeMode;
    use quartz_i18n::Language;

    #[test]
    fn progress_widget_hides_while_waiting_and_respects_high_contrast_tokens() {
        let theme = ThemeBundle::embedded().expect("embedded theme");
        let tokens = theme.tokens();
        let formatter = Formatter::new(Language::English);

        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let ui = Widgets::new(&tokens, mode, TextScale::UNSCALED);
            assert!(
                query_progress_view(&ui, formatter, LoadProgress::Waiting).is_none(),
                "waiting state renders nothing until first progress arrives"
            );
            assert!(
                query_progress_view(
                    &ui,
                    formatter,
                    LoadProgress::Counting {
                        done: 120,
                        total: 365,
                    },
                )
                .is_some()
            );
        }

        let colors: SystemColors =
            serde_json::from_str(include_str!("../../../themes/preview-high-contrast.json"))
                .expect("high contrast colors");
        let palette = Palette::high_contrast(&colors);
        let ui = Widgets {
            palette: &palette,
            metrics: tokens.metrics(),
            text_scale: TextScale::UNSCALED,
        };
        let mut rendered = query_progress_view(
            &ui,
            Formatter::new(Language::SimplifiedChinese),
            LoadProgress::Counting {
                done: 400,
                total: 365,
            },
        )
        .expect("counting state renders widget");
        assert_eq!(rendered.style().background, Some(colors.window.into()));
        assert_eq!(rendered.style().border_color, Some(colors.window_text));
    }
}
