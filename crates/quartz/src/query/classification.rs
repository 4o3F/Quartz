//! Per-query classification using the retained rule and SQLite snapshots.

use super::pipeline::CategorySource;
use quartz_domain::{
    categories::{Category, CategoryUuid},
    facts::{ApplicationId, CaptureLevel, Foreground, ForegroundKind, TitleId},
    rules::{Field, FieldSnapshot, registry::RuleSnapshot},
    text::NfcText,
};
use quartz_protocol::CategoryRef;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

struct QueryCategory {
    uuid: CategoryUuid,
    wire: Arc<quartz_protocol::Category>,
}
impl From<Category> for QueryCategory {
    fn from(category: Category) -> Self {
        use quartz_domain::categories::{BuiltinCategory, CategoryTendency};
        Self {
            uuid: category.uuid.clone(),
            wire: Arc::new(quartz_protocol::Category {
                id: quartz_protocol::CategoryId(category.id.0),
                uuid: quartz_protocol::CategoryUuid(category.uuid.0),
                builtin_key: category.builtin_key.map(|key| match key {
                    BuiltinCategory::Development => quartz_protocol::BuiltinCategory::Development,
                    BuiltinCategory::Learning => quartz_protocol::BuiltinCategory::Learning,
                    BuiltinCategory::Communication => {
                        quartz_protocol::BuiltinCategory::Communication
                    }
                    BuiltinCategory::Entertainment => {
                        quartz_protocol::BuiltinCategory::Entertainment
                    }
                    BuiltinCategory::System => quartz_protocol::BuiltinCategory::System,
                }),
                name: category.name,
                color: category.color.to_string(),
                tendency: match category.tendency {
                    CategoryTendency::Focus => quartz_protocol::CategoryTendency::Focus,
                    CategoryTendency::Neutral => quartz_protocol::CategoryTendency::Neutral,
                    CategoryTendency::Distraction => quartz_protocol::CategoryTendency::Distraction,
                },
            }),
        }
    }
}
/// Query aggregation treats unclassified time as neutral.
pub(super) trait QueryCategoryRef {
    fn tendency(&self) -> quartz_protocol::CategoryTendency;
    fn id(&self) -> Option<quartz_protocol::CategoryId>;
}
impl QueryCategoryRef for CategoryRef {
    fn tendency(&self) -> quartz_protocol::CategoryTendency {
        match self {
            Self::Unclassified => quartz_protocol::CategoryTendency::Neutral,
            Self::Classified { category } => category.tendency,
        }
    }
    fn id(&self) -> Option<quartz_protocol::CategoryId> {
        match self {
            Self::Unclassified => None,
            Self::Classified { category } => Some(category.id),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
#[cfg_attr(test, derive(Clone))]
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
#[cfg_attr(test, derive(Clone))]
pub(crate) struct Classifier {
    rules: Arc<RuleSnapshot>,
    categories: BTreeMap<CategoryUuid, Arc<quartz_protocol::Category>>,
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
                .map(QueryCategory::from)
                .map(|category| (category.uuid, category.wire))
                .collect(),
            required_fields,
            cache: HashMap::new(),
            results: vec![Classification::unclassified()],
        }
    }
    #[cfg(test)]
    pub(super) fn result_mut(&mut self, id: ClassificationId) -> &mut Classification {
        &mut self.results[id.0]
    }
    pub(super) fn result(&self, id: ClassificationId) -> &Classification {
        &self.results[id.0]
    }
    pub(super) fn classify(&mut self, row: &Foreground) -> ClassificationId {
        if row.capture_level() == CaptureLevel::Excluded
            || !matches!(row.kind, ForegroundKind::Window)
        {
            return ClassificationId::UNCLASSIFIED;
        }
        let key = MatchKey {
            app: row.visible_app().map(|app| app.id),
            title: if self.required_fields.contains(&Field::RawTitle) {
                row.visible_title_id()
            } else {
                None
            },
            level: row.capture_level(),
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
        if let Some(app) = row.visible_app() {
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
        if self.required_fields.contains(&Field::RawTitle)
            && let Some(title) = row.visible_title()
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
                    source: CategorySource::Rule(std::sync::Arc::new(matched.rule)),
                },
                None => {
                    tracing::debug!(rule = matched.rule.as_str(), category = %matched.category.0, "classification rule references a missing or archived category");
                    Classification::unclassified()
                }
            },
        }
    }
}
