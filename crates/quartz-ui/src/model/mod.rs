//! View models the UI reads. A protocol mapping fills them later; nothing here depends on GPUI,
//! the database, or rule evaluation.

mod application;
mod category;
mod day;
mod details;
mod palette;
mod ranking;
mod rule;
mod segment;
mod state;
mod summary;
mod time;
mod track;

pub(crate) use application::{AppId, AppInfo, Glyph};
pub(crate) use category::{BuiltinCategory, Category, CategoryId, CategoryName, Tendency};
pub(crate) use day::{Day, DayParts, EARLIER_DAYS, EarlierDays};
pub(crate) use details::{BarDay, Details, DetailsSubject, IntervalCard};
pub(crate) use palette::{CategoryColor, PaletteSlot, PaletteSlotMap};
pub(crate) use ranking::{RankKey, RankRow, RankingKind};
pub(crate) use rule::{Explanation, Rule, RuleCondition, RuleField, RuleId, RuleOperator};
pub(crate) use segment::{ClassificationSource, Segment, SegmentId};
pub(crate) use state::{AwayLabel, ClockAnomaly, StateBlock, StateKind};
pub(crate) use summary::{DaySummary, HourlyBar, Share};
pub(crate) use time::{MINUTES_PER_HOUR, SECONDS_PER_MINUTE, TimeOfDay, TimeSpan};
pub(crate) use track::{Lane, MainBlock};

#[cfg(test)]
pub(crate) mod fixture;
