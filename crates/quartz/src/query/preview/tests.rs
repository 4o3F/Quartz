use super::*;
use crate::{
    query::{QueryJob, QueryRequest},
    store::query::test_support::{FIXTURE_DATE, QueryFixture},
};
use quartz_domain::{capture::UtcInstant, rules::registry::RegistryCommand};
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
    fn body(&mut self, text: &str) -> Value {
        let request = QueryRequest::Range(wire::QueryRangeRequest {
            start_date: FIXTURE_DATE.parse().expect("date"),
            end_date: "2025-01-03".parse().expect("date"),
            bucket: wire::RangeBucket::Day,
            dimensions: vec![wire::RangeDimension::Category],
            cursor: None,
        });
        let QueryResponse::Range(response) = self.facts.run(request).expect("range digest") else {
            panic!("range response")
        };
        let base = self
            .registry
            .snapshots()
            .load()
            .state()
            .sets
            .get(&"user.preview".parse().expect("key"))
            .map_or(0, |entry| entry.current.version);
        json!({"mutation":{"op":"rule_sets.edit","body":{"key":"user.preview","base_version":base,"text":text}},"start_date":FIXTURE_DATE,"end_date":"2025-01-03","expected_versions":response.versions})
    }
    fn run(&mut self, body: Value) -> Result<wire::RulesPreviewResponse, RunQueryError> {
        let request = PreviewRequest::decode(wire::Op::RulesPreview, body)?;
        self.run_request(request)
    }
    fn run_request(
        &mut self,
        request: PreviewRequest,
    ) -> Result<wire::RulesPreviewResponse, RunQueryError> {
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
    fn install(&mut self, text: &str) {
        self.install_at("user.preview", text);
    }
    fn install_at(&mut self, key: &str, text: &str) {
        // Persist a fixture revision directly.
        let parts = self
            .registry
            .core()
            .decide(RegistryCommand::Apply {
                key: key.parse().expect("key"),
                file: "fixture.toml".into(),
                text: text.into(),
                base_version: self
                    .registry
                    .snapshots()
                    .load()
                    .state()
                    .sets
                    .get(&key.parse().expect("key"))
                    .map_or(0, |entry| entry.current.version),
            })
            .into_parts();
        parts.reply.expect("fixture decision");
        self.facts
            .store
            .rule_transaction(|transaction| transaction.execute(&parts.writes, 0))
            .expect("fixture commit");
        self.registry =
            RegistryShell::open(&mut self.facts.store, UtcInstant::try_from(0).expect("UTC"))
                .expect("reopen registry");
    }
    fn draft_day(&mut self, text: &str) -> wire::QueryDayResponse {
        let body = self.body(text);
        let request = PreviewRequest::decode(wire::Op::RulesPreview, body).expect("request");
        let draft = request.mutation.draft(&self.registry).expect("draft");
        self.day_with_rules(draft)
    }
    fn day_with_rules(&mut self, draft: Arc<RuleSnapshot>) -> wire::QueryDayResponse {
        let mut request = wire::QueryDayRequest {
            local_date: FIXTURE_DATE.parse().expect("date"),
            cursor: None,
        };
        let mut combined: Option<wire::QueryDayResponse> = None;
        loop {
            let mut reader = self.facts.store.take_query_reader();
            let completed = QueryJob::start(
                &mut reader,
                QueryRequest::Day(request.clone()),
                draft.clone(),
            )
            .expect("draft day")
            .run_measured()
            .expect("query day");
            self.facts.store.return_query_reader(completed.reader);
            let QueryResponse::Day(day) = completed.response else {
                panic!("day")
            };
            request.cursor = day.next_cursor.clone();
            match &mut combined {
                Some(combined) => {
                    combined.applications.extend(day.applications);
                    combined.documents.extend(day.documents);
                }
                None => combined = Some(day),
            }
            if request.cursor.is_none() {
                break;
            }
        }
        combined.expect("day response")
    }
}
#[test]
fn identity_preview_totals_equal_query_day_with_annotations_and_offline_activity() {
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
    fixture.facts.hide_titles();
    let text = Fixture::text("classification", "");
    let body = fixture.body(&text);
    let response = fixture.run(body).expect("preview");
    let totals = response.totals.expect("totals");
    assert_eq!(totals.before, totals.after);
    let QueryResponse::Day(day) = fixture.facts.run(fixture.facts.request_day()).expect("day")
    else {
        panic!("day")
    };
    assert_eq!(
        totals.before.active_ms,
        Some(day.metrics.expect("metrics").active_ms)
    );
    assert_eq!(totals.before.applications, day.applications);
    assert_eq!(totals.before.documents, day.documents);
    assert_eq!(
        totals
            .before
            .categories
            .iter()
            .map(|row| row.active_ms)
            .sum::<i64>(),
        4000
    );
}
#[test]
fn shadowed_rules_count_conditions_without_effective_contribution() {
    let mut fixture = Fixture::new(4);
    fixture.facts.mask_idle(1000, 2000);
    let text = Fixture::text("classification", Fixture::classification());
    let body = fixture.body(&text);
    let response = fixture.run(body).expect("preview");
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
    assert_eq!(first.condition_match.duration_ms, 4000);
    assert_eq!(first.condition_match.segments, 4);
    assert_eq!(first.effective_contribution.active_ms, 3000);
    assert_eq!(first.effective_contribution.segments, 3);
    assert_eq!(shadowed.condition_match, first.condition_match);
    assert_eq!(shadowed.effective_contribution.active_ms, 0);
    let totals = response.totals.expect("totals");
    let day = fixture.draft_day(&text);
    assert_eq!(
        totals.after.active_ms,
        Some(day.metrics.expect("metrics").active_ms)
    );
    assert_eq!(totals.after.applications, day.applications);
    assert_eq!(totals.after.documents, day.documents);
    assert_eq!(totals.after.categories.iter().find(|row|matches!(&row.category, wire::CategoryRef::Classified{category} if category.builtin_key==Some(wire::BuiltinCategory::Learning))).expect("learning").active_ms,3000);
}
#[test]
fn annotation_override_removes_rule_contribution_without_removing_condition_matches() {
    let mut fixture = Fixture::new(4);
    fixture.facts.annotate(
        1,
        wire::AnnotationKind::CategoryOverride,
        1000,
        3000,
        Some(wire::BuiltinCategory::Development),
    );
    let body = fixture.body(&Fixture::text("classification", Fixture::classification()));
    let response = fixture.run(body).expect("preview");
    let first = response
        .rules
        .iter()
        .find(|row| row.rule == "user.preview/first@1")
        .expect("first");
    assert_eq!(first.condition_match.duration_ms, 4000);
    assert_eq!(first.effective_contribution.active_ms, 2000);
}
#[test]
fn extraction_preview_reports_selected_outputs_and_active_contribution() {
    let mut fixture = Fixture::new(4);
    fixture.facts.mask_idle(1000, 2000);
    let body = fixture.body(&Fixture::text("extraction", Fixture::extraction()));
    let response = fixture.run(body).expect("preview");
    let row = response
        .rules
        .iter()
        .find(|row| row.rule == "user.preview/document@1")
        .expect("extraction");
    assert_eq!(row.condition_match.duration_ms, 4000);
    assert_eq!(row.effective_contribution.active_ms, 3000);
    let sample = response
        .samples
        .iter()
        .find(|row| row.rule == "user.preview/document@1")
        .expect("sample");
    assert!(
        matches!(&sample.before,wire::RulePreviewOutput::Extraction {extraction} if extraction.document.as_deref()==Some("private title fixture"))
    );
    assert!(
        matches!(&sample.after,wire::RulePreviewOutput::Extraction {extraction} if extraction.document.as_deref()==Some("title fixture"))
    );
}
#[test]
fn privacy_downgrades_include_inactive_slices_and_all_participating_rules() {
    let mut fixture = Fixture::new(4);
    fixture.facts.mask_idle(1000, 2000);
    let body = fixture.body(&Fixture::text("privacy", Fixture::privacy()));
    let response = fixture.run(body).expect("preview");
    assert_eq!(
        response.further_hidden,
        Some(wire::FurtherHidden {
            segments: 4,
            fields: 4,
            duration_ms: 4000
        })
    );
    for row in response
        .rules
        .iter()
        .filter(|row| row.rule.starts_with("user.preview/"))
    {
        assert_eq!(row.condition_match.duration_ms, 4000);
        assert_eq!(row.effective_contribution.active_ms, 3000);
        assert_eq!(row.further_hidden, response.further_hidden);
    }
    assert!(
        !serde_json::to_string(&response.samples)
            .expect("samples")
            .contains("private title fixture")
    );
}
#[test]
fn privacy_samples_remain_redacted_when_restrictions_are_added_or_removed() {
    for original in ["", Fixture::privacy()] {
        let mut fixture = Fixture::new(4);
        fixture.install(&Fixture::text("privacy", original));
        let replacement = if original.is_empty() {
            Fixture::privacy().to_owned()
        } else {
            Fixture::privacy().replace(
                "preset='title_hidden'",
                "app='keep'\nwindow_title='keep'\nweb='full'",
            )
        };
        let body = fixture.body(&Fixture::text("privacy", &replacement));
        let response = fixture.run(body).expect("privacy preview");
        assert!(!response.samples.is_empty());
        let encoded = serde_json::to_string(&response.samples).expect("samples");
        assert!(!encoded.contains("private title fixture"));
        assert!(!encoded.contains("normalized_title"));
    }
}
#[test]
fn extraction_samples_remove_outputs_hidden_by_existing_privacy() {
    let mut fixture = Fixture::new(4);
    fixture.install(&Fixture::text("privacy", Fixture::privacy()));
    let mut text = Fixture::text("extraction", Fixture::extraction());
    text = text.replace("user.preview", "user.extract");
    let mut body = fixture.body(&text);
    body["mutation"]["body"]["key"] = json!("user.extract");
    body["mutation"]["body"]["base_version"] = json!(0);
    let response = fixture.run(body).expect("extraction preview");
    let sample = response
        .samples
        .iter()
        .find(|row| row.rule == "user.extract/document@1")
        .expect("sample");
    assert!(
        matches!(&sample.before,wire::RulePreviewOutput::Extraction {extraction} if *extraction==wire::TitleExtraction::default())
    );
    assert!(
        matches!(&sample.after,wire::RulePreviewOutput::Extraction {extraction} if *extraction==wire::TitleExtraction::default())
    );
}
#[test]
fn preview_rejects_stale_expected_versions_before_scanning_facts() {
    let mut fixture = Fixture::new(4);
    let mut body = fixture.body(&Fixture::text("classification", ""));
    body["expected_versions"] = json!("outdated");
    let error = fixture.run(body).expect_err("stale inputs");
    assert!(matches!(error.kind(), ErrorKind::StaleInputs { .. }));
    fixture
        .facts
        .run(fixture.facts.request_day())
        .expect("reader restored");
}
#[test]
fn preview_keeps_one_fact_snapshot_while_capture_writes_are_serviced_between_turns() {
    let mut fixture = Fixture::new(4);
    let body = fixture.body(&Fixture::text("classification", Fixture::classification()));
    let request = PreviewRequest::decode(wire::Op::RulesPreview, body).expect("request");
    let mut reader = fixture.facts.store.take_query_reader();
    let mut job = PreviewJob::start(&mut reader, request, &fixture.registry).expect("start");
    job.advance().expect("first bounded turn");
    fixture.facts.append_fact();
    let response = loop {
        if let Some(QueryResponse::Preview(response)) = job.advance().expect("turn") {
            break response;
        }
    };
    assert_eq!(
        response.totals.expect("totals").before.active_ms,
        Some(4000)
    );
    fixture
        .facts
        .store
        .return_query_reader(job.finish().expect("reader"));
}
#[test]
fn paging_bounds_encoded_responses_and_rejects_cursors_after_a_new_revision() {
    let mut fixture = Fixture::new(900);
    let mut body = fixture.body(&Fixture::text("classification", Fixture::classification()));
    body["sample_limit"] = json!(2);
    let first = fixture.run(body.clone()).expect("first page");
    assert!(serde_json::to_vec(&first).expect("page").len() <= wire::QUERY_PAGE_BYTES);
    let cursor = first.next_cursor.clone().expect("ranking continuation");
    body["cursor"] = serde_json::to_value(cursor).expect("cursor");
    let second = fixture.run(body.clone()).expect("next page");
    assert_eq!(
        second.totals.as_ref().expect("totals").before.active_ms,
        None
    );
    assert!(second.further_hidden.is_none());
    let mut samples = first.samples;
    let mut page = second;
    loop {
        assert!(serde_json::to_vec(&page).expect("encoded page").len() <= wire::QUERY_PAGE_BYTES);
        samples.extend(page.samples);
        let Some(next) = page.next_cursor else {
            break;
        };
        body["cursor"] = serde_json::to_value(next).expect("cursor");
        page = fixture.run(body.clone()).expect("all continuations");
    }
    assert_eq!(samples.len(), 2);
    assert!(
        samples
            .iter()
            .all(|sample| sample.start_utc_ms == fixture.facts.start)
    );
    body["cursor"] =
        serde_json::to_value(first.next_cursor.expect("first cursor")).expect("cursor");
    fixture.install(&Fixture::text("classification", ""));
    assert_eq!(
        fixture.run(body).expect_err("new revision").kind(),
        ErrorKind::StaleCursor
    );
}

#[test]
fn privacy_rule_counts_include_only_fields_supplied_by_that_rule() {
    let mut fixture = Fixture::new(4);
    let rules = Fixture::privacy().replacen("preset='title_hidden'", "preset='excluded'", 1);
    let body = fixture.body(&Fixture::text("privacy", &rules));
    let response = fixture.run(body).expect("preview");
    assert_eq!(
        response.further_hidden,
        Some(wire::FurtherHidden {
            segments: 4,
            fields: 8,
            duration_ms: 4000
        })
    );
    let first = response
        .rules
        .iter()
        .find(|row| row.rule == "user.preview/one@1")
        .expect("app source");
    let second = response
        .rules
        .iter()
        .find(|row| row.rule == "user.preview/two@1")
        .expect("title source");
    assert_eq!(first.further_hidden.as_ref().expect("fields").fields, 8);
    assert_eq!(second.further_hidden.as_ref().expect("fields").fields, 4);
    assert_eq!(second.effective_contribution.active_ms, 4000);
}
#[test]
fn shadowed_extraction_matches_do_not_contribute_to_selected_outputs() {
    let mut fixture = Fixture::new(4);
    let rules = format!(
        "{}{}",
        Fixture::extraction(),
        Fixture::extraction().replace("id='document'", "id='shadowed'")
    );
    let body = fixture.body(&Fixture::text("extraction", &rules));
    let response = fixture.run(body).expect("preview");
    let second = response
        .rules
        .iter()
        .find(|row| row.rule == "user.preview/shadowed@1")
        .expect("shadowed");
    assert_eq!(second.condition_match.duration_ms, 4000);
    assert_eq!(second.effective_contribution.active_ms, 0);
}

#[test]
fn seven_logical_days_use_the_same_rankings_and_document_references_as_the_selected_day() {
    let mut fixture = Fixture::new(4);
    let text = Fixture::text("classification", Fixture::classification());
    let mut body = fixture.body(&text);
    let QueryResponse::Range(range) = fixture
        .facts
        .run(QueryRequest::Range(wire::QueryRangeRequest {
            start_date: "2024-12-27".parse().expect("start"),
            end_date: "2025-01-03".parse().expect("end"),
            bucket: wire::RangeBucket::Day,
            dimensions: vec![wire::RangeDimension::Category],
            cursor: None,
        }))
        .expect("seven day digest")
    else {
        panic!("range")
    };
    body["start_date"] = json!("2024-12-27");
    body["expected_versions"] = serde_json::to_value(range.versions).expect("versions");
    let totals = fixture
        .run(body)
        .expect("seven day preview")
        .totals
        .expect("totals");
    let day = fixture.draft_day(&text);
    assert_eq!(
        totals.after.active_ms,
        Some(day.metrics.expect("metrics").active_ms)
    );
    assert_eq!(totals.after.applications, day.applications);
    assert_eq!(totals.after.documents, day.documents);
}

#[test]
fn zero_elapsed_slices_count_matches_and_downgrades_without_active_contribution() {
    let mut fixture = Fixture::new(1);
    fixture.facts.zero_first_duration();
    let body = fixture.body(&Fixture::text("privacy", Fixture::privacy()));
    let response = fixture.run(body).expect("preview");
    let rule = response
        .rules
        .iter()
        .find(|row| row.rule == "user.preview/one@1")
        .expect("rule");
    assert_eq!(
        rule.condition_match,
        wire::RuleConditionMatch {
            segments: 1,
            duration_ms: 0
        }
    );
    assert_eq!(
        rule.effective_contribution,
        wire::RuleEffectiveContribution {
            segments: 0,
            active_ms: 0
        }
    );
    assert_eq!(
        response.further_hidden,
        Some(wire::FurtherHidden {
            segments: 1,
            fields: 1,
            duration_ms: 0
        })
    );
    let totals = response.totals.expect("totals");
    assert_eq!(totals.before.active_ms, Some(0));
    assert!(totals.before.categories.is_empty());
}

#[test]
fn invalid_mutation_targets_preserve_edit_errors_and_restore_the_reader() {
    let mut fixture = Fixture::new(1);
    let mut body = fixture.body(&Fixture::text("classification", ""));
    body["mutation"]["body"]["key"] = json!("invalid key");
    assert_eq!(
        fixture.run(body).expect_err("invalid key").kind(),
        ErrorKind::BadRequest
    );
    let mut body = fixture.body(&Fixture::text("classification", ""));
    body["mutation"]["body"]["key"] = json!("builtin.classification");
    assert_eq!(
        fixture.run(body).expect_err("builtin edit").kind(),
        ErrorKind::RuleSetReadOnly {
            key: "builtin.classification".into()
        }
    );
    fixture
        .facts
        .run(fixture.facts.request_day())
        .expect("reader restored");
}

#[test]
fn privacy_drafts_preserve_complete_query_day_totals() {
    for preset in ["title_hidden", "excluded"] {
        let mut fixture = Fixture::new(4);
        let text = Fixture::text(
            "privacy",
            &Fixture::privacy().replace("title_hidden", preset),
        );
        let before = fixture.day_with_rules(fixture.registry.snapshots().load());
        let body = fixture.body(&text);
        let totals = fixture.run(body).expect("preview").totals.expect("totals");
        let after = fixture.draft_day(&text);
        assert_eq!(
            totals.before.active_ms,
            Some(before.metrics.expect("metrics").active_ms)
        );
        assert_eq!(totals.before.applications, before.applications);
        assert_eq!(totals.before.documents, before.documents);
        assert_eq!(
            totals.after.active_ms,
            Some(after.metrics.expect("metrics").active_ms)
        );
        assert_eq!(totals.after.applications, after.applications);
        assert_eq!(totals.after.documents, after.documents);
        assert_eq!(totals.before.categories, totals.after.categories);
        assert_eq!(totals.before.applications, totals.after.applications);
        assert_eq!(totals.after.documents.len(), 4);
    }
}
#[test]
fn negated_rules_skip_excluded_and_absent_foreground_slices() {
    #[derive(Debug, Clone, Copy)]
    enum ForegroundInput {
        Excluded,
        Absent,
    }
    for kind in ["classification", "privacy", "extraction"] {
        for input in [ForegroundInput::Excluded, ForegroundInput::Absent] {
            let mut fixture = Fixture::new(4);
            match input {
                ForegroundInput::Excluded => fixture.facts.exclude_windows(),
                ForegroundInput::Absent => fixture.facts.clear_foreground(),
            }

            let rules = match kind {
                "classification" => {
                    "[[rule]]\nid='negated'\nlabel='Negated'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='equals',value='never.exe',negate=true}]\n"
                }
                "privacy" => {
                    "[[rule]]\nid='negated'\nlabel='Negated'\npreset='title_hidden'\nwhen=[{field='app.exe_name',op='equals',value='never.exe',negate=true}]\n"
                }
                "extraction" => {
                    "[[rule]]\nid='negated'\nlabel='Negated'\napp=[{field='app.exe_name',op='equals',value='never.exe',negate=true}]\npattern='(?P<document>.*)'\n"
                }
                _ => unreachable!("fixture kinds"),
            };
            let body = fixture.body(&Fixture::text(kind, rules));
            let response = fixture.run(body).expect("preview");
            let rule = response
                .rules
                .iter()
                .find(|row| row.rule == "user.preview/negated@1")
                .expect("draft rule");
            assert_eq!(rule.condition_match.segments, 0, "{kind}, input={input:?}");
            assert!(
                response
                    .samples
                    .iter()
                    .all(|sample| sample.rule != "user.preview/negated@1")
            );
        }
    }
}
#[test]
fn privacy_contribution_excludes_offline_activity_over_retained_idle_windows() {
    let mut fixture = Fixture::new(4);
    fixture.facts.mask_idle(1000, 2000);
    fixture.facts.annotate(
        1,
        wire::AnnotationKind::OfflineActivity,
        1000,
        2000,
        Some(wire::BuiltinCategory::Learning),
    );
    let body = fixture.body(&Fixture::text("privacy", Fixture::privacy()));
    let response = fixture.run(body).expect("preview");
    let rule = response
        .rules
        .iter()
        .find(|row| row.rule == "user.preview/one@1")
        .expect("rule");
    assert_eq!(rule.condition_match.duration_ms, 4000);
    assert_eq!(
        rule.further_hidden.as_ref().expect("hidden").duration_ms,
        4000
    );
    assert_eq!(rule.effective_contribution.active_ms, 3000);
    assert_eq!(rule.effective_contribution.segments, 3);
}
#[test]
fn paging_encodes_only_candidate_document_references_in_bounded_turns() {
    use super::super::documents::ISSUED_DOCUMENT_REFERENCES;
    let mut fixture = Fixture::new(900);
    let body = fixture.body(&Fixture::text("classification", Fixture::classification()));
    let request = PreviewRequest::decode(wire::Op::RulesPreview, body).expect("request");
    let mut reader = fixture.facts.store.take_query_reader();
    let mut job = PreviewJob::start(&mut reader, request, &fixture.registry).expect("job");
    ISSUED_DOCUMENT_REFERENCES.set(0);
    let mut maximum = 0;
    let response = loop {
        let previous = ISSUED_DOCUMENT_REFERENCES.get();
        let result = job.advance().expect("bounded turn");
        maximum = maximum.max(ISSUED_DOCUMENT_REFERENCES.get() - previous);
        if let Some(QueryResponse::Preview(response)) = result {
            break response;
        }
    };
    fixture
        .facts
        .store
        .return_query_reader(job.finish().expect("reader"));
    let totals = response.totals.expect("totals");
    let returned = totals.before.documents.len() + totals.after.documents.len();
    assert!(
        ISSUED_DOCUMENT_REFERENCES.get() <= returned + 1,
        "only candidate references are encoded"
    );
    assert!(
        maximum <= QUERY_BATCH_ROWS,
        "one turn encodes at most one batch"
    );
}
#[test]
fn sample_cutoff_skips_unused_privacy_work_and_output_allocations() {
    let mut fixture = Fixture::new(20);
    let mut body = fixture.body(&Fixture::text("classification", Fixture::classification()));
    body["sample_limit"] = json!(1);
    PREVIEW_WORK.set(PreviewWork::default());
    let response = fixture.run(body).expect("preview");
    let work = PREVIEW_WORK.get();
    assert_eq!(response.samples.len(), 1);
    assert_eq!(work.privacy_evaluations, 1);
    assert_eq!(work.sample_outputs, 2);
}
#[test]
fn extraction_statistics_skip_classification_and_sample_privacy_work() {
    let mut fixture = Fixture::new(4);
    let mut body = fixture.body(&Fixture::text("extraction", Fixture::extraction()));
    body["sample_limit"] = json!(0);
    PREVIEW_WORK.set(PreviewWork::default());
    let response = fixture.run(body).expect("preview");
    let work = PREVIEW_WORK.get();
    assert!(response.samples.is_empty());
    assert_eq!(work.classification_fields, 0);
    assert_eq!(work.privacy_evaluations, 0);
    assert_eq!(work.sample_outputs, 0);
}
#[test]
fn preview_omits_unused_hourly_buckets_from_the_first_day() {
    let mut fixture = Fixture::new(4);
    let body = fixture.body(&Fixture::text("classification", ""));
    let request = PreviewRequest::decode(wire::Op::RulesPreview, body).expect("request");
    let mut reader = fixture.facts.store.take_query_reader();
    let job = PreviewJob::start(&mut reader, request, &fixture.registry).expect("job");
    let hours = job.current.hours.len();
    fixture
        .facts
        .store
        .return_query_reader(job.finish().expect("reader"));
    assert_eq!(hours, 0);
}

#[test]
fn disabling_a_rule_set_removes_its_preview_contribution_without_saving() {
    let mut fixture = Fixture::new(4);
    fixture.install(&Fixture::text("classification", Fixture::classification()));
    let before = fixture.registry.snapshots().load();
    let mut body = fixture.body("");
    body["mutation"] = json!({"op":"rule_sets.set_enabled","body":{"key":"user.preview","base_version":1,"enabled":false}});
    let response = fixture.run(body).expect("disable preview");
    assert!(
        response
            .rules
            .iter()
            .all(|rule| !rule.rule.starts_with("user.preview/"))
    );
    assert_eq!(
        response.totals.expect("totals").after.categories,
        vec![wire::PreviewCategoryRank {
            category: wire::CategoryRef::Unclassified,
            active_ms: 4000
        }]
    );
    assert!(Arc::ptr_eq(&before, &fixture.registry.snapshots().load()));
}
#[test]
fn deleting_a_rule_set_previews_unclassified_time_without_saving() {
    let mut fixture = Fixture::new(4);
    fixture.install(&Fixture::text("classification", Fixture::classification()));
    let before = fixture.registry.snapshots().load();
    let mut body = fixture.body("");
    body["mutation"] =
        json!({"op":"rule_sets.delete","body":{"key":"user.preview","base_version":1}});
    let response = fixture.run(body).expect("delete preview");
    assert!(
        response
            .rules
            .iter()
            .all(|rule| !rule.rule.starts_with("user.preview/"))
    );
    assert_eq!(
        response.totals.expect("totals").after.categories,
        vec![wire::PreviewCategoryRank {
            category: wire::CategoryRef::Unclassified,
            active_ms: 4000
        }]
    );
    assert!(Arc::ptr_eq(&before, &fixture.registry.snapshots().load()));
}
#[test]
fn zero_sample_limit_preserves_statistics_and_totals() {
    let mut fixture = Fixture::new(4);
    let mut body = fixture.body(&Fixture::text("classification", Fixture::classification()));
    let with_samples = fixture.run(body.clone()).expect("sampled preview");
    body["sample_limit"] = json!(0);
    let without_samples = fixture.run(body).expect("statistics preview");
    assert!(without_samples.samples.is_empty());
    assert_eq!(with_samples.rules, without_samples.rules);
    assert_eq!(with_samples.totals, without_samples.totals);
}

#[test]
fn title_hidden_windows_keep_negated_matches_on_retained_application_fields() {
    for kind in ["classification", "privacy"] {
        let mut fixture = Fixture::new(4);
        fixture.facts.hide_titles();
        let rules = match kind {
            "classification" => {
                "[[rule]]\nid='negated'\nlabel='Negated'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='equals',value='never.exe',negate=true}]\n"
            }
            "privacy" => {
                "[[rule]]\nid='negated'\nlabel='Negated'\npreset='excluded'\nwhen=[{field='app.exe_name',op='equals',value='never.exe',negate=true}]\n"
            }
            _ => unreachable!("fixture kinds"),
        };
        let body = fixture.body(&Fixture::text(kind, rules));
        let response = fixture.run(body).expect("title-hidden preview");
        let rule = response
            .rules
            .iter()
            .find(|rule| rule.rule == "user.preview/negated@1")
            .expect("rule");
        assert_eq!(rule.condition_match.duration_ms, 4000);
        assert_eq!(rule.effective_contribution.active_ms, 4000);
    }
}
#[test]
fn active_override_keeps_privacy_contribution_on_captured_windows() {
    let mut fixture = Fixture::new(4);
    fixture.facts.mask_idle(1000, 2000);
    fixture
        .facts
        .annotate(1, wire::AnnotationKind::ActiveOverride, 1000, 2000, None);
    let body = fixture.body(&Fixture::text("privacy", Fixture::privacy()));
    let response = fixture.run(body).expect("preview");
    let rule = response
        .rules
        .iter()
        .find(|rule| rule.rule == "user.preview/one@1")
        .expect("rule");
    assert_eq!(rule.effective_contribution.active_ms, 4000);
}
