//! Opaque, outlined data marks with a fixed pattern vocabulary for high contrast (11 section 2).

use crate::model::CategoryColor;
use gpui_kit::{
    Bounds, Canvas, ContentMask, Hsla, PathBuilder, Pixels, Point, Window, canvas, fill, outline,
    point, px,
};

/// Seven patterns repeat deterministically across the 14 palette slots (11 sections 2 and 3.3).
const PATTERN_COUNT: u8 = 7;
/// Dense repeat distance derived from the 4 px design grid (11 sections 2 and 5.2).
const PATTERN_SPACING: f32 = 4.;
/// Pattern line and inside outline width (11 section 2).
const MARK_STROKE: f32 = 1.;
/// Dot size leaves a visible gap on the 4 px grid (11 section 2).
const DOT_SIZE: f32 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataPattern {
    Solid,
    Diagonal,
    ReverseDiagonal,
    CrossHatch,
    Horizontal,
    Vertical,
    Dots,
}

impl DataPattern {
    pub(crate) fn for_category(category: CategoryColor) -> Self {
        let index = match category {
            CategoryColor::Palette(slot) => (slot.ordinal() - 1) % PATTERN_COUNT,
            CategoryColor::Neutral => return Self::Dots,
        };
        match index {
            0 => Self::Solid,
            1 => Self::Diagonal,
            2 => Self::ReverseDiagonal,
            3 => Self::CrossHatch,
            4 => Self::Horizontal,
            5 => Self::Vertical,
            _ => Self::Dots,
        }
    }

    pub(crate) fn canvas(self, surface: Hsla, highlight: Hsla, foreground: Hsla) -> Canvas<()> {
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                // Every mark has an opaque surface, including the spaces between pattern strokes.
                window.paint_quad(fill(
                    bounds,
                    if self == Self::Solid {
                        highlight
                    } else {
                        surface
                    },
                ));
                let interior = bounds.inset(px(MARK_STROKE));
                if interior.size.width > px(0.) && interior.size.height > px(0.) {
                    window.with_content_mask(Some(ContentMask { bounds: interior }), |window| {
                        self.paint(interior, highlight, window)
                    });
                }
                // GPUI paints this border inside `bounds`, preserving the interval geometry and shared edges.
                window.paint_quad(outline(bounds, foreground, Default::default()));
            },
        )
    }

    fn paint(self, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        match self {
            Self::Solid => {}
            Self::Dots => {
                let mut y = PATTERN_SPACING / 2.;
                while y < height {
                    let mut x = PATTERN_SPACING / 2.;
                    while x < width {
                        window.paint_quad(fill(
                            Bounds {
                                origin: bounds.origin + point(px(x), px(y)),
                                size: gpui_kit::size(px(DOT_SIZE), px(DOT_SIZE)),
                            },
                            color,
                        ));
                        x += PATTERN_SPACING;
                    }
                    y += PATTERN_SPACING;
                }
            }
            Self::Horizontal => {
                let mut y = PATTERN_SPACING / 2.;
                while y < height {
                    Self::line(
                        window,
                        bounds.origin + point(px(0.), px(y)),
                        bounds.origin + point(px(width), px(y)),
                        color,
                    );
                    y += PATTERN_SPACING;
                }
            }
            Self::Vertical => {
                let mut x = PATTERN_SPACING / 2.;
                while x < width {
                    Self::line(
                        window,
                        bounds.origin + point(px(x), px(0.)),
                        bounds.origin + point(px(x), px(height)),
                        color,
                    );
                    x += PATTERN_SPACING;
                }
            }
            Self::Diagonal | Self::ReverseDiagonal | Self::CrossHatch => {
                let mut x = -height;
                while x < width {
                    if self != Self::ReverseDiagonal {
                        Self::line(
                            window,
                            bounds.origin + point(px(x), px(height)),
                            bounds.origin + point(px(x + height), px(0.)),
                            color,
                        );
                    }
                    if self != Self::Diagonal {
                        Self::line(
                            window,
                            bounds.origin + point(px(x), px(0.)),
                            bounds.origin + point(px(x + height), px(height)),
                            color,
                        );
                    }
                    x += PATTERN_SPACING;
                }
            }
        }
    }

    fn line(window: &mut Window, start: Point<Pixels>, end: Point<Pixels>, color: Hsla) {
        let mut path = PathBuilder::stroke(px(MARK_STROKE));
        path.move_to(start);
        path.line_to(end);
        match path.build() {
            Ok(path) => window.paint_path(path, color),
            Err(error) => tracing::warn!(%error,"could not draw high contrast pattern"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PaletteSlot;
    #[test]
    fn categories_use_a_fixed_repeating_vocabulary_and_neutral_uses_dots() {
        assert_eq!(
            DataPattern::for_category(CategoryColor::Neutral),
            DataPattern::Dots
        );
        let first = DataPattern::for_category(CategoryColor::Palette(PaletteSlot::numbered(1)));
        assert_eq!(first, DataPattern::Solid);
        assert_eq!(
            first,
            DataPattern::for_category(CategoryColor::Palette(PaletteSlot::numbered(8)))
        );
        for slot in PaletteSlot::ALL {
            let _pattern = DataPattern::for_category(CategoryColor::Palette(slot));
        }
    }
}
