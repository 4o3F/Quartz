use super::*;
use crate::{
    query::QueryJob,
    store::query::test_support::{FIXTURE_DATE, QueryFixture},
};
use quartz_domain::capture::UtcInstant;
use serde_json::{Value, json};

struct Fixture {
    facts: QueryFixture,
    registry: RegistryShell,
}
impl Fixture {
    fn new(count: i64) -> Self {
        let mut facts = QueryFixture::new(count);
        let registry = RegistryShell::open(&mut facts.store, UtcInstant::try_from(0).expect("UTC"))
            .expect("registry");
        Self { facts, registry }
    }
    fn text(kind: &str, rules: &str) -> String {
        format!(
            "format='quartz-rules/1'\n[set]\nkey='user.preview'\nkind='{kind}'\nname='Preview'\n{rules}"
        )
    }
    fn classification() -> &'static str {
        "[[rule]]\nid='first'\nlabel='First'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='starts_with',value='app-'}]\n[[rule]]\nid='shadowed'\nlabel='Shadowed'\ncategory='builtin:development'\nwhen=[{field='app.exe_name',op='starts_with',value='app-'}]\n"
    }
    fn privacy() -> &'static str {
        "[[rule]]\nid='one'\nlabel='One'\npreset='title_hidden'\nwhen=[{field='app.exe_name',op='starts_with',value='app-'}]\n[[rule]]\nid='two'\nlabel='Two'\npreset='title_hidden'\nwhen=[{field='app.exe_name',op='starts_with',value='app-'}]\n"
    }
    fn extraction() -> &'static str {
        "[[rule]]\nid='document'\nlabel='Document'\napp=[{field='app.exe_name',op='starts_with',value='app-'}]\npattern='private (?P<document>.+)'\n"
    }
    fn body(&self, op: &str, key: &str, text: &str) -> Value {
        let base = self
            .registry
            .snapshots()
            .load()
            .state()
            .sets
            .get(&key.parse().expect("key"))
            .map_or(0, |entry| entry.current.version);
        json!({
            "mutation": {"op": op, "body": {"key": key, "base_version": base, "text": text}},
            "start_date": FIXTURE_DATE,
            "end_date": "2025-01-03",
        })
    }
    fn edit_body(&self, text: &str) -> Value {
        self.body("rule_sets.edit", "user.preview", text)
    }
    fn run(&mut self, body: Value) -> Result<wire::RulesPreviewResponse, RunQueryError> {
        let request = PreviewRequest::decode(body)?;
        let mut reader = self.facts.store.take_query_reader();
        let mut job = match PreviewJob::start(&mut reader, request, &self.registry) {
            Ok(job) => job,
            Err(error) => {
                if let Some(reader) = reader {
                    self.facts.store.return_query_reader(reader);
                }
                return Err(error);
            }
        };
        let result = loop {
            match job.advance() {
                Ok(Some(QueryResponse::Preview(response))) => break Ok(response),
                Ok(None) => {}
                Ok(Some(_)) => panic!("preview response"),
                Err(error) => break Err(error),
            }
        };
        self.facts.store.return_query_reader(job.finish()?);
        result
    }
    fn day_with_rules(&mut self, rules: Arc<RuleSnapshot>) -> wire::QueryDayResponse {
        let mut reader = self.facts.store.take_query_reader();
        let completed = QueryJob::start(&mut reader, self.facts.request_day(), rules)
            .expect("query day start")
            .run_measured()
            .expect("query day");
        self.facts.store.return_query_reader(completed.reader);
        let QueryResponse::Day(day) = completed.response else {
            panic!("day")
        };
        day
    }
    fn day_category_totals(day: &wire::QueryDayResponse) -> Vec<wire::PreviewCategoryRank> {
        let mut by_id: BTreeMap<Option<wire::CategoryId>, wire::PreviewCategoryRank> =
            BTreeMap::new();
        for hour in &day.hours {
            for bucket in &hour.categories {
                if bucket.duration_ms > 0 {
                    by_id
                        .entry(bucket.category.id())
                        .or_insert_with(|| wire::PreviewCategoryRank {
                            category: bucket.category.clone(),
                            active_ms: 0,
                        })
                        .active_ms += bucket.duration_ms;
                }
            }
        }
        let mut categories: Vec<_> = by_id.into_values().collect();
        categories.sort_by(|left, right| {
            right
                .active_ms
                .cmp(&left.active_ms)
                .then(left.category.id().cmp(&right.category.id()))
        });
        categories
    }
}

#[test]
fn production_and_draft_totals_equal_query_day() {
    let mut fixture = Fixture::new(4);
    fixture.facts.mask_idle(1000, 2000);
    fixture.facts.annotate(
        1,
        wire::AnnotationKind::OfflineActivity,
        1000,
        2000,
        Some(wire::BuiltinCategory::Learning),
    );
    fixture.facts.annotate(
        2,
        wire::AnnotationKind::CategoryOverride,
        2000,
        3000,
        Some(wire::BuiltinCategory::Development),
    );
    let text = Fixture::text("classification", Fixture::classification());
    let body = fixture.edit_body(&text);
    let draft = PreviewRequest::decode(body.clone())
        .expect("decode")
        .mutation
        .draft(&fixture.registry)
        .expect("draft");
    let before_day = fixture.day_with_rules(fixture.registry.snapshots().load());
    let after_day = fixture.day_with_rules(draft);
    let response = fixture.run(body).expect("preview");
    assert_eq!(
        response.totals.before.active_ms,
        before_day.metrics.active_ms
    );
    assert_eq!(response.totals.after.active_ms, after_day.metrics.active_ms);
    assert_eq!(
        response.totals.before.categories,
        Fixture::day_category_totals(&before_day)
    );
    assert_eq!(
        response.totals.after.categories,
        Fixture::day_category_totals(&after_day)
    );
    assert_eq!(response.document_change_count, 0);
    assert_eq!(response.further_hidden_count, 0);
    assert!(!response.samples_truncated);
}

#[test]
fn shadowed_rules_distinguish_condition_matches_from_effective_time() {
    let mut fixture = Fixture::new(4);
    fixture.facts.mask_idle(1000, 2000);
    let body = fixture.edit_body(&Fixture::text("classification", Fixture::classification()));
    let response = fixture.run(body).expect("classification preview");
    let first = response
        .rules
        .iter()
        .find(|row| row.rule == "user.preview/first@1")
        .expect("first");
    let shadowed = response
        .rules
        .iter()
        .find(|row| row.rule == "user.preview/shadowed@1")
        .expect("shadowed");
    assert_eq!(first.matched_count, 4);
    assert_eq!(first.matched_ms, 4000);
    assert_eq!(first.effective_ms, 3000);
    assert_eq!(shadowed.matched_count, 4);
    assert_eq!(shadowed.matched_ms, 4000);
    assert_eq!(shadowed.effective_ms, 0);

    let rules = format!(
        "{}{}",
        Fixture::extraction(),
        Fixture::extraction().replace("id='document'", "id='shadowed'")
    );
    let response = fixture
        .run(fixture.edit_body(&Fixture::text("extraction", &rules)))
        .expect("extraction preview");
    let selected = response
        .rules
        .iter()
        .find(|row| row.rule == "user.preview/document@1")
        .expect("selected");
    let shadowed = response
        .rules
        .iter()
        .find(|row| row.rule == "user.preview/shadowed@1")
        .expect("shadowed");
    assert_eq!(selected.matched_ms, 4000);
    assert_eq!(selected.effective_ms, 3000);
    assert_eq!(shadowed.matched_ms, 4000);
    assert_eq!(shadowed.effective_ms, 0);
    assert_eq!(response.document_change_count, 3);
}

#[test]
fn annotation_override_takes_precedence_over_rule_effective_time() {
    let mut fixture = Fixture::new(4);
    fixture.facts.annotate(
        1,
        wire::AnnotationKind::CategoryOverride,
        1000,
        3000,
        Some(wire::BuiltinCategory::Development),
    );
    let body = fixture.edit_body(&Fixture::text("classification", Fixture::classification()));
    let response = fixture.run(body).expect("preview");
    let first = response
        .rules
        .iter()
        .find(|row| row.rule == "user.preview/first@1")
        .expect("first");
    assert_eq!(first.matched_count, 4);
    assert_eq!(first.matched_ms, 4000);
    assert_eq!(first.effective_ms, 2000);
}

#[test]
fn privacy_redacts_hidden_title_samples_and_counts_further_hidden_facts() {
    let mut fixture = Fixture::new(4);
    fixture.facts.mask_idle(1000, 2000);
    let privacy_body = fixture.edit_body(&Fixture::text("privacy", Fixture::privacy()));
    let privacy = fixture.run(privacy_body).expect("privacy preview");
    assert_eq!(privacy.further_hidden_count, 4);
    assert_eq!(privacy.totals.before, privacy.totals.after);
    assert!(
        !serde_json::to_string(&privacy.samples)
            .expect("privacy samples")
            .contains("private title fixture")
    );

    let mut hidden_fixture = Fixture::new(4);
    hidden_fixture.facts.hide_titles();
    let hidden_privacy = hidden_fixture
        .run(hidden_fixture.edit_body(&Fixture::text("privacy", Fixture::privacy())))
        .expect("already hidden titles");
    assert_eq!(hidden_privacy.further_hidden_count, 0);
    assert!(
        !serde_json::to_string(&hidden_privacy.samples)
            .expect("hidden samples")
            .contains("private title fixture")
    );
    let extraction = hidden_fixture
        .run(hidden_fixture.edit_body(&Fixture::text("extraction", Fixture::extraction())))
        .expect("extraction on hidden titles");
    assert!(extraction.samples.is_empty());
    assert_eq!(extraction.document_change_count, 0);
}

#[test]
fn seven_day_bound_and_invalid_dates_are_enforced() {
    let mut fixture = Fixture::new(4);
    let text = Fixture::text("classification", Fixture::classification());
    let mut seven_days = fixture.edit_body(&text);
    seven_days["start_date"] = json!("2024-12-27");
    seven_days["end_date"] = json!("2025-01-03");
    let response = fixture.run(seven_days).expect("seven logical days");
    assert_eq!(response.totals.after.active_ms, 4000);

    for (start, end) in [
        ("2025-01-02", "2025-01-02"),
        ("2025-01-03", "2025-01-02"),
        ("2024-12-26", "2025-01-03"),
    ] {
        let mut bad = fixture.edit_body(&text);
        bad["start_date"] = json!(start);
        bad["end_date"] = json!(end);
        assert_eq!(
            fixture.run(bad).expect_err("invalid range").kind(),
            ErrorKind::InvalidRange
        );
    }
}

#[test]
fn rules_text_import_mutation_evaluates_without_persisting() {
    let mut fixture = Fixture::new(4);
    let before = fixture.registry.snapshots().load();
    let body = fixture.body(
        "rules_text.import",
        "user.imported",
        &Fixture::text("classification", Fixture::classification())
            .replace("user.preview", "user.imported"),
    );
    let response = fixture.run(body).expect("import preview");
    let rule = response
        .rules
        .iter()
        .find(|row| row.rule == "user.imported/first@1")
        .expect("imported rule");
    assert_eq!(rule.effective_ms, 4000);
    assert!(Arc::ptr_eq(&before, &fixture.registry.snapshots().load()));
}

#[test]
fn oversized_or_capped_samples_truncate_and_set_flag_without_error() {
    let mut fixture = Fixture::new(60);
    let capped = fixture
        .run(fixture.edit_body(&Fixture::text("classification", Fixture::classification())))
        .expect("capped preview");
    assert_eq!(capped.samples.len(), wire::PREVIEW_MAX_SAMPLES);
    assert!(capped.samples_truncated);

    let mut small = Fixture::new(4);
    let mut response = small
        .run(small.edit_body(&Fixture::text("extraction", Fixture::extraction())))
        .expect("extraction preview");
    assert_eq!(response.samples.len(), 4);
    assert!(!response.samples_truncated);
    let huge = "x".repeat(wire::MAX_FRAME_LEN / 4);
    for sample in &mut response.samples {
        if let wire::RulePreviewOutput::Extraction { extraction } = &mut sample.after {
            extraction.document = Some(huge.clone());
        }
    }
    let fitted = fit_to_frame(response, PREVIEW_MAX_RESPONSE_BYTES).expect("frame fit");
    assert!(fitted.samples_truncated);
    assert!(fitted.samples.len() < 4);
    let encoded = wire::encode_json(&wire::Envelope::Response(wire::Response {
        id: Some(wire::RequestId(1)),
        outcome: wire::ResponseOutcome::Ok {
            body: serde_json::to_value(&fitted).expect("response JSON"),
        },
    }))
    .expect("response fits within frame limit");
    assert!(encoded.len() <= wire::MAX_FRAME_LEN);
}
