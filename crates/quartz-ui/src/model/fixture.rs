//! Small days for model tests: two applications, three categories and one rule, with segments
//! and states chosen per test.

use std::{collections::BTreeMap, num::NonZeroU32};

use super::{
    AppId, AppInfo, BuiltinCategory, Category, CategoryId, ClassificationSource, Day, DayParts,
    EarlierDays, Glyph, Rule, RuleCondition, RuleField, RuleId, RuleOperator, Segment, SegmentId,
    StateBlock, StateKind, TimeOfDay, TimeSpan,
};
use crate::model::ClockAnomaly;

/// A focus category.
pub(crate) const WORK: CategoryId = CategoryId::new(1);
/// A distraction category.
pub(crate) const PLAY: CategoryId = CategoryId::new(2);
/// The unclassified category.
pub(crate) const NONE: CategoryId = CategoryId::new(3);

pub(crate) const EDITOR: AppId = AppId::new(1);
pub(crate) const BROWSER: AppId = AppId::new(2);

/// Classifies the editor by its executable.
pub(crate) const EDITOR_RULE: RuleId = RuleId::new(1);

/// The parts of a Monday with no activity.
pub(crate) fn parts() -> DayParts {
    let app = |id, name: &str, glyph| AppInfo {
        id,
        name: name.to_owned(),
        short_name: name.to_owned(),
        exe: format!("{name}.exe"),
        glyph,
    };
    DayParts {
        date: jiff::civil::date(2026, 11, 16),
        categories: vec![
            Category::builtin(WORK, BuiltinCategory::Development),
            Category::builtin(PLAY, BuiltinCategory::Entertainment),
            Category::builtin(NONE, BuiltinCategory::Uncategorized),
        ],
        apps: vec![
            app(EDITOR, "editor", Glyph::Editor),
            app(BROWSER, "browser", Glyph::Browser),
        ],
        segments: Vec::new(),
        states: Vec::new(),
        history: BTreeMap::from([
            (EDITOR, EarlierDays::default()),
            (BROWSER, EarlierDays::default()),
        ]),
        rules: vec![Rule {
            id: EDITOR_RULE,
            rule_set: "test".to_owned(),
            name: "editor is work".to_owned(),
            priority: 1,
            condition: RuleCondition {
                field: RuleField::AppExeName,
                operator: RuleOperator::Equals,
                value: "editor.exe".to_owned(),
            },
        }],
    }
}

/// A span between two `(hour, minute)` pairs.
pub(crate) fn span(from: (u32, u32), to: (u32, u32)) -> TimeSpan {
    TimeSpan::new(TimeOfDay::hm(from.0, from.1), TimeOfDay::hm(to.0, to.1))
}

/// An unmatched segment of `app` in `category`.
pub(crate) fn segment(
    id: u32,
    app: AppId,
    category: CategoryId,
    from: (u32, u32),
    to: (u32, u32),
) -> Segment {
    Segment {
        id: SegmentId::new(id),
        span: span(from, to),
        app,
        category,
        title: format!("title {id}"),
        domain: None,
        document: None,
        source: ClassificationSource::Unmatched,
        merged: NonZeroU32::MIN,
    }
}

/// A state block without a clock anomaly.
pub(crate) fn state(kind: StateKind, from: (u32, u32), to: (u32, u32)) -> StateBlock {
    StateBlock {
        span: span(from, to),
        kind,
        clock: ClockAnomaly::None,
    }
}

/// A valid day with these segments and states.
pub(crate) fn day(segments: Vec<Segment>, states: Vec<StateBlock>) -> Day {
    Day::new(DayParts {
        segments,
        states,
        ..parts()
    })
    .expect("fixture day is valid")
}
