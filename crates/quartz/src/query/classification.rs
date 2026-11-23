//! Per-query classification using the retained rule and SQLite snapshots.

use super::pipeline::CategorySource;
use quartz_domain::{
    categories::{Category, CategoryUuid},
    facts::{ApplicationId, CaptureLevel, Foreground, ForegroundKind, TitleId},
    rules::{Field, FieldSnapshot, PreparedClassifier, PreparedExtractor, registry::RuleSnapshot},
};
use quartz_protocol::CategoryRef;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

pub(super) use quartz_domain::rules::ExtractedTitle;
#[derive(Debug, PartialEq, Eq, Hash)]
#[cfg_attr(test, derive(Clone))]
struct ExtractionKey {
    app: ApplicationId,
    title: Option<TitleId>,
    level: CaptureLevel,
}

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
    window_class: Option<String>,
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
    extraction_fields: BTreeSet<Field>,
    extractions: HashMap<ExtractionKey, Arc<ExtractedTitle>>,
    extractors: HashMap<ApplicationId, PreparedExtractor>,
    classifiers: HashMap<Option<ApplicationId>, PreparedClassifier>,
    // The retained rule and database snapshots fix rule hashes and metadata for both caches.
    cache: HashMap<MatchKey, ClassificationId>,
    results: Vec<Classification>,
}
impl Classifier {
    pub(crate) fn new(rules: Arc<RuleSnapshot>, categories: Vec<Category>) -> Self {
        let required_fields = rules.classification().fields().collect();
        let extraction_fields = rules
            .extraction()
            .fields()
            .chain([Field::DisplayName])
            .collect();
        Self {
            rules,
            categories: categories
                .into_iter()
                .map(QueryCategory::from)
                .map(|category| (category.uuid, category.wire))
                .collect(),
            required_fields,
            extraction_fields,
            extractions: HashMap::new(),
            extractors: HashMap::new(),
            classifiers: HashMap::new(),
            cache: HashMap::new(),
            results: vec![Classification::unclassified()],
        }
    }
    pub(super) fn annotation(
        &mut self,
        annotation: &crate::store::query::QueryAnnotation,
    ) -> ClassificationId {
        let category = annotation
            .category
            .and_then(|id| {
                self.categories
                    .values()
                    .find(|category| category.id.0 == id.0)
            })
            .cloned();
        if let Some(category_id) = annotation.category
            && category.is_none()
        {
            tracing::debug!(annotation = %annotation.id.0, category = category_id.0, "annotation references a missing or archived category");
        }
        let id = ClassificationId(self.results.len());
        self.results.push(Classification {
            category: category.map_or(CategoryRef::Unclassified, |category| {
                CategoryRef::Classified { category }
            }),
            source: CategorySource::Annotation(annotation.id.clone()),
        });
        id
    }
    #[cfg(test)]
    pub(super) fn result_mut(&mut self, id: ClassificationId) -> &mut Classification {
        &mut self.results[id.0]
    }
    pub(super) fn result(&self, id: ClassificationId) -> &Classification {
        &self.results[id.0]
    }
    pub(super) fn rule_details(
        &self,
        id: &quartz_domain::rules::ExplanationId,
        category: CategoryRef,
    ) -> quartz_protocol::EntityClassificationRule {
        let rule = self
            .rules
            .classification()
            .rule(id)
            .expect("classification source belongs to retained rule snapshot");
        quartz_protocol::EntityClassificationRule {
            rule: id
                .as_str()
                .parse()
                .expect("domain revision satisfies wire grammar"),
            label: rule.label.clone(),
            label_key: rule
                .builtin
                .as_ref()
                .and_then(|metadata| metadata.label_key.clone()),
            category,
        }
    }
    pub(super) fn extract(&mut self, row: &Foreground) -> Option<Arc<ExtractedTitle>> {
        if row.capture_level() != CaptureLevel::Full || !matches!(row.kind, ForegroundKind::Window)
        {
            return None;
        }
        let app = row.visible_app()?.id;
        let key = ExtractionKey {
            app,
            title: row.visible_title_id(),
            level: row.capture_level(),
        };
        Some(
            self.extractions
                .entry(key)
                .or_insert_with(|| {
                    let extractor = self.extractors.entry(app).or_insert_with(|| {
                        let fields = FieldSnapshot::from_foreground(
                            row,
                            self.extraction_fields.iter().copied(),
                            None,
                        );
                        self.rules.extraction().prepare_extractor(&fields)
                    });
                    Arc::new(extractor.extract(row.visible_title()))
                })
                .clone(),
        )
    }
    pub(super) fn classify(
        &mut self,
        row: &Foreground,
        extraction: Option<&ExtractedTitle>,
    ) -> ClassificationId {
        if row.capture_level() == CaptureLevel::Excluded
            || !matches!(row.kind, ForegroundKind::Window)
        {
            return ClassificationId::UNCLASSIFIED;
        }
        let app = row.visible_app().map(|app| app.id);
        let plan = self.classifiers.entry(app).or_insert_with(|| {
            let app_fields = self.required_fields.iter().copied().filter(|field| {
                matches!(
                    field,
                    Field::IdentityKey | Field::ExeName | Field::DisplayName
                )
            });
            let fields = FieldSnapshot::from_foreground(row, app_fields, None);
            self.rules.classification().prepare_classifier(&fields)
        });
        let key = MatchKey {
            app: if self.required_fields.iter().any(|field| {
                matches!(
                    field,
                    Field::IdentityKey
                        | Field::ExeName
                        | Field::DisplayName
                        | Field::NormalizedTitle
                        | Field::Document
                        | Field::Project
                        | Field::Profile
                )
            }) {
                row.visible_app().map(|app| app.id)
            } else {
                None
            },
            title: if plan.fields().any(|field| {
                matches!(
                    field,
                    Field::RawTitle
                        | Field::NormalizedTitle
                        | Field::Document
                        | Field::Project
                        | Field::Profile
                )
            }) {
                row.visible_title_id()
            } else {
                None
            },
            window_class: if plan.fields().any(|field| field == Field::WindowClass) {
                row.visible_window_class().map(str::to_owned)
            } else {
                None
            },
            level: row.capture_level(),
        };
        if let Some(result) = self.cache.get(&key) {
            return *result;
        }
        let result = self.evaluate(row, extraction);
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
    fn evaluate(&self, row: &Foreground, extraction: Option<&ExtractedTitle>) -> Classification {
        let app = row.visible_app().map(|app| app.id);
        match self
            .classifiers
            .get(&app)
            .expect("application classification is prepared before evaluation")
            .classify_foreground(row, extraction.map(|value| &value.fields))
        {
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

#[cfg(test)]
mod tests;
