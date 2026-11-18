//! Text widths measured with GPUI's text system, so columns are as wide as their content and not
//! as wide as a guess (11 section 5.6).

use std::fmt;

use gpui_kit::{Font, FontStyle, SharedString, TextRun, Window, WindowTextSystem, black};

use super::{metrics::Ramp, widgets::Widgets};
use crate::timeline_scale::{MeasuredLabel, PixelPlacement};

/// Measures text in the fonts the views render it in.
pub(crate) struct TextMeasure<'a> {
    system: &'a WindowTextSystem,
    ui: Widgets<'a>,
    /// The theme font family, used by every step without a family of its own.
    family: SharedString,
}

impl fmt::Debug for TextMeasure<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextMeasure")
            .field("family", &self.family)
            .finish_non_exhaustive()
    }
}

impl<'a> TextMeasure<'a> {
    /// A measure of `ui` text in the theme font `family`.
    pub(crate) fn new(window: &'a Window, ui: Widgets<'a>, family: SharedString) -> Self {
        Self {
            system: window.text_system(),
            ui,
            family,
        }
    }

    /// The width in logical pixels of one line of `text` in `ramp` type with equal-width digits,
    /// as the views render numbers.
    pub(crate) fn width(&self, text: &str, ramp: Ramp) -> f32 {
        if text.is_empty() {
            return 0.;
        }
        let step = self.ui.step(ramp);
        let font = Font {
            family: step.font_family.unwrap_or_else(|| self.family.clone()),
            features: Widgets::tabular_figures(),
            fallbacks: None,
            weight: step.weight,
            style: FontStyle::Normal,
        };
        let run = TextRun {
            len: text.len(),
            font,
            color: black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = self.system.layout_line(text, step.font_size, &[run], None);
        f32::from(line.width)
    }

    /// The longest of the labels `full` and `short` in caption type that fits in `block` with
    /// `padding` on each side; `None` when neither does.
    pub(crate) fn fit_block_label(
        &self,
        block: PixelPlacement,
        padding: f32,
        short: &str,
        full: &str,
    ) -> Option<String> {
        let measured = |text| MeasuredLabel {
            text,
            width: self.width(text, Ramp::Caption),
        };
        block
            .fitting_label(padding, measured(full), measured(short))
            .map(str::to_owned)
    }

    /// The widths of every text of `texts` in `ramp` type.
    pub(crate) fn widths<'t>(
        &'t self,
        texts: impl IntoIterator<Item = &'t str> + 't,
        ramp: Ramp,
    ) -> impl Iterator<Item = f32> + 't {
        texts.into_iter().map(move |text| self.width(text, ramp))
    }
}
