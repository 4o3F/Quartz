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
    receipts: crate::rules::receipts::ReceiptOwner,
    receipt_clock: std::sync::Arc<std::sync::Mutex<std::time::Instant>>,
}
impl Fixture {
    fn new(count: i64) -> Self {
        let mut facts = QueryFixture::new(count);
        let registry = RegistryShell::open(&mut facts.store, UtcInstant::try_from(0).expect("UTC"))
            .expect("registry");
        let receipt_clock = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
        let clock = receipt_clock.clone();
        let receipts = crate::rules::receipts::ReceiptOwner::with_clock(
            wire::Role::Ui,
            std::sync::Arc::new(move || *clock.lock().expect("test clock")),
        );
        Self {
            facts,
            registry,
            receipts,
            receipt_clock,
        }
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
        mut request: PreviewRequest,
    ) -> Result<wire::RulesPreviewResponse, RunQueryError> {
        if request.receipts.is_none() {
            request.receipts = Some(self.receipts.context());
        }
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
        let grant = job.take_receipt_grant();
        self.facts.store.return_query_reader(job.finish()?);
        if result.is_ok()
            && let Some(grant) = grant
        {
            grant.delivered().expect("deliver receipt");
        }
        result
    }
    fn install(&mut self, text: &str) {
        self.install_at("user.preview", text);
    }
    fn install_at(&mut self, key: &str, text: &str) {
        // Persist a fixture revision directly, including privacy revisions preceding E2.
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
fn cancelled_or_expired_previews_return_no_partial_result_and_restore_the_reader() {
    let mut fixture = Fixture::new(4);
    let body = fixture.body(&Fixture::text("classification", Fixture::classification()));
    let request = PreviewRequest::decode(wire::Op::RulesPreview, body.clone()).expect("request");
    request.control.cancel();
    assert_eq!(
        fixture.run_request(request).expect_err("cancel").kind(),
        ErrorKind::Cancelled
    );
    let mut request = PreviewRequest::decode(wire::Op::RulesPreview, body).expect("request");
    request.control = QueryControl::expired();
    assert_eq!(
        fixture.run_request(request).expect_err("deadline").kind(),
        ErrorKind::DeadlineExceeded
    );
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
    let control = request.control.clone();
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
    let body = fixture.body(&Fixture::text("classification", Fixture::classification()));
    let request = PreviewRequest::decode(wire::Op::RulesPreview, body).expect("request");
    let control_after_start = request.control.clone();
    let mut reader = fixture.facts.store.take_query_reader();
    let mut job = PreviewJob::start(&mut reader, request, &fixture.registry).expect("start");
    control_after_start.cancel();
    assert_eq!(
        job.advance().expect_err("running cancellation").kind(),
        ErrorKind::Cancelled
    );
    fixture
        .facts
        .store
        .return_query_reader(job.finish().expect("reader"));
    control.check().expect("first job completed independently");
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
fn cancellation_and_deadline_discard_statistics_already_computed() {
    let mut fixture = Fixture::new(600);
    for stop in [ErrorKind::Cancelled, ErrorKind::DeadlineExceeded] {
        let body = fixture.body(&Fixture::text("classification", Fixture::classification()));
        let request = PreviewRequest::decode(wire::Op::RulesPreview, body).expect("request");
        let mut reader = fixture.facts.store.take_query_reader();
        let mut job = PreviewJob::start(&mut reader, request, &fixture.registry).expect("start");
        while !matches!(job.stage, Stage::Analyze(_)) {
            assert!(job.advance().expect("bounded turn").is_none());
        }
        assert!(job.advance().expect("compute first batch").is_none());
        assert!(
            job.statistics
                .values()
                .any(|row| row.condition_match.segments > 0)
        );
        match stop {
            ErrorKind::Cancelled => job.control.cancel(),
            ErrorKind::DeadlineExceeded => job.control = QueryControl::expired(),
            _ => unreachable!("stop reasons"),
        }
        assert_eq!(
            job.advance()
                .expect_err("stop without partial response")
                .kind(),
            stop
        );
        fixture
            .facts
            .store
            .return_query_reader(job.finish().expect("reader restored"));
    }
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
fn reordering_rule_sets_changes_the_preview_winner_without_saving() {
    let mut fixture = Fixture::new(4);
    fixture.install(&Fixture::text("classification", Fixture::classification()));
    fixture.install_at(
        "user.second",
        &Fixture::text("classification", Fixture::classification())
            .replace("user.preview", "user.second")
            .replace("builtin:learning", "builtin:development"),
    );
    let before = fixture.registry.snapshots().load();
    let mut body = fixture.body("");
    body["mutation"] = json!({"op":"rule_sets.reorder","body":{"siblings":[{"key":"user.second","base_version":1},{"key":"user.preview","base_version":1}]}});
    let response = fixture.run(body).expect("reorder preview");
    let winner = response
        .rules
        .iter()
        .find(|rule| rule.rule == "user.second/first@2")
        .expect("draft winner");
    assert_eq!(winner.effective_contribution.active_ms, 4000);
    assert!(Arc::ptr_eq(&before, &fixture.registry.snapshots().load()));
}
#[test]
fn reverting_a_rule_set_previews_the_historical_category_without_saving() {
    let mut fixture = Fixture::new(4);
    let text = Fixture::text("classification", Fixture::classification());
    fixture.install(&text);
    fixture.install(&text.replace("builtin:learning", "builtin:development"));
    let before = fixture.registry.snapshots().load();
    let mut body = fixture.body("");
    body["mutation"] =
        json!({"op":"rule_sets.revert","body":{"key":"user.preview","base_version":2,"version":1}});
    let response = fixture.run(body).expect("revert preview");
    let category = &response.totals.expect("totals").after.categories[0].category;
    assert!(
        matches!(category,wire::CategoryRef::Classified {category} if category.builtin_key==Some(wire::BuiltinCategory::Learning))
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

impl Fixture {
    fn privacy_preview(&mut self) -> Value {
        let text = Self::text("privacy", Self::privacy());
        self.body(&text)
    }
    fn confirm(
        &mut self,
        preview: &Value,
        receipt: Option<&str>,
    ) -> Result<wire::RuleSetsWriteResponse, ProtocolError> {
        self.confirm_on(preview, receipt, self.receipts.context())
    }
    fn confirm_on(
        &mut self,
        preview: &Value,
        receipt: Option<&str>,
        context: crate::rules::receipts::ReceiptContext,
    ) -> Result<wire::RuleSetsWriteResponse, ProtocolError> {
        let op: wire::Op =
            serde_json::from_value(preview["mutation"]["op"].clone()).expect("operation");
        let mut body = preview["mutation"]["body"].clone();
        if let Some(receipt) = receipt {
            body["preview_receipt"] = json!(receipt);
        }
        let request = crate::persistence::dispatch::WriteRequest::decode(op, body)
            .expect("write request")
            .bind_receipts(context);
        match request.execute(
            &mut self.facts.store,
            UtcInstant::try_from(0).expect("UTC"),
            None,
            &mut self.registry,
        )? {
            crate::persistence::dispatch::WriteResponse::Rules(response) => Ok(response),
            _ => panic!("rule write response"),
        }
    }
    fn mismatch(reason: wire::PreviewMismatchReason) -> ErrorKind {
        ErrorKind::PreviewMismatch { reason }
    }
}
#[test]
fn completed_privacy_preview_authorizes_exact_save() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("completed statistics");
    assert!(preview.expires_utc_ms.expect("expiry") > jiff::Timestamp::now().as_millisecond());
    assert_eq!(preview.further_hidden.expect("hidden").segments, 4);
    let saved = fixture
        .confirm(&body, preview.preview_receipt.as_deref())
        .expect("authorized save");
    assert_eq!(saved.sets[0].version, 1);
    assert_eq!(
        fixture.facts.store.rule_sets().load().expect("stored").sets
            [&"user.preview".parse().expect("key")]
            .current
            .version,
        1
    );
}
#[test]
fn text_import_requires_its_own_preview_and_returns_edit_response() {
    let mut fixture = Fixture::new(4);
    let mut body = fixture.privacy_preview();
    body["mutation"]["op"] = json!("rules_text.import");
    let request =
        PreviewRequest::decode(wire::Op::RulesTextPreview, body.clone()).expect("text preview");
    let preview = fixture
        .run_request(request)
        .expect("completed import preview");
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect("import saved")
            .sets[0]
            .version,
        1
    );
}
#[test]
fn changed_raw_text_bytes_require_another_preview() {
    let mut fixture = Fixture::new(4);
    let mut body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    let text = body["mutation"]["body"]["text"]
        .as_str()
        .expect("text")
        .to_owned();
    body["mutation"]["body"]["text"] = json!(format!("{text}\n# same parsed contents\n"));
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("raw bytes changed")
            .kind,
        Fixture::mismatch(wire::PreviewMismatchReason::Mutation)
    );
    let refreshed = fixture.run(body.clone()).expect("re-preview");
    fixture
        .confirm(&body, refreshed.preview_receipt.as_deref())
        .expect("new bytes authorized");
}
#[test]
fn changed_operation_cannot_reuse_editor_preview() {
    let mut fixture = Fixture::new(4);
    let mut body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    body["mutation"]["op"] = json!("rules_text.import");
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("different operation")
            .kind,
        Fixture::mismatch(wire::PreviewMismatchReason::Mutation)
    );
}
#[test]
fn changed_target_cannot_reuse_preview() {
    let mut fixture = Fixture::new(4);
    let mut body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    body["mutation"]["body"]["key"] = json!("user.other");
    let text = body["mutation"]["body"]["text"]
        .as_str()
        .expect("text")
        .replace("user.preview", "user.other");
    body["mutation"]["body"]["text"] = json!(text);
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("different target")
            .kind,
        Fixture::mismatch(wire::PreviewMismatchReason::Mutation)
    );
}
#[test]
fn missing_privacy_receipt_requires_preview() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    assert_eq!(
        fixture
            .confirm(&body, None)
            .expect_err("missing proof")
            .kind,
        ErrorKind::PreviewRequired
    );
}
#[test]
fn receipt_expires_on_monotonic_clock() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    *fixture.receipt_clock.lock().expect("clock") += std::time::Duration::from_secs(5 * 60);
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("expiry boundary")
            .kind,
        ErrorKind::PreviewExpired
    );
}
#[test]
fn ninth_preview_evicts_only_oldest_receipt() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let first = fixture.run(body.clone()).expect("first");
    let second = fixture.run(body.clone()).expect("second");
    for _ in 2..9 {
        fixture.run(body.clone()).expect("additional preview");
    }
    assert_eq!(
        fixture
            .confirm(&body, first.preview_receipt.as_deref())
            .expect_err("oldest evicted")
            .kind,
        ErrorKind::PreviewExpired
    );
    fixture
        .confirm(&body, second.preview_receipt.as_deref())
        .expect("second retained");
}
#[test]
fn another_connection_cannot_use_receipt() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    let other = crate::rules::receipts::ReceiptOwner::new(wire::Role::Ui);
    assert_eq!(
        fixture
            .confirm_on(&body, preview.preview_receipt.as_deref(), other.context())
            .expect_err("connection mismatch")
            .kind,
        ErrorKind::PreviewExpired
    );
}
#[test]
fn disconnected_connection_drops_receipts_even_with_pending_context() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    let old_context = fixture.receipts.context();
    fixture.receipts = crate::rules::receipts::ReceiptOwner::new(wire::Role::Ui);
    assert_eq!(
        fixture
            .confirm_on(&body, preview.preview_receipt.as_deref(), old_context)
            .expect_err("disconnected")
            .kind,
        ErrorKind::PreviewExpired
    );
}
#[test]
fn successful_save_consumes_receipt_and_lost_reply_retry_cannot_append_revision() {
    let mut fixture = Fixture::new(4);
    let mut body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    fixture
        .confirm(&body, preview.preview_receipt.as_deref())
        .expect("committed response discarded");
    assert!(matches!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("lost response retry")
            .kind,
        ErrorKind::RuleSetStale {
            current_version: wire::RuleSetVersion(1),
            ..
        }
    ));
    body["mutation"]["body"]["base_version"] = json!(1);
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("consumed proof")
            .kind,
        ErrorKind::PreviewExpired
    );
    assert_eq!(
        fixture.registry.snapshots().load().state().sets[&"user.preview".parse().expect("key")]
            .current
            .version,
        1
    );
}
#[test]
fn changed_fact_versions_require_repreview() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    fixture.facts.append_fact();
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("facts changed")
            .kind,
        Fixture::mismatch(wire::PreviewMismatchReason::Inputs)
    );
    let body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("fresh preview");
    fixture
        .confirm(&body, preview.preview_receipt.as_deref())
        .expect("fresh facts accepted");
}
#[test]
fn changed_annotation_versions_invalidate_receipt() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    fixture
        .facts
        .annotate(97, wire::AnnotationKind::Note, 0, 1000, None);
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("annotation changed")
            .kind,
        Fixture::mismatch(wire::PreviewMismatchReason::Inputs)
    );
}
#[test]
fn changed_metadata_invalidates_receipt() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    fixture.facts.bump_metadata();
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("metadata changed")
            .kind,
        Fixture::mismatch(wire::PreviewMismatchReason::Inputs)
    );
}
#[test]
fn changed_logical_date_boundaries_invalidate_receipt() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    fixture
        .facts
        .store
        .test_day_offset_write(60)
        .expect("changed date boundaries");
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("range changed")
            .kind,
        Fixture::mismatch(wire::PreviewMismatchReason::Range)
    );
}
#[test]
fn changed_rule_inputs_invalidate_receipt_even_when_effective_contents_match() {
    for kind in ["classification", "extraction", "privacy"] {
        let mut fixture = Fixture::new(4);
        let body = fixture.privacy_preview();
        let preview = fixture.run(body.clone()).expect("preview");
        let text = format!(
            "format='quartz-rules/1'\n[set]\nkey='user.other'\nkind='{kind}'\nname='Other'\n"
        );
        fixture.install_at("user.other", &text);
        assert_eq!(
            fixture
                .confirm(&body, preview.preview_receipt.as_deref())
                .expect_err("rule input revision changed")
                .kind,
            Fixture::mismatch(wire::PreviewMismatchReason::Inputs),
            "{kind}"
        );
    }
}
#[test]
fn changed_parent_chain_invalidates_receipt() {
    let mut fixture = Fixture::new(4);
    let parent = "format='quartz-rules/1'\n[set]\nkey='user.parent'\nkind='privacy'\nname='Parent'\nparent='builtin.privacy'\n";
    fixture.install_at("user.parent", parent);
    let text = format!("{}parent='user.parent'\n", Fixture::text("privacy", ""));
    let body = fixture.body(&text);
    let preview = fixture.run(body.clone()).expect("child preview");
    fixture.install_at("user.parent", parent);
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("parent revision changed")
            .kind,
        Fixture::mismatch(wire::PreviewMismatchReason::Inputs)
    );
}
#[test]
fn nonprivacy_writes_need_no_receipt_and_previews_issue_none() {
    let mut fixture = Fixture::new(4);
    let text = Fixture::text("classification", Fixture::classification());
    let body = fixture.body(&text);
    let preview = fixture.run(body.clone()).expect("classification preview");
    assert!(preview.preview_receipt.is_none());
    assert!(preview.expires_utc_ms.is_none());
    fixture.confirm(&body, None).expect("classification saved");
    let mut import = fixture.body(&text);
    import["mutation"]["op"] = json!("rules_text.import");
    assert_eq!(
        fixture
            .confirm(&import, None)
            .expect("classification import needs no receipt")
            .sets[0]
            .version,
        2
    );
}

#[test]
fn every_privacy_operation_accepts_only_its_exact_preview() {
    for op in [
        "rule_sets.edit",
        "rule_sets.set_enabled",
        "rule_sets.reorder",
        "rule_sets.delete",
        "rule_sets.revert",
    ] {
        let mut fixture = Fixture::new(4);
        let text = Fixture::text("privacy", Fixture::privacy());
        fixture.install(&text);
        let mut body = fixture.body(&text);
        body["mutation"] = match op {
            "rule_sets.edit" => {
                json!({"op":op,"body":{"key":"user.preview","base_version":1,"text":text}})
            }
            "rule_sets.set_enabled" => {
                json!({"op":op,"body":{"key":"user.preview","base_version":1,"enabled":false}})
            }
            "rule_sets.reorder" => {
                json!({"op":op,"body":{"siblings":[{"key":"user.preview","base_version":1}]}})
            }
            "rule_sets.delete" => json!({"op":op,"body":{"key":"user.preview","base_version":1}}),
            "rule_sets.revert" => {
                json!({"op":op,"body":{"key":"user.preview","base_version":1,"version":1}})
            }
            _ => unreachable!("operations"),
        };
        let preview = fixture.run(body.clone()).expect("operation preview");
        let response = fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect("operation authorized");
        assert_eq!(response.sets[0].version, 2, "{op}");
        assert_eq!(response.sets[0].deleted, op == "rule_sets.delete", "{op}");
    }
}
#[test]
fn changed_enablement_body_cannot_reuse_receipt() {
    let mut fixture = Fixture::new(4);
    let text = Fixture::text("privacy", Fixture::privacy());
    fixture.install(&text);
    let mut body = fixture.body(&text);
    body["mutation"] = json!({"op":"rule_sets.set_enabled","body":{"key":"user.preview","base_version":1,"enabled":false}});
    let preview = fixture.run(body.clone()).expect("disable preview");
    body["mutation"]["body"]["enabled"] = json!(true);
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("body changed")
            .kind,
        Fixture::mismatch(wire::PreviewMismatchReason::Mutation)
    );
    body["mutation"]["body"]["enabled"] = json!(false);
    fixture
        .confirm(&body, preview.preview_receipt.as_deref())
        .expect("mismatch did not consume receipt");
}
#[test]
fn changed_base_version_is_checked_before_receipt() {
    let mut fixture = Fixture::new(4);
    let mut body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    body["mutation"]["body"]["base_version"] = json!(1);
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("stale base")
            .kind,
        ErrorKind::RuleSetStale {
            key: "user.preview".into(),
            current_version: wire::RuleSetVersion(0)
        }
    );
}
#[test]
fn cli_preview_receipt_cannot_authorize_save() {
    let mut fixture = Fixture::new(4);
    let owner = crate::rules::receipts::ReceiptOwner::new(wire::Role::Cli);
    let mut body = fixture.privacy_preview();
    body["mutation"]["op"] = json!("rules_text.import");
    let mut request =
        PreviewRequest::decode(wire::Op::RulesTextPreview, body.clone()).expect("CLI preview");
    request.receipts = Some(owner.context());
    let preview = fixture.run_request(request).expect("CLI can preview");
    assert_eq!(
        fixture
            .confirm_on(&body, preview.preview_receipt.as_deref(), owner.context())
            .expect_err("CLI cannot save")
            .kind,
        ErrorKind::Forbidden
    );
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("UI connection cannot borrow CLI receipt")
            .kind,
        ErrorKind::PreviewExpired
    );
}

#[test]
fn failed_privacy_transaction_keeps_receipt_and_current_snapshot() {
    use diesel::{Connection, connection::SimpleConnection};
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    let before = fixture.registry.snapshots().load();
    let path = fixture.facts.directory.path().join("quartz.db");
    let mut connection =
        diesel::sqlite::SqliteConnection::establish(path.to_str().expect("isolated path"))
            .expect("isolated database");
    connection.batch_execute("CREATE TRIGGER reject_privacy BEFORE INSERT ON rule_set_versions BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;").expect("reject commit");
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("transaction rolled back")
            .kind,
        ErrorKind::Internal
    );
    assert!(Arc::ptr_eq(&before, &fixture.registry.snapshots().load()));
    connection
        .batch_execute("DROP TRIGGER reject_privacy")
        .expect("restore writes");
    fixture
        .confirm(&body, preview.preview_receipt.as_deref())
        .expect("unconsumed receipt succeeds");
}
#[test]
fn facts_changed_during_preview_are_rechecked_when_saving() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let mut request =
        PreviewRequest::decode(wire::Op::RulesPreview, body.clone()).expect("request");
    request.receipts = Some(fixture.receipts.context());
    let mut reader = fixture.facts.store.take_query_reader();
    let mut job = PreviewJob::start(&mut reader, request, &fixture.registry).expect("snapshot");
    fixture.facts.append_fact();
    let response = loop {
        if let Some(QueryResponse::Preview(response)) = job.advance().expect("statistics") {
            break response;
        }
    };
    let grant = job.take_receipt_grant().expect("receipt grant");
    fixture
        .facts
        .store
        .return_query_reader(job.finish().expect("reader"));
    grant.delivered().expect("deliver receipt");
    assert_eq!(
        fixture
            .confirm(&body, response.preview_receipt.as_deref())
            .expect_err("commit rechecks newer facts")
            .kind,
        Fixture::mismatch(wire::PreviewMismatchReason::Inputs)
    );
}
#[test]
fn abandoning_completed_preview_revokes_undelivered_receipt() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let mut request =
        PreviewRequest::decode(wire::Op::RulesPreview, body.clone()).expect("request");
    request.receipts = Some(fixture.receipts.context());
    let mut reader = fixture.facts.store.take_query_reader();
    let mut job = PreviewJob::start(&mut reader, request, &fixture.registry).expect("snapshot");
    let response = loop {
        if let Some(QueryResponse::Preview(response)) = job.advance().expect("statistics") {
            break response;
        }
    };
    fixture
        .facts
        .store
        .return_query_reader(job.finish().expect("reader"));
    assert_eq!(
        fixture
            .confirm(&body, response.preview_receipt.as_deref())
            .expect_err("undelivered result")
            .kind,
        ErrorKind::PreviewExpired
    );
}
#[test]
fn registry_apply_and_import_paths_cannot_bypass_privacy_confirmation() {
    for operation in ["apply", "import"] {
        let mut fixture = Fixture::new(4);
        let text = Fixture::text("privacy", Fixture::privacy());
        let command = if operation == "import" {
            RegistryCommand::Import {
                key: "user.preview".parse().expect("key"),
                file: "fixture.toml".into(),
                text,
                base_version: 0,
            }
        } else {
            RegistryCommand::Apply {
                key: "user.preview".parse().expect("key"),
                file: "fixture.toml".into(),
                text,
                base_version: 0,
            }
        };
        assert!(matches!(
            fixture.registry.run(
                &mut fixture.facts.store,
                UtcInstant::try_from(0).expect("UTC"),
                command
            ),
            Err(crate::rules::registry::RunRegistryCommandError::PreviewRequired)
        ));
    }
}

#[test]
fn only_first_privacy_page_issues_receipt_within_page_budget() {
    let mut fixture = Fixture::new(900);
    let mut body = fixture.privacy_preview();
    let first = fixture.run(body.clone()).expect("first page");
    assert!(first.preview_receipt.is_some());
    assert!(first.expires_utc_ms.is_some());
    assert!(serde_json::to_vec(&first).expect("page").len() <= wire::QUERY_PAGE_BYTES);
    body["cursor"] =
        serde_json::to_value(first.next_cursor.expect("continuation")).expect("cursor");
    let second = fixture.run(body.clone()).expect("continuation");
    assert!(second.preview_receipt.is_none());
    assert!(second.expires_utc_ms.is_none());
    assert!(serde_json::to_vec(&second).expect("page").len() <= wire::QUERY_PAGE_BYTES);
    fixture
        .confirm(&body, first.preview_receipt.as_deref())
        .expect("all statistics were completed on first page");
}
#[test]
fn restarting_core_does_not_retain_receipts() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    fixture.registry = RegistryShell::open(
        &mut fixture.facts.store,
        UtcInstant::try_from(0).expect("UTC"),
    )
    .expect("restarted registry");
    fixture.receipts = crate::rules::receipts::ReceiptOwner::new(wire::Role::Ui);
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("volatile receipt")
            .kind,
        ErrorKind::PreviewExpired
    );
}

#[test]
fn changed_effective_rules_of_every_kind_invalidate_receipt() {
    for kind in ["classification", "extraction", "privacy"] {
        let mut fixture = Fixture::new(4);
        let body = fixture.privacy_preview();
        let preview = fixture.run(body.clone()).expect("preview");
        let rules = match kind {
            "classification" => Fixture::classification(),
            "extraction" => Fixture::extraction(),
            "privacy" => Fixture::privacy(),
            _ => unreachable!("rule families"),
        };
        let text = Fixture::text(kind, rules).replace("user.preview", "user.other");
        fixture.install_at("user.other", &text);
        assert_eq!(
            fixture
                .confirm(&body, preview.preview_receipt.as_deref())
                .expect_err("effective rules changed")
                .kind,
            Fixture::mismatch(wire::PreviewMismatchReason::Inputs),
            "{kind}"
        );
    }
}
#[test]
fn receipt_expiry_is_rechecked_inside_write_transaction() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut fixture = Fixture::new(4);
    let origin = std::time::Instant::now();
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    fixture.receipts = crate::rules::receipts::ReceiptOwner::with_clock(
        wire::Role::Ui,
        std::sync::Arc::new(move || {
            const TRANSACTION_CHECK_CALL: usize = 3;
            if calls.fetch_add(1, Ordering::SeqCst) >= TRANSACTION_CHECK_CALL {
                origin + std::time::Duration::from_secs(5 * 60)
            } else {
                origin
            }
        }),
    );
    let body = fixture.privacy_preview();
    let preview = fixture.run(body.clone()).expect("preview");
    let before = fixture.registry.snapshots().load();
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("expired after initial check")
            .kind,
        ErrorKind::PreviewExpired
    );
    assert!(Arc::ptr_eq(&before, &fixture.registry.snapshots().load()));
}

#[test]
fn restoring_deleted_privacy_key_requires_matching_preview() {
    let mut fixture = Fixture::new(4);
    let text = Fixture::text("privacy", Fixture::privacy());
    fixture.install(&text);
    let mut deletion = fixture.body(&text);
    deletion["mutation"] =
        json!({"op":"rule_sets.delete","body":{"key":"user.preview","base_version":1}});
    let preview = fixture.run(deletion.clone()).expect("delete preview");
    fixture
        .confirm(&deletion, preview.preview_receipt.as_deref())
        .expect("delete");
    let mut restore = fixture.body(&text);
    restore["mutation"]["body"]["base_version"] = json!(0);
    assert_eq!(
        fixture
            .confirm(&restore, None)
            .expect_err("restore needs preview")
            .kind,
        ErrorKind::PreviewRequired
    );
    let preview = fixture.run(restore.clone()).expect("restore preview");
    let saved = fixture
        .confirm(&restore, preview.preview_receipt.as_deref())
        .expect("restore authorized");
    assert_eq!(saved.sets[0].version, 3);
    assert!(!saved.sets[0].deleted);
}
#[test]
fn permuted_privacy_siblings_cannot_reuse_reorder_preview() {
    let mut fixture = Fixture::new(4);
    let text = Fixture::text("privacy", "");
    fixture.install(&text);
    fixture.install_at("user.other", &text.replace("user.preview", "user.other"));
    let mut body = fixture.body(&text);
    body["mutation"] = json!({"op":"rule_sets.reorder","body":{"siblings":[{"key":"user.preview","base_version":1},{"key":"user.other","base_version":1}]}});
    let preview = fixture.run(body.clone()).expect("reorder preview");
    body["mutation"]["body"]["siblings"]
        .as_array_mut()
        .expect("siblings")
        .reverse();
    assert_eq!(
        fixture
            .confirm(&body, preview.preview_receipt.as_deref())
            .expect_err("order changed")
            .kind,
        Fixture::mismatch(wire::PreviewMismatchReason::Mutation)
    );
    body["mutation"]["body"]["siblings"]
        .as_array_mut()
        .expect("siblings")
        .reverse();
    let saved = fixture
        .confirm(&body, preview.preview_receipt.as_deref())
        .expect("original order authorized");
    assert_eq!(saved.sets.len(), 2);
    assert!(saved.sets.iter().all(|set| set.version == 2));
}
#[test]
fn cancelled_ninth_preview_keeps_first_receipt_valid() {
    let mut fixture = Fixture::new(4);
    let body = fixture.privacy_preview();
    let first = fixture.run(body.clone()).expect("first preview");
    for _ in 1..8 {
        fixture.run(body.clone()).expect("delivered preview");
    }
    let mut request =
        PreviewRequest::decode(wire::Op::RulesPreview, body.clone()).expect("request");
    request.receipts = Some(fixture.receipts.context());
    let mut reader = fixture.facts.store.take_query_reader();
    let mut job = PreviewJob::start(&mut reader, request, &fixture.registry).expect("job");
    loop {
        if let Some(QueryResponse::Preview(_)) = job.advance().expect("statistics") {
            break;
        }
    }
    job.control.cancel();
    assert_eq!(
        job.control.check().expect_err("cancelled").kind(),
        ErrorKind::Cancelled
    );
    fixture
        .facts
        .store
        .return_query_reader(job.finish().expect("reader"));
    fixture
        .confirm(&body, first.preview_receipt.as_deref())
        .expect("cancelled preview did not evict first receipt");
}
