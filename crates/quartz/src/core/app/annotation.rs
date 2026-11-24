//! Quick annotation of the most recently completed idle availability range.

use quartz_domain::capture::{AvailabilityEndReason, RowId, Timestamp};
use quartz_protocol::{
    AnnotationCategory, AnnotationId, AnnotationKind, AnnotationLabelPreset, AnnotationValues,
    AnnotationsCreateRequest, BuiltinCategory, CreateAnnotation,
};

/// 07 §1.3 only prompts for idle periods of at least fifteen minutes.
pub(crate) const MIN_ANNOTATION_IDLE_MS: i64 = 15 * crate::tray_summary::MINUTE_MS;
/// 07 §1.3 keeps the completed idle prompt for ten minutes.
const ANNOTATION_WINDOW_MS: i64 = 10 * crate::tray_summary::MINUTE_MS;

#[derive(Debug, Clone, Copy)]
pub(crate) enum IdleAvailability {
    Opened {
        id: RowId,
        start: Timestamp,
    },
    Closed {
        id: RowId,
        at: Timestamp,
        reason: AvailabilityEndReason,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnnotationChoice {
    Rest,
    Meeting,
    Reading,
    Ignore,
}
impl AnnotationChoice {
    pub(crate) fn request(
        self,
        period: IdlePeriod,
        uuid: AnnotationId,
    ) -> AnnotationsCreateRequest {
        let mut entity = CreateAnnotation {
            uuid,
            kind: AnnotationKind::Note,
            values: AnnotationValues {
                start_utc_ms: period.start_utc_ms,
                end_utc_ms: period.end_utc_ms,
                category: None,
                label_preset: None,
                label: None,
                note: None,
            },
        };
        match self {
            Self::Rest => entity.values.label_preset = Some(AnnotationLabelPreset::Rest),
            Self::Meeting | Self::Reading => {
                entity.kind = AnnotationKind::OfflineActivity;
                entity.values.category = Some(AnnotationCategory::Builtin {
                    key: if self == Self::Meeting {
                        BuiltinCategory::Communication
                    } else {
                        BuiltinCategory::Learning
                    },
                });
            }
            Self::Ignore => entity.kind = AnnotationKind::DismissedAway,
        }
        AnnotationsCreateRequest {
            entities: vec![entity],
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdlePeriod {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
}
impl IdlePeriod {
    pub(crate) fn minutes(self) -> i64 {
        (self.end_utc_ms - self.start_utc_ms) / crate::tray_summary::MINUTE_MS
    }
}
#[derive(Debug)]
struct OpenIdle {
    id: Option<RowId>,
    start: Timestamp,
}
#[derive(Debug, Default)]
pub(crate) struct QuickAnnotation {
    open: Option<OpenIdle>,
    pending: Option<IdlePeriod>,
}
impl QuickAnnotation {
    pub(crate) fn observe(&mut self, change: IdleAvailability) {
        match change {
            IdleAvailability::Opened { id, start } => {
                let start = self
                    .open
                    .as_ref()
                    .filter(|open| open.id.is_none())
                    .map_or(start, |open| open.start);
                self.open = Some(OpenIdle {
                    id: Some(id),
                    start,
                });
                self.pending = None;
            }
            IdleAvailability::Closed { id, at, reason } => {
                if self.open.as_ref().is_none_or(|open| open.id != Some(id)) {
                    return;
                }
                let mut open = self.open.take().expect("matching idle row");
                if reason == AvailabilityEndReason::MidnightSplit {
                    open.id = None;
                    self.open = Some(open);
                    return;
                }
                if matches!(
                    reason,
                    AvailabilityEndReason::Resumed | AvailabilityEndReason::Exception(_)
                ) {
                    let period = IdlePeriod {
                        start_utc_ms: open.start.utc.get().as_millisecond(),
                        end_utc_ms: at.utc.get().as_millisecond(),
                    };
                    if period.end_utc_ms.saturating_sub(period.start_utc_ms)
                        >= MIN_ANNOTATION_IDLE_MS
                    {
                        self.pending = Some(period);
                    }
                }
            }
        }
    }
    pub(crate) fn visible(&self, now_ms: i64) -> Option<IdlePeriod> {
        self.pending.filter(|period| {
            (0..ANNOTATION_WINDOW_MS).contains(&now_ms.saturating_sub(period.end_utc_ms))
        })
    }
    pub(crate) fn completed(&mut self, period: IdlePeriod) {
        if self.pending == Some(period) {
            self.pending = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_domain::capture::{MonoInstant, UtcInstant};
    fn at(ms: i64) -> Timestamp {
        Timestamp {
            utc: UtcInstant::try_from(ms).expect("UTC"),
            mono: MonoInstant::from_millis(u64::try_from(ms).expect("positive time")),
        }
    }
    fn ended(duration: i64) -> QuickAnnotation {
        let mut annotation = QuickAnnotation::default();
        let id = RowId::from_persisted(1);
        annotation.observe(IdleAvailability::Opened { id, start: at(0) });
        annotation.observe(IdleAvailability::Closed {
            id,
            at: at(duration),
            reason: AvailabilityEndReason::Resumed,
        });
        annotation
    }
    #[test]
    fn rest_quick_action_emits_label_preset_without_label() {
        let id = AnnotationId(
            "abcdef00-0000-4000-8000-000000000001"
                .parse()
                .expect("UUID"),
        );
        let request = AnnotationChoice::Rest.request(
            IdlePeriod {
                start_utc_ms: 0,
                end_utc_ms: 1000,
            },
            id,
        );
        let wire = serde_json::to_value(request).expect("quick action request");
        assert_eq!(wire["entities"][0]["label_preset"], "rest");
        assert_eq!(wire["entities"][0]["label"], serde_json::Value::Null);
    }

    #[test]
    fn prompt_obeys_idle_threshold_window_and_successful_choice() {
        assert!(
            ended(MIN_ANNOTATION_IDLE_MS - 1)
                .visible(MIN_ANNOTATION_IDLE_MS)
                .is_none()
        );
        let mut annotation = ended(MIN_ANNOTATION_IDLE_MS);
        let period = annotation
            .visible(MIN_ANNOTATION_IDLE_MS)
            .expect("threshold included");
        assert_eq!(period.minutes(), 15);
        assert!(annotation.visible(MIN_ANNOTATION_IDLE_MS - 1).is_none());
        assert!(
            annotation
                .visible(MIN_ANNOTATION_IDLE_MS + ANNOTATION_WINDOW_MS - 1)
                .is_some()
        );
        assert!(
            annotation
                .visible(MIN_ANNOTATION_IDLE_MS + ANNOTATION_WINDOW_MS)
                .is_none()
        );
        annotation.completed(period);
        assert!(annotation.visible(MIN_ANNOTATION_IDLE_MS).is_none());
    }
    #[test]
    fn midnight_fragments_keep_the_whole_idle_range_and_clock_jump_cancels_it() {
        let mut annotation = QuickAnnotation::default();
        let first = RowId::from_persisted(1);
        let second = RowId::from_persisted(2);
        annotation.observe(IdleAvailability::Opened {
            id: first,
            start: at(0),
        });
        annotation.observe(IdleAvailability::Closed {
            id: first,
            at: at(MIN_ANNOTATION_IDLE_MS / 2),
            reason: AvailabilityEndReason::MidnightSplit,
        });
        annotation.observe(IdleAvailability::Opened {
            id: second,
            start: at(MIN_ANNOTATION_IDLE_MS / 2),
        });
        annotation.observe(IdleAvailability::Closed {
            id: second,
            at: at(MIN_ANNOTATION_IDLE_MS),
            reason: AvailabilityEndReason::Resumed,
        });
        assert_eq!(
            annotation.visible(MIN_ANNOTATION_IDLE_MS),
            Some(IdlePeriod {
                start_utc_ms: 0,
                end_utc_ms: MIN_ANNOTATION_IDLE_MS
            })
        );
        annotation.observe(IdleAvailability::Opened {
            id: first,
            start: at(0),
        });
        annotation.observe(IdleAvailability::Closed {
            id: first,
            at: at(MIN_ANNOTATION_IDLE_MS),
            reason: AvailabilityEndReason::ClockJump,
        });
        assert!(annotation.visible(MIN_ANNOTATION_IDLE_MS).is_none());
    }
    #[test]
    fn choices_preserve_range_identity_and_exact_storage_mapping() {
        let period = IdlePeriod {
            start_utc_ms: 12,
            end_utc_ms: 1_000_000,
        };
        let id = AnnotationId(
            "abcdef00-0000-4000-8000-000000000001"
                .parse()
                .expect("UUID"),
        );
        for choice in [
            AnnotationChoice::Rest,
            AnnotationChoice::Meeting,
            AnnotationChoice::Reading,
            AnnotationChoice::Ignore,
        ] {
            let request = choice.request(period, id.clone());
            let entity = &request.entities[0];
            assert_eq!(entity.uuid, id);
            assert_eq!(entity.values.start_utc_ms, period.start_utc_ms);
            assert_eq!(entity.values.end_utc_ms, period.end_utc_ms);
            assert_eq!(entity.values.note, None);
            assert_eq!(entity.values.label, None);
            assert_eq!(
                entity.kind,
                match choice {
                    AnnotationChoice::Rest => AnnotationKind::Note,
                    AnnotationChoice::Ignore => AnnotationKind::DismissedAway,
                    _ => AnnotationKind::OfflineActivity,
                }
            );
            assert_eq!(
                entity.values.label_preset,
                if choice == AnnotationChoice::Rest {
                    Some(AnnotationLabelPreset::Rest)
                } else {
                    None
                }
            );
            assert_eq!(
                entity.values.category,
                match choice {
                    AnnotationChoice::Meeting => Some(AnnotationCategory::Builtin {
                        key: BuiltinCategory::Communication
                    }),
                    AnnotationChoice::Reading => Some(AnnotationCategory::Builtin {
                        key: BuiltinCategory::Learning
                    }),
                    _ => None,
                }
            );
        }
    }
}
