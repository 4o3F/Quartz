//! Typed sidebar content; protocol pages are consumed during mapping.
use super::{BuiltinCategory, CategoryColor, CategoryName, PaletteSlot};
use crate::link::{DetailData, DetailRequest, UtcRange};
use quartz_protocol::{self as wire, ClassificationRuleId};
use std::{collections::BTreeMap, time::Duration};

#[derive(Debug, snafu::Snafu)]
#[snafu(display("could not map detail response"))]
pub(crate) struct MapDetailsError;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CategoryDisplay {
    pub name: CategoryName,
    pub color: CategoryColor,
}
impl CategoryDisplay {
    pub(crate) fn from_protocol(category: &wire::CategoryRef) -> Result<Self, MapDetailsError> {
        let wire::CategoryRef::Classified { category } = category else {
            return Ok(Self {
                name: CategoryName::Builtin(BuiltinCategory::Uncategorized),
                color: CategoryColor::Neutral,
            });
        };
        let name = match (&category.name, category.builtin_key) {
            (Some(name), _) => CategoryName::Custom(name.clone()),
            (None, Some(key)) => CategoryName::Builtin(match key {
                wire::BuiltinCategory::Development => BuiltinCategory::Development,
                wire::BuiltinCategory::Learning => BuiltinCategory::Learning,
                wire::BuiltinCategory::Communication => BuiltinCategory::Communication,
                wire::BuiltinCategory::Entertainment => BuiltinCategory::Entertainment,
                wire::BuiltinCategory::System => BuiltinCategory::System,
            }),
            (None, None) => return Err(MapDetailsError),
        };
        let color = if category.color == "neutral" {
            CategoryColor::Neutral
        } else {
            CategoryColor::Palette(
                PaletteSlot::ALL
                    .into_iter()
                    .find(|slot| slot.token_key() == category.color)
                    .ok_or(MapDetailsError)?,
            )
        };
        Ok(Self { name, color })
    }
}
#[derive(Debug, Clone)]
pub(crate) enum RuleLabel {
    Text(String),
    Key(String),
    Identity(ClassificationRuleId),
}
#[derive(Debug, Clone)]
pub(crate) struct DetailRule {
    pub identity: ClassificationRuleId,
    pub label: RuleLabel,
    pub category: CategoryDisplay,
}
#[derive(Debug, Clone)]
pub(crate) enum DetailSource {
    Rule(RuleLabel),
    Manual,
    Unclassified,
    Mixed,
}
#[derive(Debug, Clone)]
pub(crate) struct DetailInterval {
    pub range: UtcRange,
    pub duration: Duration,
    pub app: Option<String>,
    pub title: Option<String>,
    pub category: CategoryDisplay,
    pub source: DetailSource,
}
#[derive(Debug, Clone)]
pub(crate) struct DetailDay {
    pub date: wire::LocalDate,
    pub duration: Duration,
}
#[derive(Debug, Clone)]
pub(crate) struct DetailTitle {
    pub title: String,
    pub duration: Duration,
}
#[derive(Debug, Clone)]
pub(crate) struct EntityDetails {
    pub daily: Vec<DetailDay>,
    pub titles: Vec<DetailTitle>,
    pub rules: Vec<DetailRule>,
}
#[derive(Debug, Clone)]
pub(crate) struct LiveDetails {
    pub intervals: Vec<DetailInterval>,
    pub entity: Option<EntityDetails>,
}
impl LiveDetails {
    fn duration(value: i64) -> Result<Duration, MapDetailsError> {
        Ok(Duration::from_millis(
            u64::try_from(value).map_err(|_| MapDetailsError)?,
        ))
    }
    pub(crate) fn from_protocol(data: DetailData) -> Result<Self, MapDetailsError> {
        if data
            .request
            .app()
            .is_some_and(|app| !data.entities.iter().any(|entity| entity.app == app))
        {
            return Err(MapDetailsError);
        }
        let mut labels = BTreeMap::new();
        for entity in &data.entities {
            for rule in &entity.rules {
                labels.insert(
                    rule.rule.clone(),
                    match (&rule.label, &rule.label_key) {
                        (Some(label), _) => RuleLabel::Text(label.clone()),
                        (None, Some(key)) => RuleLabel::Key(key.clone()),
                        (None, None) => RuleLabel::Identity(rule.rule.clone()),
                    },
                );
            }
        }
        let mut intervals = Vec::new();
        for interval in data.intervals {
            if let crate::link::DetailTarget::Segment {
                range,
                app,
                window_instance,
            } = data.request.target
                && (interval.start_utc_ms < range.start
                    || interval.end_utc_ms > range.end
                    || app.is_some_and(|expected| {
                        interval.app.as_ref().map(|app| app.id) != Some(expected)
                    })
                    || window_instance
                        .is_some_and(|expected| interval.window_instance != Some(expected)))
            {
                return Err(MapDetailsError);
            }
            if interval.end_utc_ms <= interval.start_utc_ms {
                return Err(MapDetailsError);
            }
            jiff::Timestamp::from_millisecond(interval.start_utc_ms)
                .map_err(|_| MapDetailsError)?;
            jiff::Timestamp::from_millisecond(interval.end_utc_ms).map_err(|_| MapDetailsError)?;
            let source = match interval.category_source {
                wire::CategorySource::Rule { rule } => DetailSource::Rule(
                    labels
                        .get(&rule)
                        .cloned()
                        .unwrap_or(RuleLabel::Identity(rule)),
                ),
                wire::CategorySource::Annotation { .. } => DetailSource::Manual,
                wire::CategorySource::Unclassified => DetailSource::Unclassified,
                wire::CategorySource::Mixed => DetailSource::Mixed,
            };
            intervals.push(DetailInterval {
                range: UtcRange {
                    start: interval.start_utc_ms,
                    end: interval.end_utc_ms,
                },
                duration: Self::duration(interval.duration_ms)?,
                app: interval.app.map(|app| app.display_name),
                title: interval.title,
                category: CategoryDisplay::from_protocol(&interval.category)?,
                source,
            });
        }
        let selected = data.request.app().or(match data.entities.as_slice() {
            [entity] => Some(entity.app),
            _ => None,
        });
        let mut result = Self {
            intervals,
            entity: None,
        };
        for entity in data.entities {
            if Some(entity.app) != selected {
                continue;
            }
            if entity.daily.len() != wire::ENTITY_DETAIL_DAYS {
                return Err(MapDetailsError);
            }
            let mut daily = Vec::new();
            for row in entity.daily {
                let expected = data
                    .request
                    .date
                    .checked_sub(
                        jiff::Span::new().days(
                            i64::try_from(wire::ENTITY_DETAIL_DAYS - daily.len() - 1)
                                .map_err(|_| MapDetailsError)?,
                        ),
                    )
                    .map_err(|_| MapDetailsError)?;
                if row.local_date != expected {
                    return Err(MapDetailsError);
                }
                daily.push(DetailDay {
                    date: row.local_date,
                    duration: Self::duration(row.active_ms)?,
                });
            }
            let titles = entity
                .titles
                .into_iter()
                .map(|row| {
                    Ok(DetailTitle {
                        title: row.title,
                        duration: Self::duration(row.duration_ms)?,
                    })
                })
                .collect::<Result<Vec<_>, MapDetailsError>>()?;
            let rules = entity
                .rules
                .into_iter()
                .map(|rule| {
                    Ok(DetailRule {
                        label: labels
                            .get(&rule.rule)
                            .cloned()
                            .unwrap_or(RuleLabel::Identity(rule.rule.clone())),
                        identity: rule.rule,
                        category: CategoryDisplay::from_protocol(&rule.category)?,
                    })
                })
                .collect::<Result<Vec<_>, MapDetailsError>>()?;
            result.entity = Some(EntityDetails {
                daily,
                titles,
                rules,
            });
        }
        Ok(result)
    }
}
impl super::Day {
    pub(crate) fn detail_request(&self, subject: super::DetailsSubject) -> Option<DetailRequest> {
        use crate::link::DetailTarget;
        let target = match subject {
            super::DetailsSubject::App(app) => DetailTarget::App(self.protocol_app(app)?),
            super::DetailsSubject::Segment(id) => {
                let segment = self.segment(id)?;
                DetailTarget::Segment {
                    range: self.utc_range(segment.span)?,
                    app: Some(self.protocol_app(segment.app)?),
                    window_instance: segment.window_instance,
                }
            }
            super::DetailsSubject::MainBlock(span) => DetailTarget::Segment {
                range: self.utc_range(span)?,
                app: None,
                window_instance: None,
            },
        };
        Some(DetailRequest {
            date: self.date(),
            target,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        format::Formatter,
        link::{DetailTarget, EntityData},
    };
    use quartz_i18n::Language;

    struct Fixture;
    impl Fixture {
        fn data() -> DetailData {
            let date: wire::LocalDate = "2026-11-21".parse().expect("date");
            let rule: ClassificationRuleId = "user.classification/editor@2".parse().expect("rule");
            let first = wire::QueryEntityDetailResponse {
                daily: Some(
                    (0..wire::ENTITY_DETAIL_DAYS)
                        .map(|index| wire::EntityDailyDuration {
                            local_date: date
                                .checked_sub(
                                    jiff::Span::new().days(
                                        i64::try_from(wire::ENTITY_DETAIL_DAYS - index - 1)
                                            .expect("seven days"),
                                    ),
                                )
                                .expect("daily date"),
                            active_ms: 1000,
                        })
                        .collect(),
                ),
                titles: Some(vec![wire::EntityTitleDuration {
                    title: "retained Full title".into(),
                    duration_ms: 600,
                }]),
                rules: vec![wire::EntityClassificationRule {
                    rule: rule.clone(),
                    label: Some("Chosen rule".into()),
                    label_key: Some("category.builtin.learning".into()),
                    category: wire::CategoryRef::Unclassified,
                }],
                versions: wire::QueryVersions("entity".into()),
                next_cursor: None,
            };
            let first: wire::QueryEntityDetailResponse =
                serde_json::from_value(serde_json::to_value(first).expect("entity JSON"))
                    .expect("entity fixture");
            let segments = wire::QuerySegmentDetailResponse {
                reference_range: None,
                intervals: vec![wire::TimelineSegment {
                    extraction: None,
                    document_ref: None,
                    start_utc_ms: 100,
                    end_utc_ms: 200,
                    duration_ms: 75,
                    clock_segment_id: Some(wire::ClockSegmentId(1)),
                    activity: wire::Activity::Active,
                    category: wire::CategoryRef::Unclassified,
                    category_source: wire::CategorySource::Rule { rule },
                    app: Some(wire::ApplicationRef {
                        id: wire::ApplicationId(3),
                        exe_name: None,
                        display_name: "Editor".into(),
                        aumid: None,
                    }),
                    window_instance: None,
                    title: Some("retained Full title".into()),
                }],
                versions: wire::QueryVersions("range".into()),
                next_cursor: None,
            };
            let segments: wire::QuerySegmentDetailResponse =
                serde_json::from_value(serde_json::to_value(segments).expect("segment JSON"))
                    .expect("segment fixture");
            DetailData {
                request: DetailRequest {
                    date,
                    target: DetailTarget::Segment {
                        range: UtcRange {
                            start: 100,
                            end: 200,
                        },
                        app: Some(wire::ApplicationId(3)),
                        window_instance: None,
                    },
                },
                intervals: segments.intervals,
                entities: vec![EntityData {
                    app: wire::ApplicationId(3),
                    daily: first.daily.expect("first-page daily"),
                    titles: first.titles.expect("first-page titles"),
                    rules: first.rules,
                }],
            }
        }
    }
    #[test]
    fn full_titles_and_rule_labels_remain_typed_and_language_dependent() {
        let mapped = LiveDetails::from_protocol(Fixture::data()).expect("mapped fixtures");
        let formatter = Formatter::new(Language::English);
        let row = mapped.intervals.first().expect("interval");
        assert_eq!(row.duration, Duration::from_millis(75));
        assert_eq!(row.title.as_deref(), Some("retained Full title"));
        assert_eq!(formatter.detail_source(&row.source), "Rule: Chosen rule");
        let entity = mapped.entity.expect("entity");
        assert_eq!(
            entity.titles.first().expect("title").duration,
            Duration::from_millis(600)
        );
        assert_eq!(
            entity.rules.first().expect("rule").identity.as_str(),
            "user.classification/editor@2"
        );
        let key = RuleLabel::Key("category.builtin.learning".into());
        assert_eq!(formatter.rule_label(&key), "Learning");
        assert_eq!(
            Formatter::new(Language::SimplifiedChinese).rule_label(&key),
            "学习"
        );
        assert!(
            Formatter::new(Language::PseudoEnglish)
                .rule_label(&key)
                .starts_with('[')
        );
    }
    #[test]
    fn unnamed_rules_keep_the_revision_identity_and_empty_titles_stay_absent() {
        let mut input = Fixture::data();
        let entity = input.entities.first_mut().expect("entity");
        entity.titles.clear();
        let rule = entity.rules.first_mut().expect("rule");
        rule.label = None;
        rule.label_key = None;
        input.intervals.first_mut().expect("interval").title = None;
        let mapped = LiveDetails::from_protocol(input).expect("unnamed rule");
        let formatter = Formatter::new(Language::English);
        let row = mapped.intervals.first().expect("interval");
        assert!(row.title.is_none());
        assert_eq!(
            formatter.detail_source(&row.source),
            "Rule: user.classification/editor@2"
        );
        assert!(mapped.entity.expect("entity").titles.is_empty());
    }
    #[test]
    fn malformed_daily_history_and_negative_elapsed_durations_are_rejected() {
        let mut input = Fixture::data();
        input.entities.first_mut().expect("entity").daily.pop();
        assert!(LiveDetails::from_protocol(input).is_err());
        let mut input = Fixture::data();
        input
            .entities
            .first_mut()
            .expect("entity")
            .daily
            .first_mut()
            .expect("day")
            .active_ms = -1;
        assert!(LiveDetails::from_protocol(input).is_err());
        let mut input = Fixture::data();
        input.intervals.first_mut().expect("interval").duration_ms = -1;
        assert!(LiveDetails::from_protocol(input).is_err());
    }
}
