//! The Windows "Text size" accessibility setting as a validated factor (07 section 2.4, 11
//! section 5.7).

use std::{
    fmt::{self, Display},
    num::ParseFloatError,
    str::FromStr,
};

use num_traits::ToPrimitive as _;
use snafu::{Report, ResultExt as _, Snafu};

/// The smallest text scale: 100%, the Windows default.
const MIN_TEXT_SCALE: f64 = 1.;
/// The largest text scale: 225%, the top of the Windows "Text size" slider range.
const MAX_TEXT_SCALE: f64 = 2.25;

/// A text scale outside the 1.0 to 2.25 range of the Windows "Text size" setting, not a number, or
/// not representable for GPUI.
#[derive(Debug, Clone, Copy, PartialEq, Snafu)]
#[snafu(display("text scale {value} is outside {MIN_TEXT_SCALE} to {MAX_TEXT_SCALE}"))]
pub struct ValidateTextScaleError {
    value: f64,
}

/// A text scale text is not a number in the valid range.
#[derive(Debug, Clone, PartialEq, Snafu)]
#[non_exhaustive]
pub enum ParseTextScaleError {
    /// The text is not a decimal number.
    #[snafu(display("text scale {text:?} is not a number"))]
    NotANumber {
        text: String,
        source: ParseFloatError,
    },
    /// The number is outside the Windows "Text size" range.
    #[snafu(display("invalid text scale"))]
    OutOfRange { source: ValidateTextScaleError },
}

/// Why the system text scale could not be read.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum ReadTextScaleError {
    /// Windows did not provide `UISettings.TextScaleFactor`.
    #[cfg(windows)]
    #[snafu(display("could not read the Windows text size setting"))]
    ReadSetting { source: windows::core::Error },
    /// Windows reported a factor outside the range of its own slider.
    #[snafu(display("the Windows text size setting is not a valid scale"))]
    InvalidSetting { source: ValidateTextScaleError },
}

/// The factor Windows applies to text: 1.0 to 2.25, where 1.0 is unscaled.
///
/// Scales the type ramp only; spacing, radii, strokes and icons keep their logical size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextScale {
    /// The validated value, as Windows reports it.
    exact: f64,
    /// The same value as GPUI lengths use it.
    gpui: f32,
}

impl TextScale {
    /// No scaling: 100%, and the value every non-Windows build uses.
    pub const UNSCALED: Self = Self {
        exact: MIN_TEXT_SCALE,
        gpui: 1.,
    };

    /// A scale of `factor`.
    ///
    /// # Errors
    /// Returns an error when `factor` is not a number from 1.0 to 2.25.
    pub fn try_new(factor: f64) -> Result<Self, ValidateTextScaleError> {
        if !(MIN_TEXT_SCALE..=MAX_TEXT_SCALE).contains(&factor) {
            return Err(ValidateTextScaleError { value: factor });
        }
        // The range check above is the guarantee: the value is finite and within 1.0 to 2.25, so
        // the conversion is exact to within `f32` rounding (nearest, ties to even).
        let gpui = factor
            .to_f32()
            .ok_or(ValidateTextScaleError { value: factor })?;
        Ok(Self {
            exact: factor,
            gpui,
        })
    }

    /// The factor as Windows reports it.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.exact
    }

    /// The factor as GPUI lengths, which are `f32`.
    #[must_use]
    pub const fn for_gpui(self) -> f32 {
        self.gpui
    }

    /// Reads the current Windows "Text size" setting; non-Windows builds are unscaled.
    ///
    /// # Errors
    /// Returns an error when Windows cannot provide the setting or reports an invalid one.
    #[cfg(windows)]
    pub(crate) fn read_system() -> Result<Self, ReadTextScaleError> {
        use windows::UI::ViewManagement::UISettings;

        let settings = UISettings::new().context(ReadSettingSnafu)?;
        let factor = settings.TextScaleFactor().context(ReadSettingSnafu)?;
        Self::try_new(factor).context(InvalidSettingSnafu)
    }

    /// Non-Windows builds have no such setting and are unscaled.
    ///
    /// # Errors
    /// Never fails; the signature matches the Windows one.
    #[cfg(not(windows))]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "same signature as the Windows reader"
    )]
    pub(crate) fn read_system() -> Result<Self, ReadTextScaleError> {
        Ok(Self::UNSCALED)
    }
}

/// Where the text scale comes from.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) enum TextScaleSource {
    /// The Windows "Text size" setting, read again whenever the window is activated.
    #[default]
    System,
    /// A fixed scale, such as the preview override.
    Fixed(TextScale),
}

impl TextScaleSource {
    /// The scale right now.
    ///
    /// # Errors
    /// Returns an error when the system setting cannot be read.
    pub(crate) fn read(self) -> Result<TextScale, ReadTextScaleError> {
        match self {
            Self::System => TextScale::read_system(),
            Self::Fixed(scale) => Ok(scale),
        }
    }
}

/// Whether a refresh changed the text scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextScaleChange {
    Changed,
    Unchanged,
}

/// The text scale a window uses, re-read from its source when the window is activated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LiveTextScale {
    source: TextScaleSource,
    current: TextScale,
}

impl LiveTextScale {
    /// Reads `source` for the first time.
    ///
    /// # Errors
    /// Returns an error when the system setting cannot be read.
    pub(crate) fn read(source: TextScaleSource) -> Result<Self, ReadTextScaleError> {
        Ok(Self {
            source,
            current: source.read()?,
        })
    }

    /// The scale now in use.
    #[must_use]
    pub(crate) const fn current(&self) -> TextScale {
        self.current
    }

    /// Reads the source again. A failure keeps the previous scale and is logged.
    pub(crate) fn refresh(&mut self) -> TextScaleChange {
        match self.source.read() {
            Ok(scale) if scale != self.current => {
                self.current = scale;
                TextScaleChange::Changed
            }
            Ok(_) => TextScaleChange::Unchanged,
            Err(error) => {
                tracing::warn!(
                    "keeping text scale {}: {}",
                    self.current,
                    Report::from_error(&error)
                );
                TextScaleChange::Unchanged
            }
        }
    }
}

impl Default for TextScale {
    fn default() -> Self {
        Self::UNSCALED
    }
}

impl Display for TextScale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.exact, f)
    }
}

impl FromStr for TextScale {
    type Err = ParseTextScaleError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let factor: f64 = text.parse().context(NotANumberSnafu { text })?;
        Self::try_new(factor).context(OutOfRangeSnafu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_range_edges_are_valid_and_values_beyond_them_are_not() {
        assert_eq!(TextScale::try_new(1.).map(TextScale::get), Ok(1.));
        assert_eq!(TextScale::try_new(2.25).map(TextScale::get), Ok(2.25));
        assert_eq!(TextScale::try_new(1.5).map(TextScale::for_gpui), Ok(1.5));
        for invalid in [0.99, 2.250_000_1, 0., -1., f64::NAN, f64::INFINITY, 1e40] {
            assert!(
                TextScale::try_new(invalid).is_err(),
                "{invalid} must be rejected, not clamped"
            );
        }
    }

    #[test]
    fn decimals_just_outside_the_range_are_not_rounded_onto_an_edge() {
        // Parsed as f32 these would round to exactly 2.25 and 1.0.
        assert!("2.2500000001".parse::<TextScale>().is_err());
        assert!("0.9999999999".parse::<TextScale>().is_err());
    }

    #[test]
    fn the_gpui_factor_is_the_nearest_f32() {
        let scale = TextScale::try_new(1.333_333_333_3).expect("valid");
        assert!((f64::from(scale.for_gpui()) - scale.get()).abs() < 1e-6);
        // The midpoint of two adjacent f32 values rounds to the even one, as a direct
        // conversion does; the shortest decimal text of it does not always.
        let below = 1.5_f32;
        let above = f32::from_bits(below.to_bits() + 1);
        let midpoint = (f64::from(below) + f64::from(above)) / 2.;
        let scale = TextScale::try_new(midpoint).expect("valid");
        assert_eq!(scale.for_gpui(), below, "ties to even");
        assert_eq!(TextScale::UNSCALED.for_gpui(), 1.);
    }

    #[test]
    fn text_parses_strictly() {
        assert_eq!("1.5".parse::<TextScale>().map(TextScale::get), Ok(1.5));
        assert!(matches!(
            "big".parse::<TextScale>(),
            Err(ParseTextScaleError::NotANumber { .. })
        ));
        assert!(matches!(
            "3".parse::<TextScale>(),
            Err(ParseTextScaleError::OutOfRange { .. })
        ));
        assert_eq!(TextScale::default(), TextScale::UNSCALED);
    }
}
