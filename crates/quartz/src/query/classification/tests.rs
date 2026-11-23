use super::*;
use crate::query::tests::{categories, classified_window, classifier, edit_foreground};
use quartz_domain::{
    facts::ForegroundContent,
    rules::registry::{RegistryCommand, RegistryState, RuleRegistry},
};

fn foreground() -> Foreground {
    let crate::query::input::InputRow::Foreground(row) = classified_window(1, 0, 10, "code.exe")
    else {
        panic!("foreground fixture");
    };
    row
}

fn isolated_classifier(field: Field) -> Classifier {
    let categories = categories();
    let mut state = RegistryState::default();
    state.references.categories.insert(
        "Learning".into(),
        categories
            .iter()
            .find(|category| {
                category.builtin_key == Some(quartz_domain::categories::BuiltinCategory::Learning)
            })
            .expect("learning category")
            .uuid
            .0
            .clone(),
    );
    let mut registry = RuleRegistry::from_state(state).expect("registry");
    let source = format!(
        "format='quartz-rules/1'\n[set]\nkey='user.single'\nkind='classification'\nname='Single field'\n[[rule]]\nid='match'\nlabel='Match field'\ncategory='Learning'\nwhen=[{{field='{}',op='contains',value='document'}}]",
        field.as_str()
    );
    let decision = registry
        .decide(RegistryCommand::Apply {
            key: "user.single".parse().expect("key"),
            file: "single.toml".into(),
            text: source,
            base_version: 0,
        })
        .into_parts();
    decision.reply.expect("install decision");
    registry
        .committed(decision.outcome.expect("write"))
        .expect("publish");
    Classifier::new(registry.snapshots().load(), categories)
}

#[test]
fn extraction_is_shared_by_application_title_and_snapshot_and_obeys_privacy() {
    let mut classifier = classifier(&[]);
    let mut row = foreground();
    edit_foreground(&mut row, CaptureLevel::Full, |content| {
        content.title = Some("● facts.rs - Quartz - Visual Studio Code".into());
    });
    let first = classifier.extract(&row).expect("full extraction");
    assert_eq!(first.fields.document.as_deref(), Some("facts.rs"));
    assert_eq!(first.fields.project.as_deref(), Some("Quartz"));
    assert_eq!(
        first.extractor_rule.as_deref(),
        Some("builtin.extraction/vscode-project@1")
    );
    assert!(first.fields.dirty);
    row.span.id.0 += 1;
    edit_foreground(&mut row, CaptureLevel::Full, |content| {
        content.window_class = Some("AnotherWindowClass".into());
    });
    assert!(Arc::ptr_eq(
        &first,
        &classifier.extract(&row).expect("same title")
    ));
    assert_eq!(classifier.extractions.len(), 1);
    edit_foreground(&mut row, CaptureLevel::Full, |content| {
        content.title_id = Some(TitleId(2));
        content.title = Some("other.rs - Quartz - Visual Studio Code".into());
    });
    assert!(!Arc::ptr_eq(
        &first,
        &classifier.extract(&row).expect("another title")
    ));
    assert_eq!(classifier.extractions.len(), 2);
    edit_foreground(&mut row, CaptureLevel::Full, |content| {
        Arc::make_mut(content.app.as_mut().expect("app")).id = ApplicationId(2);
    });
    classifier.extract(&row).expect("another app");
    assert_eq!(classifier.extractions.len(), 3);
    let mut another_query = crate::query::tests::classifier(&[]);
    assert!(!Arc::ptr_eq(
        &first,
        &another_query.extract(&foreground()).expect("new query")
    ));
    for level in [CaptureLevel::TitleHidden, CaptureLevel::Excluded] {
        let hidden = Foreground::new(
            row.span.clone(),
            row.kind.clone(),
            level,
            ForegroundContent {
                app: row.visible_app().cloned(),
                title: Some("private".into()),
                title_id: Some(TitleId(3)),
                ..Default::default()
            },
        );
        assert!(classifier.extract(&hidden).is_none());
    }
    assert_eq!(classifier.extractions.len(), 3);
}

#[test]
fn classification_cache_uses_only_effective_field_dependencies() {
    for field in [
        Field::ExeName,
        Field::RawTitle,
        Field::NormalizedTitle,
        Field::Document,
        Field::Project,
        Field::Profile,
        Field::WindowClass,
    ] {
        let mut classifier = isolated_classifier(field);
        let mut row = foreground();
        classifier.classify(&row, None);
        let key = classifier.cache.keys().next().expect("cache key");
        let needs_extraction = matches!(
            field,
            Field::NormalizedTitle | Field::Document | Field::Project | Field::Profile
        );
        assert_eq!(
            key.app.is_some(),
            field == Field::ExeName || needs_extraction
        );
        assert_eq!(
            key.title.is_some(),
            field == Field::RawTitle || needs_extraction
        );
        edit_foreground(&mut row, CaptureLevel::Full, |content| {
            content.title_id = Some(TitleId(2));
            content.title = Some("another document".into());
        });
        classifier.classify(&row, None);
        let title_entries = if field == Field::RawTitle || needs_extraction {
            2
        } else {
            1
        };
        assert_eq!(classifier.cache.len(), title_entries, "{field:?}");
        edit_foreground(&mut row, CaptureLevel::Full, |content| {
            content.window_class = Some("document".into());
        });
        let matched = classifier.classify(&row, None);
        assert_eq!(
            classifier.cache.len(),
            title_entries + usize::from(field == Field::WindowClass),
            "{field:?}"
        );
        if field == Field::WindowClass {
            assert!(matches!(
                classifier.result(matched).category,
                CategoryRef::Classified { .. }
            ));
            edit_foreground(&mut row, CaptureLevel::TitleHidden, |_| {});
            let matched = classifier.classify(&row, None);
            assert!(matches!(
                classifier.result(matched).category,
                CategoryRef::Classified { .. }
            ));
        }
        let entries = classifier.cache.len();
        edit_foreground(&mut row, CaptureLevel::Excluded, |_| {});
        let excluded = classifier.classify(&row, None);
        assert!(matches!(
            classifier.result(excluded).category,
            CategoryRef::Unclassified
        ));
        assert_eq!(classifier.cache.len(), entries);
    }
}

#[test]
fn builtin_application_results_reuse_classification_while_every_title_is_extracted() {
    let mut classifier = classifier(&[]);
    let mut row = foreground();
    let first = classifier.extract(&row).expect("first extraction");
    classifier.classify(&row, Some(&first));
    edit_foreground(&mut row, CaptureLevel::Full, |content| {
        content.title_id = Some(TitleId(2));
        content.title = Some("another document".into());
    });
    let second = classifier.extract(&row).expect("second extraction");
    classifier.classify(&row, Some(&second));
    assert_eq!(classifier.extractions.len(), 2);
    assert_eq!(
        classifier.cache.len(),
        1,
        "document rules for another application do not add a title dependency"
    );
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(
        classifier.classifiers[&Some(ApplicationId(1))]
            .fields()
            .next()
            .is_none()
    );
}
