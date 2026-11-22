//! UI quick annotation mappings mirror the tray's stored annotation semantics.
use super::UtcRange;
use quartz_protocol::{
    AnnotationCategory, AnnotationId, AnnotationKind, AnnotationValues, AnnotationsCreateRequest,
    BuiltinCategory, CreateAnnotation,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AwayAction {
    Rest,
    Meeting,
    Reading,
    Ignore,
}
impl AwayAction {
    pub(crate) fn request(self, range: UtcRange, uuid: AnnotationId) -> AnnotationsCreateRequest {
        let mut entity = CreateAnnotation {
            uuid,
            kind: AnnotationKind::Note,
            values: AnnotationValues {
                start_utc_ms: range.start,
                end_utc_ms: range.end,
                category: None,
                label: None,
                note: None,
            },
        };
        match self {
            Self::Rest => entity.values.label = Some("休息".into()),
            Self::Meeting | Self::Reading => {
                entity.kind = AnnotationKind::OfflineActivity;
                entity.values.category = Some(AnnotationCategory::Builtin {
                    key: match self {
                        Self::Meeting => BuiltinCategory::Communication,
                        _ => BuiltinCategory::Learning,
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mappings_match_the_tray_and_keep_the_exact_range() {
        let range = UtcRange {
            start: 100,
            end: 1000,
        };
        let id = AnnotationId(
            "abcdef00-0000-4000-8000-000000000001"
                .parse()
                .expect("UUID"),
        );
        for action in [
            AwayAction::Rest,
            AwayAction::Meeting,
            AwayAction::Reading,
            AwayAction::Ignore,
        ] {
            let request = action.request(range, id.clone());
            let entity = request.entities.first().expect("annotation");
            assert_eq!(entity.values.start_utc_ms, range.start);
            assert_eq!(entity.values.end_utc_ms, range.end);
            match action {
                AwayAction::Rest => {
                    assert_eq!(entity.kind, AnnotationKind::Note);
                    assert_eq!(entity.values.label.as_deref(), Some("休息"));
                }
                AwayAction::Meeting => assert_eq!(
                    entity.values.category,
                    Some(AnnotationCategory::Builtin {
                        key: BuiltinCategory::Communication
                    })
                ),
                AwayAction::Reading => assert_eq!(
                    entity.values.category,
                    Some(AnnotationCategory::Builtin {
                        key: BuiltinCategory::Learning
                    })
                ),
                AwayAction::Ignore => {
                    assert_eq!(entity.kind, AnnotationKind::DismissedAway);
                    assert_eq!(entity.values.category, None);
                    assert_eq!(entity.values.label, None);
                }
            }
        }
    }
}
