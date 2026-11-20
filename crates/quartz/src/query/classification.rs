//! Per-query classification using the retained rule and SQLite snapshots.

use quartz_domain::{
    categories::{Category, CategoryUuid},
    facts::{ApplicationId, CaptureLevel, Foreground, ForegroundKind, TitleId},
    rules::{Field, FieldSnapshot, registry::RuleSnapshot},
    text::NfcText,
};
use quartz_protocol::{CategoryRef, CategorySource};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

#[derive(Debug, PartialEq, Eq, Hash)]
struct MatchKey {
    app: Option<ApplicationId>,
    title: Option<TitleId>,
    level: CaptureLevel,
}
/// Index into the immutable results retained by one query's classifier.
#[derive(Debug, Clone, Copy)]
pub(super) struct ClassificationId(usize);
impl ClassificationId {
    /// Every classifier reserves its first result for absence of classification.
    pub(super) const UNCLASSIFIED: Self = Self(0);
}
#[derive(Debug, Clone)]
pub(crate) struct Classification {
    pub category: CategoryRef,
    pub source: CategorySource,
}
impl Classification {
    fn unclassified() -> Self {
        Self {
            category: CategoryRef::Unclassified,
            source: CategorySource::Unclassified,
        }
    }
}
#[derive(Debug)]
pub(crate) struct Classifier {
    rules: Arc<RuleSnapshot>,
    categories: BTreeMap<CategoryUuid, Arc<Category>>,
    required_fields: BTreeSet<Field>,
    // The retained snapshot fixes the rule hash for every entry in this cache.
    cache: HashMap<MatchKey, ClassificationId>,
    results: Vec<Classification>,
}
impl Classifier {
    pub(crate) fn new(rules: Arc<RuleSnapshot>, categories: Vec<Category>) -> Self {
        let required_fields = rules.classification().fields().collect();
        Self {
            rules,
            categories: categories
                .into_iter()
                .map(|category| (category.uuid.clone(), Arc::new(category)))
                .collect(),
            required_fields,
            cache: HashMap::new(),
            results: vec![Classification::unclassified()],
        }
    }
    pub(super) fn result(&self, id: ClassificationId) -> &Classification {
        &self.results[id.0]
    }
    pub(super) fn classify(&mut self, row: &Foreground) -> ClassificationId {
        if row.level == CaptureLevel::Excluded || !matches!(row.kind, ForegroundKind::Window) {
            return ClassificationId::UNCLASSIFIED;
        }
        let key = MatchKey {
            app: row.app.as_ref().map(|app| app.id),
            title: if row.level == CaptureLevel::Full
                && self.required_fields.contains(&Field::RawTitle)
            {
                row.title_id
            } else {
                None
            },
            level: row.level,
        };
        if let Some(result) = self.cache.get(&key) {
            return *result;
        }
        let result = self.evaluate(row);
        let id = if matches!(&result.category, CategoryRef::Unclassified) {
            ClassificationId::UNCLASSIFIED
        } else {
            let id = ClassificationId(self.results.len());
            self.results.push(result);
            id
        };
        self.cache.insert(key, id);
        id
    }
    fn evaluate(&self, row: &Foreground) -> Classification {
        let mut fields = FieldSnapshot::default();
        if let Some(app) = &row.app {
            if self.required_fields.contains(&Field::IdentityKey) {
                fields
                    .text
                    .insert(Field::IdentityKey, NfcText::new(&app.identity_key));
            }
            if self.required_fields.contains(&Field::DisplayName) {
                fields
                    .text
                    .insert(Field::DisplayName, NfcText::new(&app.display_name));
            }
            if self.required_fields.contains(&Field::ExeName)
                && let Some(exe) = &app.exe_name
            {
                fields.text.insert(Field::ExeName, NfcText::new(exe));
            }
        }
        if row.level == CaptureLevel::Full
            && self.required_fields.contains(&Field::RawTitle)
            && let Some(title) = &row.title
        {
            fields.text.insert(Field::RawTitle, NfcText::new(title));
        }
        match self.rules.classification().classify(&fields) {
            None => Classification::unclassified(),
            Some(matched) => match self.categories.get(&matched.category) {
                Some(category) => Classification {
                    category: CategoryRef::Classified {
                        category: category.clone(),
                    },
                    source: CategorySource::Rule { rule: matched.rule },
                },
                None => {
                    tracing::warn!(rule = matched.rule.as_str(), category = %matched.category.0, "classification rule references a missing or archived category");
                    Classification::unclassified()
                }
            },
        }
    }
}
