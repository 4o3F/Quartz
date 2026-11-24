//! UI quick annotation mappings mirror the tray's stored annotation semantics.
use super::UtcRange;
use quartz_protocol::{
    AnnotationCategory, AnnotationId, AnnotationKind, AnnotationLabelPreset, AnnotationValues,
    AnnotationsCreateRequest, BuiltinCategory, CreateAnnotation,
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
    fn rest_quick_action_emits_label_preset_without_label() {
        let id = AnnotationId(
            "abcdef00-0000-4000-8000-000000000001"
                .parse()
                .expect("UUID"),
        );
        let request = AwayAction::Rest.request(
            UtcRange {
                start: 0,
                end: 1000,
            },
            id,
        );
        let wire = serde_json::to_value(request).expect("quick action request");
        let entity = wire
            .get("entities")
            .and_then(|entities| entities.get(0))
            .expect("quick annotation");
        assert_eq!(entity.get("label_preset"), Some(&serde_json::json!("rest")));
        assert_eq!(entity.get("label"), Some(&serde_json::Value::Null));
    }

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
                    assert_eq!(entity.values.label, None);
                    assert_eq!(
                        entity.values.label_preset,
                        Some(AnnotationLabelPreset::Rest)
                    );
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
