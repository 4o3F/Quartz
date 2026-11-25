use super::*;
use crate::{
    query::{QueryJob, QueryRequest},
    rules::registry::RegistryShell,
    store::query::test_support::QueryFixture,
};
use quartz_domain::{capture::UtcInstant, rules::registry::RegistryCommand};
use serde_json::{Value, json};

struct Fixture {
    facts: QueryFixture,
    registry: RegistryShell,
    snapshot: Option<Arc<RuleSnapshot>>,
}
impl Fixture {
    fn new(count: i64) -> Self {
        let mut facts = QueryFixture::new(count);
        let registry = RegistryShell::open(&mut facts.store, UtcInstant::try_from(0).expect("UTC"))
            .expect("registry");
        Self {
            facts,
            registry,
            snapshot: None,
        }
    }
    fn rules(&self) -> Arc<RuleSnapshot> {
        self.snapshot
            .clone()
            .unwrap_or_else(|| self.registry.snapshots().load())
    }
    fn startup(&mut self, change: impl FnOnce(&mut domain::registry::RegistryState)) {
        let mut state = self.registry.snapshots().load().state().clone();
        change(&mut state);
        for entry in state.sets.values_mut() {
            let revision = entry
                .versions
                .get_mut(&entry.current.version)
                .expect("current revision");
            revision.content = entry.current.set.content.clone();
            revision.header = Some(entry.current.set.header.clone());
        }
        self.snapshot = Some(
            domain::registry::RuleRegistry::from_state(state)
                .expect("startup snapshot")
                .snapshots()
                .load(),
        );
    }
    fn range(&self) -> Value {
        json!({"start_utc_ms":self.facts.start,"end_utc_ms":self.facts.start + self.facts.count * 1000})
    }
    fn install(&mut self, key: &str, kind: &str, parent: Option<&str>, contents: &str) {
        let parent = parent
            .map(|parent| format!("parent='{parent}'\n"))
            .unwrap_or_default();
        let mut labelled = String::new();
        for section in contents.split("[[") {
            if section.is_empty() {
                continue;
            }
            labelled.push_str("[[");
            labelled.push_str(section);
            if (section.starts_with("rule]]") || section.starts_with("override]]"))
                && !section.lines().any(|line| line.starts_with("label="))
            {
                labelled.push_str("label='Fixture rule'\n");
            }
        }
        let text = format!(
            "format='quartz-rules/1'\n[set]\nkey='{key}'\nkind='{kind}'\nname='Explain'\n{parent}{labelled}"
        );
        let version = self
            .registry
            .snapshots()
            .load()
            .state()
            .sets
            .get(&key.parse().expect("key"))
            .map_or(0, |entry| entry.current.version);
        self.apply(RegistryCommand::Apply {
            key: key.parse().expect("key"),
            file: "fixture.toml".into(),
            text,
            base_version: version,
        });
    }
    fn apply(&mut self, command: RegistryCommand) {
        let parts = self.registry.core().decide(command).into_parts();
        parts.reply.expect("decision");
        self.facts
            .store
            .rule_transaction(|transaction| transaction.execute(&parts.writes, 0))
            .expect("fixture transaction");
        self.registry =
            RegistryShell::open(&mut self.facts.store, UtcInstant::try_from(0).expect("UTC"))
                .expect("reopen");
    }
    fn explain(&mut self, body: Value) -> Result<wire::RulesExplainResponse, RunQueryError> {
        self.run(ExplainRequest::decode(body)?)
    }
    fn all(&mut self, mut body: Value) -> wire::RulesExplainResponse {
        let mut complete: Option<wire::RulesExplainResponse> = None;
        loop {
            let response = self.explain(body.clone()).expect("explanation page");
            let cursor = response.next_cursor.clone();
            if let Some(complete) = &mut complete {
                for mut fragment in response.fragments {
                    if let Some(previous) = complete.fragments.last_mut().filter(|previous| {
                        previous.start_utc_ms == fragment.start_utc_ms
                            && previous.end_utc_ms == fragment.end_utc_ms
                            && previous.clock_segment_id == fragment.clock_segment_id
                    }) {
                        previous.nodes.append(&mut fragment.nodes);
                    } else {
                        complete.fragments.push(fragment);
                    }
                }
            } else {
                complete = Some(response);
            }
            let Some(cursor) = cursor else {
                break;
            };
            body["cursor"] = serde_json::to_value(cursor).expect("cursor");
        }
        let mut complete = complete.expect("first page");
        complete.next_cursor = None;
        complete
    }
    fn run(
        &mut self,
        request: ExplainRequest,
    ) -> Result<wire::RulesExplainResponse, RunQueryError> {
        let mut reader = self.facts.store.take_query_reader();
        let mut job = match ExplainJob::start(&mut reader, request, self.rules()) {
            Ok(job) => job,
            Err(error) => {
                self.facts
                    .store
                    .return_query_reader(reader.expect("returned reader"));
                return Err(error);
            }
        };
        let result = loop {
            match job.advance() {
                Ok(Some(QueryResponse::Explain(response))) => break Ok(response),
                Ok(None) => {}
                Err(error) => break Err(error),
                _ => panic!("explanation response"),
            }
        };
        self.facts.store.return_query_reader(job.finish()?);
        result
    }
    fn query(&mut self, request: QueryRequest) -> QueryResponse {
        let mut reader = self.facts.store.take_query_reader();
        let mut job = QueryJob::start(&mut reader, request, self.rules()).expect("query");
        let response = loop {
            if let Some(response) = job.advance().expect("query turn") {
                break response;
            }
        };
        self.facts
            .store
            .return_query_reader(job.finish().expect("finish query"));
        response
    }
    fn details(&mut self) -> wire::QuerySegmentDetailResponse {
        let request = QueryRequest::SegmentDetail(wire::QuerySegmentDetailRequest {
            start_utc_ms: self.facts.start,
            end_utc_ms: self.facts.start + self.facts.count * 1000,
            app: None,
            window_instance: None,
            cursor: None,
        });
        let QueryResponse::SegmentDetail(response) = self.query(request) else {
            panic!("details response");
        };
        response
    }
    fn classifier(&mut self) {
        self.install("user.explain", "classification", None,
            "[[rule]]\nid='winner'\npriority=100\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='starts_with',value='app-'},{field='window.class',op='equals',value='Editor',negate=true}]\n[[rule]]\nid='shadowed'\ncategory='builtin:development'\nwhen=[{field='app.exe_name',op='starts_with',value='app-'}]\n");
    }
}
fn rule<'response>(
    response: &'response wire::RulesExplainResponse,
    id: &str,
) -> &'response wire::ExplainNode {
    response.fragments[0]
        .nodes
        .iter()
        .find(|node| node.rule.as_deref().is_some_and(|rule| rule.contains(id)))
        .expect("rule node")
}
#[test]
fn actual_and_missing_fields_use_retained_values_and_condition_results() {
    let mut fixture = Fixture::new(1);
    fixture.classifier();
    let response = fixture.explain(fixture.range()).expect("explanation");
    let fields: Vec<_> = response.fragments[0]
        .nodes
        .iter()
        .filter(|node| node.kind == wire::ExplainNodeKind::Condition)
        .collect();
    assert!(
        fields
            .iter()
            .any(|node| node.field == Some(wire::RuleField::ExeName)
                && node.value.as_deref() == Some("app-1.exe")
                && node.missing == Some(false)
                && node.matched == Some(true))
    );
    assert!(
        fields
            .iter()
            .any(|node| node.field == Some(wire::RuleField::WindowClass)
                && node.value.is_none()
                && node.missing == Some(true)
                && node.matched == Some(true))
    );
    assert_eq!(
        rule(&response, "/winner@").reason,
        Some(wire::ExplainReason::Selected)
    );
    assert!(matches!(
        rule(&response, "/shadowed@").reason,
        Some(wire::ExplainReason::Shadowed {
            order: wire::ExplainShadowOrder::Priority,
            ..
        })
    ));
    assert_eq!(
        rule(&response, "/shadowed@").state,
        wire::ExplainState::Shadowed
    );
}
#[test]
fn annotation_precedence_retains_the_rule_that_would_otherwise_win() {
    let mut fixture = Fixture::new(1);
    fixture.classifier();
    fixture.facts.annotate(
        1,
        wire::AnnotationKind::CategoryOverride,
        0,
        1000,
        Some(wire::BuiltinCategory::Development),
    );
    let response = fixture.explain(fixture.range()).expect("explain");
    assert!(
        matches!(&response.fragments[0].category_source, wire::ExplainCategorySource::Annotation {otherwise_rule: Some(rule), ..} if rule == "user.explain/winner@1")
    );
    assert_eq!(
        rule(&response, "/winner@").state,
        wire::ExplainState::Shadowed
    );
    assert!(matches!(
        rule(&response, "/winner@").reason,
        Some(wire::ExplainReason::Annotation { .. })
    ));
    assert_eq!(
        response.fragments[0].category,
        fixture.details().intervals[0].category
    );
}
#[test]
fn extraction_and_classification_agree_with_details_at_the_same_instant() {
    let mut fixture = Fixture::new(1);
    fixture.install("user.extract", "extraction", None, "[[rule]]\nid='document'\napp=[{field='app.exe_name',op='starts_with',value='app-'}]\npattern='private (?P<document>.+)'\n");
    fixture.install("user.classify", "classification", None, "[[rule]]\nid='document'\ncategory='builtin:development'\nwhen=[{field='doc.document',op='equals',value='title fixture'}]\n");
    let response = fixture
        .explain(json!({"at_utc_ms": fixture.facts.start + 500}))
        .expect("instant");
    let details = fixture.details();
    let fragment = &response.fragments[0];
    let detail = &details.intervals[0];
    assert_eq!(fragment.start_utc_ms, fixture.facts.start);
    assert_eq!(fragment.end_utc_ms, fixture.facts.start + 1000);
    assert_eq!(fragment.category, detail.category);
    assert_eq!(fragment.extraction, detail.extraction);
    assert_eq!(
        fragment
            .extraction
            .as_ref()
            .expect("extraction")
            .extractor_rule
            .as_deref(),
        Some("user.extract/document@1")
    );
    let nodes = &fragment.nodes;
    let extraction_end = nodes
        .iter()
        .rposition(|node| node.stage == wire::RuleKind::Extraction)
        .expect("extraction stage");
    let classification_start = nodes
        .iter()
        .position(|node| node.stage == wire::RuleKind::Classification)
        .expect("classification stage");
    assert!(extraction_end < classification_start);
    assert!(
        nodes
            .iter()
            .any(|node| node.field == Some(wire::RuleField::Document)
                && node.value.as_deref() == Some("title fixture"))
    );
}
#[test]
fn no_extraction_rule_returns_no_source_while_details_keep_the_fallback_identity() {
    let mut fixture = Fixture::new(1);
    let response = fixture.explain(fixture.range()).expect("explain");
    assert!(response.fragments[0].extraction.is_none());
    assert!(response.fragments[0].privacy.is_none());
    let details = fixture.details();
    let extraction = details.intervals[0]
        .extraction
        .as_ref()
        .expect("fallback identity");
    assert!(extraction.extractor_rule.is_none());
    assert_eq!(extraction.status, Some(wire::ExtractionStatus::Fallback));
}

#[test]
fn hidden_titles_never_appear_even_with_negated_privacy_and_classification_rules() {
    let mut fixture = Fixture::new(1);
    fixture.facts.hide_titles();
    fixture.install("user.privacy", "privacy", None, "[[rule]]\nid='hidden'\npreset='title_hidden'\nwhen=[{field='title.raw',op='contains',value='private title fixture',negate=true}]\n");
    fixture.install("user.classify", "classification", None, "[[rule]]\nid='missing'\ncategory='builtin:learning'\nwhen=[{field='title.raw',op='contains',value='private title fixture',negate=true}]\n");
    fixture.install("user.extract", "extraction", None, "[[rule]]\nid='extract'\napp=[{field='app.exe_name',op='starts_with',value='app-'}]\npattern='(?P<document>private title fixture)'\n");
    let response = fixture.explain(fixture.range()).expect("explain");
    let encoded = serde_json::to_string(&response).expect("JSON");
    assert!(!encoded.contains("private title fixture"));
    let fields: Vec<_> = response.fragments[0]
        .nodes
        .iter()
        .filter(|node| node.field == Some(wire::RuleField::RawTitle))
        .collect();
    assert!(!fields.is_empty());
    assert!(
        fields
            .iter()
            .all(|node| node.value.is_none() && node.missing == Some(true))
    );
    assert_eq!(
        response.fragments[0].category,
        fixture.details().intervals[0].category
    );
    assert!(response.fragments[0].extraction.is_none());
    fixture.facts.exclude_windows();
    let response = fixture.explain(fixture.range()).expect("excluded");
    assert!(
        !serde_json::to_string(&response)
            .expect("JSON")
            .contains("app-1.exe")
    );
    assert_eq!(
        response.fragments[0].category,
        wire::CategoryRef::Unclassified
    );
}
#[test]
fn historical_privacy_attribution_survives_changed_and_deleted_current_rules() {
    let mut fixture = Fixture::new(1);
    fixture
        .facts
        .retain_privacy_attribution("user.privacy/hidden@1", "Original label");
    fixture.install("user.privacy", "privacy", None, "[[rule]]\nid='hidden'\nlabel='New label'\npreset='excluded'\nwhen=[{field='app.exe_name',op='starts_with',value='app-'}]\n");
    let response = fixture.explain(fixture.range()).expect("changed rule");
    let attribution = response.fragments[0].privacy.clone().expect("attribution");
    assert_eq!(attribution.rule.as_deref(), Some("user.privacy/hidden@1"));
    assert_eq!(attribution.label.as_deref(), Some("Original label"));
    assert!(fixture.details().intervals[0].title.is_some());
    fixture.install("user.privacy", "privacy", None, "");
    let response = fixture.explain(fixture.range()).expect("deleted rule");
    assert_eq!(response.fragments[0].privacy, Some(attribution));
}
#[test]
fn clock_rollback_produces_distinct_fragments_and_allows_clock_selection() {
    let mut fixture = Fixture::new(2);
    fixture.facts.repeat_clock_for_last_fact();
    let mut body = json!({"at_utc_ms":fixture.facts.start + 500});
    let response = fixture.all(body.clone());
    assert_eq!(response.fragments.len(), 2);
    assert_eq!(
        response.fragments[0].clock_segment_id,
        Some(wire::ClockSegmentId(1))
    );
    assert_eq!(
        response.fragments[1].clock_segment_id,
        Some(wire::ClockSegmentId(2))
    );
    body["clock_segment_id"] = 2.into();
    let narrowed = fixture.all(body);
    assert_eq!(
        narrowed.fragments[0].category_source,
        response.fragments[1].category_source
    );
    assert_eq!(
        narrowed.fragments[0].extraction,
        response.fragments[1].extraction
    );
    assert_eq!(
        narrowed.fragments[0].nodes.len(),
        response.fragments[1].nodes.len()
    );
    for (left, right) in narrowed.fragments[0]
        .nodes
        .iter()
        .zip(&response.fragments[1].nodes)
    {
        assert_eq!(
            serde_json::to_value(left).expect("node"),
            serde_json::to_value(right).expect("node")
        );
    }
    assert_eq!(narrowed.fragments, response.fragments[1..]);
}
#[test]
fn ranges_keep_distinct_explanations_and_merge_identical_neighbours() {
    let mut fixture = Fixture::new(2);
    fixture.classifier();
    let distinct = fixture.explain(fixture.range()).expect("distinct");
    assert_eq!(distinct.fragments.len(), 2);
    fixture.facts.share_first_application();
    let merged = fixture.explain(fixture.range()).expect("merged");
    assert_eq!(merged.fragments.len(), 1);
    assert_eq!(
        merged.fragments[0].end_utc_ms - merged.fragments[0].start_utc_ms,
        2000
    );
}
#[test]
fn invalid_ranges_and_mixed_selectors_are_rejected() {
    for body in [
        json!({"start_utc_ms":1,"end_utc_ms":1}),
        json!({"start_utc_ms":2,"end_utc_ms":1}),
        json!({"start_utc_ms":0,"end_utc_ms":wire::SEGMENT_DETAIL_MAX_SPAN_MS+1}),
        json!({"start_utc_ms":i64::MIN,"end_utc_ms":i64::MAX}),
        json!({"at_utc_ms":i64::MAX}),
        json!({"at_utc_ms":1,"start_utc_ms":1,"end_utc_ms":2}),
    ] {
        assert_eq!(
            ExplainRequest::decode(body).expect_err("invalid").kind,
            ErrorKind::BadRequest
        );
    }
    ExplainRequest::decode(json!({"start_utc_ms":0,"end_utc_ms":wire::SEGMENT_DETAIL_MAX_SPAN_MS}))
        .expect("48 hours");
}
#[test]
fn cancellation_and_deadlines_return_no_partial_response_and_release_the_reader() {
    let mut fixture = Fixture::new(2);
    let mut request = ExplainRequest::decode(fixture.range()).expect("request");
    request.control.cancel();
    assert_eq!(
        fixture.run(request).expect_err("cancelled").kind(),
        ErrorKind::Cancelled
    );
    request = ExplainRequest::decode(fixture.range()).expect("request");
    request.control = QueryControl::expired();
    assert_eq!(
        fixture.run(request).expect_err("expired").kind(),
        ErrorKind::DeadlineExceeded
    );
    assert!(fixture.explain(fixture.range()).is_ok());
}
#[test]
fn cursors_reject_changed_inputs_revisions_parameters_and_invalid_positions() {
    let mut fixture = Fixture::new(1);
    let body = fixture.range();
    let response = fixture.explain(body.clone()).expect("first");
    let request = ExplainRequest::decode(body.clone()).expect("parameters");
    let cursor = wire::QueryCursor::new(response.versions.clone(), request.parameters.clone(), 1);
    let mut continuation = body.clone();
    continuation["cursor"] = serde_json::to_value(cursor).expect("cursor");
    assert!(fixture.explain(continuation.clone()).is_ok());
    let mut invalid = continuation.clone();
    invalid["end_utc_ms"] = (fixture.facts.start + 500).into();
    assert_eq!(
        fixture
            .explain(invalid)
            .expect_err("parameter mismatch")
            .kind(),
        ErrorKind::BadRequest
    );
    fixture.facts.bump_fact_version();
    assert_eq!(
        fixture
            .explain(continuation)
            .expect_err("changed facts")
            .kind(),
        ErrorKind::StaleCursor
    );
    let mut stale = body.clone();
    stale["expected_versions"] = "old inputs".into();
    assert!(matches!(
        fixture.explain(stale).expect_err("stale inputs").kind(),
        ErrorKind::StaleInputs { .. }
    ));
    let response = fixture.explain(body.clone()).expect("new page");
    let mut continuation = body;
    continuation["cursor"] = serde_json::to_value(wire::QueryCursor::new(
        response.versions,
        request.parameters,
        1,
    ))
    .expect("cursor");
    fixture.install("user.empty", "classification", None, "");
    assert_eq!(
        fixture
            .explain(continuation)
            .expect_err("new revision")
            .kind(),
        ErrorKind::StaleCursor
    );
}

#[test]
fn overridden_and_disabled_rules_keep_the_overlay_reason_and_defining_provenance() {
    let mut fixture = Fixture::new(1);
    fixture.install("user.base", "classification", None, "[[rule]]\nid='replaced'\ncategory='builtin:learning'\nwhen=[{field='app.identity_key',op='contains',value='fixture'}]\n[[rule]]\nid='disabled'\ncategory='builtin:learning'\nwhen=[{field='title.raw',op='contains',value='private'}]\n");
    fixture.install("user.overlay", "classification", Some("user.base"), "[[override]]\ntarget='user.base/replaced'\ncategory='builtin:development'\nwhen=[{field='app.exe_name',op='starts_with',value='app-'}]\n[[disable]]\ntarget='user.base/disabled'\n");
    fixture.install("user.sleeping", "classification", None, "[[rule]]\nid='sleeping'\ncategory='builtin:learning'\nwhen=[{field='title.raw',op='contains',value='private'}]\n");
    fixture.apply(RegistryCommand::Toggle {
        key: "user.sleeping".parse().expect("key"),
        base_version: 1,
        enabled: domain::Enablement::Disabled,
    });
    let response = fixture.explain(fixture.range()).expect("explanation");
    let replaced = rule(&response, "user.base/replaced@");
    assert_eq!(replaced.state, wire::ExplainState::Overridden);
    assert_eq!(
        replaced.reason,
        Some(wire::ExplainReason::Overridden {
            by_set: "user.overlay".into()
        })
    );
    let disabled = rule(&response, "user.base/disabled@");
    assert_eq!(disabled.state, wire::ExplainState::Disabled);
    assert_eq!(
        disabled.reason,
        Some(wire::ExplainReason::Disabled {
            by_set: "user.overlay".into()
        })
    );
    let replacement = rule(&response, "user.overlay/replaced@");
    assert_eq!(
        replacement.provenance,
        vec![
            wire::RuleProvenance {
                key: "user.base".into(),
                version: 1,
                id: "replaced".into()
            },
            wire::RuleProvenance {
                key: "user.overlay".into(),
                version: 1,
                id: "replaced".into()
            }
        ]
    );
    assert_eq!(
        rule(&response, "/sleeping@").reason,
        Some(wire::ExplainReason::SetDisabled)
    );
    assert!(
        response.fragments[0]
            .nodes
            .iter()
            .any(|node| node.field == Some(wire::RuleField::IdentityKey)
                && node.value.as_deref() == Some("exe:c:/fixture/app-1.exe"))
    );
    assert!(
        response.fragments[0]
            .nodes
            .windows(2)
            .all(|pair| pair[0].path < pair[1].path)
    );
}

#[test]
fn shadowed_rules_report_depth_and_file_order_after_equal_priorities() {
    let mut fixture = Fixture::new(1);
    fixture.install("user.base", "classification", None, "[[rule]]\nid='parent'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='starts_with',value='app-'}]\n");
    fixture.install("user.child", "classification", Some("user.base"), "[[rule]]\nid='first'\ncategory='builtin:development'\nwhen=[{field='app.exe_name',op='starts_with',value='app-'}]\n[[rule]]\nid='second'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='starts_with',value='app-'}]\n");
    let response = fixture.explain(fixture.range()).expect("explanation");
    assert!(matches!(
        rule(&response, "/parent@").reason,
        Some(wire::ExplainReason::Shadowed {
            order: wire::ExplainShadowOrder::Depth,
            ..
        })
    ));
    assert!(matches!(
        rule(&response, "/second@").reason,
        Some(wire::ExplainReason::Shadowed {
            order: wire::ExplainShadowOrder::Order,
            ..
        })
    ));
}

#[test]
fn continuation_pages_repeat_fragment_sources_and_return_only_remaining_nodes() {
    let mut fixture = Fixture::new(1);
    fixture.classifier();
    let fragment = fixture.all(fixture.range()).fragments.remove(0);
    let mut large = fragment.clone();
    large.nodes = (0..150)
        .map(|index| {
            let mut node = fragment.nodes.last().expect("node").clone();
            node.path = vec![0, 0, index];
            node.value = Some("x".repeat(4096));
            node
        })
        .collect();
    let original_nodes = large.nodes.clone();
    let parameters = ExplainRequest::decode(fixture.range())
        .expect("request")
        .parameters;
    let versions = wire::QueryVersions("versions".into());
    let mut position = PagePosition::First;
    let mut recovered = Vec::new();
    let mut pages = 0;
    loop {
        let mut page = ExplainPage::new(
            vec![large.clone()],
            versions.clone(),
            parameters.clone(),
            position,
        )
        .expect("page");
        while page.advance(&QueryControl::default()).expect("page turn") == PageProgress::More {}
        let response = page.response;
        assert!(serde_json::to_vec(&response).expect("JSON").len() <= wire::QUERY_PAGE_BYTES);
        assert_eq!(response.fragments.len(), 1);
        let part = &response.fragments[0];
        assert_eq!(part.start_utc_ms, large.start_utc_ms);
        assert_eq!(part.end_utc_ms, large.end_utc_ms);
        assert_eq!(part.clock_segment_id, large.clock_segment_id);
        assert_eq!(part.category_source, large.category_source);
        assert_eq!(part.extraction, large.extraction);
        assert_eq!(part.privacy, large.privacy);
        recovered.extend(part.nodes.clone());
        pages += 1;
        let Some(cursor) = response.next_cursor else {
            break;
        };
        position =
            PagePosition::from_cursor(Some(&cursor), &versions, &parameters).expect("position");
    }
    assert!(pages > 1);
    assert_eq!(recovered, original_nodes);
}

#[test]
fn oversized_nodes_and_invalid_continuations_fail_without_repeating_a_cursor() {
    let mut fixture = Fixture::new(1);
    let mut fragment = fixture.all(fixture.range()).fragments.remove(0);
    fragment.nodes.truncate(1);
    fragment.nodes[0].value = Some("x".repeat(wire::QUERY_PAGE_BYTES));
    let parameters = ExplainRequest::decode(fixture.range())
        .expect("request")
        .parameters;
    let versions = wire::QueryVersions("versions".into());
    let mut page = ExplainPage::new(
        vec![fragment.clone()],
        versions.clone(),
        parameters.clone(),
        PagePosition::First,
    )
    .expect("page");
    assert!(matches!(
        page.advance(&QueryControl::default()),
        Err(RunQueryError::PageTooLarge)
    ));
    let mut page = ExplainPage::new(
        vec![fragment.clone()],
        versions.clone(),
        parameters.clone(),
        PagePosition::Cursor(usize::MAX),
    )
    .expect("page");
    assert!(matches!(
        page.advance(&QueryControl::default()),
        Err(RunQueryError::InvalidCursor)
    ));
    let mut page = ExplainPage::new(
        vec![fragment],
        versions,
        parameters,
        PagePosition::Cursor(1),
    )
    .expect("page");
    let control = QueryControl::default();
    control.cancel();
    assert_eq!(
        page.advance(&control).expect_err("cancelled").kind(),
        ErrorKind::Cancelled
    );
    assert_eq!(
        page.advance(&QueryControl::expired())
            .expect_err("expired")
            .kind(),
        ErrorKind::DeadlineExceeded
    );
}

#[test]
fn final_categories_match_day_hour_stacks_and_detail_sources_for_the_same_facts() {
    let mut fixture = Fixture::new(2);
    fixture.classifier();
    fixture.facts.annotate(
        1,
        wire::AnnotationKind::CategoryOverride,
        1000,
        2000,
        Some(wire::BuiltinCategory::Development),
    );
    let explained = fixture.all(fixture.range());
    let details = fixture.details();
    for fragment in &explained.fragments {
        let detail = details
            .intervals
            .iter()
            .find(|interval| interval.start_utc_ms == fragment.start_utc_ms)
            .expect("same detail slice");
        assert_eq!(fragment.category, detail.category);
        match (&fragment.category_source, &detail.category_source) {
            (
                wire::ExplainCategorySource::Rule { rule: explained },
                wire::CategorySource::Rule { rule: detail },
            ) => assert_eq!(explained, detail.as_str()),
            (
                wire::ExplainCategorySource::Annotation {
                    annotation: explained,
                    ..
                },
                wire::CategorySource::Annotation { annotation: detail },
            ) => assert_eq!(explained, detail),
            _ => panic!("same category source"),
        }
    }
    let QueryResponse::Day(day) = fixture.query(fixture.facts.request_day()) else {
        panic!("day response");
    };
    for fragment in &explained.fragments {
        let expected: i64 = explained
            .fragments
            .iter()
            .filter(|other| other.category == fragment.category)
            .map(|other| other.end_utc_ms - other.start_utc_ms)
            .sum();
        let actual: i64 = day
            .hours
            .as_ref()
            .expect("hours")
            .iter()
            .flat_map(|hour| &hour.categories)
            .filter(|duration| duration.category == fragment.category)
            .map(|duration| duration.duration_ms)
            .sum();
        assert_eq!(actual, expected);
    }
    assert_eq!(day.metrics.expect("metrics").active_ms, 2000);
}

#[test]
fn input_versions_accept_details_and_reject_later_mutations() {
    let mut fixture = Fixture::new(1);
    let versions = fixture.details().versions;
    let mut body = fixture.range();
    body["expected_versions"] = serde_json::to_value(&versions).expect("versions");
    fixture.explain(body.clone()).expect("same inputs");
    fixture.facts.bump_fact_version();
    assert!(matches!(
        fixture.explain(body).expect_err("changed inputs").kind(),
        ErrorKind::StaleInputs { .. }
    ));
}

#[test]
fn cancellation_after_admission_discards_work_and_preserves_snapshot_cleanup() {
    let mut fixture = Fixture::new(1);
    let request =
        ExplainRequest::decode(json!({"at_utc_ms": fixture.facts.start+500})).expect("request");
    let control = request.control.clone();
    let mut reader = fixture.facts.store.take_query_reader();
    let mut job =
        ExplainJob::start(&mut reader, request, fixture.registry.snapshots().load()).expect("job");
    assert!(job.advance().expect("first turn").is_none());
    control.cancel();
    assert_eq!(
        job.advance().expect_err("cancelled").kind(),
        ErrorKind::Cancelled
    );
    fixture
        .facts
        .store
        .return_query_reader(job.finish().expect("finish cancelled snapshot"));
    fixture.explain(fixture.range()).expect("reader reused");
}

#[test]
fn retained_values_use_the_evaluators_unicode_normalization_and_field_limit() {
    let mut fixture = Fixture::new(1);
    fixture
        .facts
        .replace_retained_title(&"e\u{301}🦀".repeat(4096));
    fixture.install("user.unicode", "classification", None, "[[rule]]\nid='bounded'\ncategory='builtin:learning'\nwhen=[{field='title.raw',op='starts_with',value='é🦀'}]\n");
    let response = fixture.all(fixture.range());
    let condition = response.fragments[0]
        .nodes
        .iter()
        .find(|node| {
            node.stage == wire::RuleKind::Classification
                && node.field == Some(wire::RuleField::RawTitle)
        })
        .expect("raw title condition");
    let actual = condition.value.as_deref().expect("retained value");
    assert!(actual.starts_with("é🦀"));
    assert_eq!(actual.encode_utf16().count(), 4096);
    assert_eq!(condition.matched, Some(true));
}

#[test]
fn offline_activity_discloses_only_the_post_annotation_detail_fields() {
    for background in ["idle", "stopped", "gap", "no_foreground", "outside_clock"] {
        let mut fixture = Fixture::new(1);
        fixture.classifier();
        fixture
            .facts
            .retain_privacy_attribution("user.privacy/old@1", "Historical label");
        let mut start = 0;
        match background {
            "idle" => fixture.facts.mask_idle(0, 1000),
            "stopped" => fixture.facts.mask_locked(0, 1000),
            "gap" => fixture.facts.mask_gap(0, 1000),
            "no_foreground" => fixture.facts.clear_foreground(),
            "outside_clock" => start = -1000,
            _ => unreachable!("fixture background"),
        }
        fixture.facts.annotate(
            9,
            wire::AnnotationKind::OfflineActivity,
            start,
            start + 1000,
            Some(wire::BuiltinCategory::Development),
        );
        let response = fixture.all(json!({"start_utc_ms":fixture.facts.start+start,"end_utc_ms":fixture.facts.start+start+1000}));
        let fragment = &response.fragments[0];
        let QueryResponse::SegmentDetail(details) = fixture.query(QueryRequest::SegmentDetail(
            wire::QuerySegmentDetailRequest {
                start_utc_ms: fixture.facts.start + start,
                end_utc_ms: fixture.facts.start + start + 1000,
                app: None,
                window_instance: None,
                cursor: None,
            },
        )) else {
            panic!("details");
        };
        let detail = &details.intervals[0];
        assert!(detail.app.is_none() && detail.title.is_none());
        assert_eq!(fragment.category, detail.category);
        assert!(fragment.extraction.is_none(), "{background}");
        assert!(fragment.privacy.is_none(), "{background}");
        assert!(
            matches!(
                fragment.category_source,
                wire::ExplainCategorySource::Annotation {
                    otherwise_rule: None,
                    ..
                }
            ),
            "{background}"
        );
        for node in &fragment.nodes {
            assert!(node.value.is_none(), "{background}: condition values");
            if node.kind == wire::ExplainNodeKind::Rule {
                assert_eq!(node.state, wire::ExplainState::Disabled, "{background}");
                assert_eq!(
                    node.reason,
                    Some(wire::ExplainReason::StageUnavailable),
                    "{background}"
                );
            }
        }
        if background == "outside_clock" {
            assert!(fragment.clock_segment_id.is_none());
        }
    }
}

#[test]
fn startup_compilation_failures_keep_disabled_rules_and_unknown_condition_results() {
    let mut fixture = Fixture::new(1);
    fixture.classifier();
    fixture.startup(|state| {
        let set = &mut state
            .sets
            .get_mut(&"user.explain".parse().expect("key"))
            .expect("set")
            .current
            .set;
        let domain::RuleBody::Classification { when, .. } = &mut set.content.rule[0].body else {
            panic!("classification");
        };
        when[0].op = domain::Operator::Regex;
        when[0].value = "[".into();
    });
    let response = fixture.all(fixture.range());
    let node = rule(&response, "user.explain/winner@");
    assert_eq!(node.state, wire::ExplainState::Disabled);
    let condition = response.fragments[0]
        .nodes
        .iter()
        .find(|condition| {
            condition.path.starts_with(&node.path)
                && condition.field == Some(wire::RuleField::ExeName)
        })
        .expect("condition");
    assert_eq!(condition.value.as_deref(), Some("app-1.exe"));
    assert_eq!(condition.matched, None);
}

#[test]
fn startup_orphaned_and_cyclic_sets_have_disabled_root_paths() {
    for parent in ["user.missing", "user.explain"] {
        let mut fixture = Fixture::new(1);
        fixture.install("user.missing", "classification", None, "");
        fixture.classifier();
        fixture.startup(|state| {
            state
                .sets
                .get_mut(&"user.missing".parse().expect("key"))
                .expect("parent")
                .deletion = domain::registry::RuleSetDeletion::Deleted;
            state
                .sets
                .get_mut(&"user.explain".parse().expect("key"))
                .expect("set")
                .current
                .set
                .header
                .parent = Some(parent.into());
        });
        let response = fixture.all(fixture.range());
        let set = response.fragments[0]
            .nodes
            .iter()
            .find(|node| {
                node.set
                    .as_ref()
                    .is_some_and(|set| set.key == "user.explain")
            })
            .expect("orphan set");
        assert_eq!(set.path.len(), 1);
        assert_eq!(set.state, wire::ExplainState::Disabled);
    }
}

#[test]
fn local_and_override_rules_with_the_same_id_keep_distinct_outcomes_and_suppression() {
    let mut fixture = Fixture::new(1);
    fixture.install("user.base","classification",None,"[[rule]]\nid='shared'\ncategory='builtin:learning'\nwhen=[{field='title.raw',op='contains',value='private'}]\n");
    fixture.install("user.local","classification",Some("user.base"),"[[rule]]\nid='shared'\npriority=100\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='equals',value='never.exe'}]\n[[override]]\ntarget='user.base/shared'\npriority=90\ncategory='builtin:development'\nwhen=[{field='title.raw',op='contains',value='private'}]\n");
    let response = fixture.all(fixture.range());
    let matching: Vec<_> = response.fragments[0]
        .nodes
        .iter()
        .filter(|node| node.rule.as_deref() == Some("user.local/shared@1"))
        .collect();
    assert_eq!(matching.len(), 2);
    assert_eq!(matching[0].reason, Some(wire::ExplainReason::DidNotMatch));
    assert_eq!(matching[1].reason, Some(wire::ExplainReason::Selected));
    fixture.install(
        "user.child",
        "classification",
        Some("user.local"),
        "[[disable]]\ntarget='user.base/shared'\n",
    );
    let response = fixture.all(fixture.range());
    let local = response.fragments[0]
        .nodes
        .iter()
        .find(|node| {
            node.rule.as_deref() == Some("user.local/shared@1")
                && node
                    .provenance
                    .first()
                    .is_some_and(|source| source.key == "user.local")
        })
        .expect("local definition");
    assert_eq!(local.reason, Some(wire::ExplainReason::DidNotMatch));
    assert_eq!(local.state, wire::ExplainState::Effective);
}

#[test]
fn archived_categories_never_report_a_selected_or_annotation_shadowed_rule() {
    for annotation in [None, Some(wire::BuiltinCategory::Development)] {
        let mut fixture = Fixture::new(1);
        fixture.classifier();
        fixture.facts.archive_learning_category();
        if let Some(category) = annotation {
            fixture.facts.annotate(
                1,
                wire::AnnotationKind::CategoryOverride,
                0,
                1000,
                Some(category),
            );
        }
        let response = fixture.all(fixture.range());
        let node = rule(&response, "/winner@");
        assert_eq!(node.state, wire::ExplainState::Disabled);
        assert_eq!(node.reason, Some(wire::ExplainReason::CategoryUnavailable));
        if annotation.is_some() {
            assert!(matches!(
                response.fragments[0].category_source,
                wire::ExplainCategorySource::Annotation {
                    otherwise_rule: None,
                    ..
                }
            ));
        }
        assert!(!matches!(
            node.reason,
            Some(wire::ExplainReason::Selected | wire::ExplainReason::Annotation { .. })
        ));
        assert_eq!(
            response.fragments[0].category,
            fixture.details().intervals[0].category
        );
        if annotation.is_none() {
            assert_eq!(
                response.fragments[0].category,
                wire::CategoryRef::Unclassified
            );
        }
    }
}

#[test]
fn instant_notes_and_category_overrides_do_not_widen_the_fact_scan() {
    let day = quartz_domain::capture::limits::UTC_DAY_MS;
    for kind in [
        wire::AnnotationKind::Note,
        wire::AnnotationKind::CategoryOverride,
    ] {
        let mut fixture = Fixture::new(1);
        fixture.facts.annotate(
            8,
            kind,
            -14 * day,
            14 * day,
            (kind == wire::AnnotationKind::CategoryOverride)
                .then_some(wire::BuiltinCategory::Development),
        );
        let request =
            ExplainRequest::decode(json!({"at_utc_ms":fixture.facts.start+500})).expect("request");
        let mut reader = fixture.facts.store.take_query_reader();
        let mut job = ExplainJob::start(&mut reader, request, fixture.rules()).expect("job");
        while matches!(job.stage, Stage::SelectInstant { .. }) {
            assert!(job.advance().expect("selection turn").is_none());
        }
        assert_eq!(job.scan_range.start, fixture.facts.start);
        assert_eq!(job.scan_range.end, fixture.facts.start + 1000);
        fixture
            .facts
            .store
            .return_query_reader(job.finish().expect("finish"));
    }
}

#[test]
fn offline_instant_expansion_is_bounded_and_clock_selection_keeps_fact_boundaries() {
    let day = quartz_domain::capture::limits::UTC_DAY_MS;
    let mut fixture = Fixture::new(1);
    fixture.facts.mask_idle(0, 1000);
    fixture.facts.annotate(
        8,
        wire::AnnotationKind::OfflineActivity,
        -14 * day,
        14 * day,
        Some(wire::BuiltinCategory::Development),
    );
    for clock in [None, Some(wire::ClockSegmentId(1))] {
        let request = ExplainRequest::decode(
            json!({"at_utc_ms":fixture.facts.start+500,"clock_segment_id":clock}),
        )
        .expect("request");
        let mut reader = fixture.facts.store.take_query_reader();
        let mut job = ExplainJob::start(&mut reader, request, fixture.rules()).expect("job");
        while matches!(job.stage, Stage::SelectInstant { .. }) {
            job.advance().expect("selection turn");
        }
        assert!(job.scan_range.end - job.scan_range.start <= wire::SEGMENT_DETAIL_MAX_SPAN_MS);
        if clock.is_some() {
            assert_eq!(job.scan_range.start, fixture.facts.start);
            assert_eq!(job.scan_range.end, fixture.facts.start + 1000);
        }
        let state = &job.version_state;
        assert_eq!(
            state.settings.metadata_version,
            job.snapshot.settings().expect("settings").metadata_version
        );
        fixture
            .facts
            .store
            .return_query_reader(job.finish().expect("finish"));
    }
}

#[test]
fn continuations_accept_an_explicit_default_deadline_and_reject_changed_expected_versions() {
    let mut fixture = Fixture::new(1);
    let mut body = fixture.range();
    let response = fixture.explain(body.clone()).expect("first page");
    assert!(!response.fragments.is_empty());
    let parameters = ExplainRequest::decode(body.clone())
        .expect("request")
        .parameters;
    body["cursor"] = serde_json::to_value(wire::QueryCursor::new(response.versions, parameters, 1))
        .expect("cursor");
    body["deadline_ms"] = wire::PREVIEW_DEFAULT_DEADLINE_MS.into();
    fixture.explain(body).expect("equivalent default");
    let mut body = fixture.range();
    body["expected_versions"] = serde_json::to_value(fixture.details().versions).expect("versions");
    let response = fixture.explain(body.clone()).expect("matching first page");
    let parameters = ExplainRequest::decode(body.clone())
        .expect("request")
        .parameters;
    body["cursor"] = serde_json::to_value(wire::QueryCursor::new(response.versions, parameters, 1))
        .expect("cursor");
    fixture.facts.bump_fact_version();
    assert_eq!(
        fixture.explain(body).expect_err("changed inputs").kind(),
        ErrorKind::StaleCursor
    );
}

#[test]
fn instant_cursors_include_days_only_present_in_the_expanded_scan() {
    let mut fixture = Fixture::new(1);
    fixture.facts.extend_first_fact_into_previous_day();
    let mut body = json!({"at_utc_ms":fixture.facts.start+500});
    let response = fixture.explain(body.clone()).expect("first page");
    assert!(!response.fragments.is_empty());
    let parameters = ExplainRequest::decode(body.clone())
        .expect("request")
        .parameters;
    body["cursor"] = serde_json::to_value(wire::QueryCursor::new(response.versions, parameters, 1))
        .expect("cursor");
    let continuation =
        ExplainRequest::decode(body.clone()).expect("valid instant continuation decoder");
    continuation
        .cursor
        .as_ref()
        .expect("cursor")
        .decode()
        .expect("cursor schema");
    fixture.facts.bump_previous_fact_day();
    assert_eq!(
        fixture
            .run(continuation)
            .expect_err("expanded inputs changed")
            .kind(),
        ErrorKind::StaleCursor
    );
}

#[test]
fn capture_levels_and_missing_foreground_disable_the_unavailable_stages() {
    for level in ["title_hidden", "excluded", "no_foreground"] {
        let mut fixture = Fixture::new(1);
        fixture.classifier();
        match level {
            "title_hidden" => fixture.facts.hide_titles(),
            "excluded" => fixture.facts.exclude_windows(),
            "no_foreground" => fixture.facts.clear_foreground(),
            _ => unreachable!("fixture"),
        };
        let response = fixture.all(fixture.range());
        for node in response.fragments[0].nodes.iter().filter(|node| {
            node.kind == wire::ExplainNodeKind::Rule
                && (level != "title_hidden" || node.stage == wire::RuleKind::Extraction)
        }) {
            assert_eq!(node.state, wire::ExplainState::Disabled);
            assert_eq!(node.reason, Some(wire::ExplainReason::StageUnavailable));
        }
    }
}

#[test]
fn missing_targets_and_disabled_ancestors_keep_their_specific_reasons() {
    let mut fixture = Fixture::new(1);
    fixture.install("user.base","classification",None,"[[rule]]\nid='target'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='contains',value='app'}]\n");
    fixture.install("user.child","classification",Some("user.base"),"[[override]]\ntarget='user.base/target'\ncategory='builtin:development'\nwhen=[{field='title.raw',op='contains',value='private'}]\n[[rule]]\nid='child'\ncategory='builtin:learning'\nwhen=[{field='app.exe_name',op='contains',value='app'}]\n");
    fixture.startup(|state| {
        let set = &mut state
            .sets
            .get_mut(&"user.base".parse().expect("key"))
            .expect("base")
            .current
            .set;
        set.content.rule.clear();
        set.content.order.clear();
    });
    let response = fixture.all(fixture.range());
    assert_eq!(
        rule(&response, "user.child/target@").reason,
        Some(wire::ExplainReason::TargetMissing)
    );
    fixture.snapshot = None;
    fixture.apply(RegistryCommand::Toggle {
        key: "user.base".parse().expect("key"),
        base_version: 1,
        enabled: domain::Enablement::Disabled,
    });
    let response = fixture.all(fixture.range());
    assert_eq!(
        rule(&response, "user.child/child@").reason,
        Some(wire::ExplainReason::SetDisabled)
    );
    let child = response.fragments[0]
        .nodes
        .iter()
        .find(|node| node.set.as_ref().is_some_and(|set| set.key == "user.child"))
        .expect("child");
    assert_eq!(child.state, wire::ExplainState::Disabled);
}

#[test]
fn pages_preserve_two_fragment_headers_and_a_zero_node_fragment() {
    let mut fixture = Fixture::new(1);
    let header = fixture.all(fixture.range()).fragments.remove(0);
    let mut first = header.clone();
    first.nodes = (0..150)
        .map(|index| {
            let mut node = header.nodes[0].clone();
            node.path = vec![index];
            node.value = Some("x".repeat(4096));
            node
        })
        .collect();
    let mut empty = header.clone();
    empty.start_utc_ms += 1000;
    empty.end_utc_ms += 1000;
    empty.nodes.clear();
    let mut last = first.clone();
    last.start_utc_ms += 2000;
    last.end_utc_ms += 2000;
    let originals = vec![first, empty, last];
    let mut recovered: Vec<wire::ExplainFragment> = Vec::new();
    let parameters = ExplainRequest::decode(fixture.range())
        .expect("request")
        .parameters;
    let versions = wire::QueryVersions("v".into());
    let mut position = PagePosition::First;
    let mut pages = 0;
    loop {
        let mut page = ExplainPage::new(
            originals.clone(),
            versions.clone(),
            parameters.clone(),
            position,
        )
        .expect("page");
        while page.advance(&QueryControl::default()).expect("turn") == PageProgress::More {}
        let response = page.response;
        pages += 1;
        assert!(serde_json::to_vec(&response).expect("JSON").len() <= wire::QUERY_PAGE_BYTES);
        for mut fragment in response.fragments {
            if let Some(previous) = recovered
                .last_mut()
                .filter(|previous| previous.start_utc_ms == fragment.start_utc_ms)
            {
                previous.nodes.append(&mut fragment.nodes);
            } else {
                recovered.push(fragment);
            }
        }
        let Some(cursor) = response.next_cursor else {
            break;
        };
        position =
            PagePosition::from_cursor(Some(&cursor), &versions, &parameters).expect("cursor");
    }
    assert!(pages > 2);
    assert_eq!(recovered, originals);
}

#[test]
fn blank_ranges_are_empty_and_range_endpoints_clip_instant_boundaries() {
    let mut fixture = Fixture::new(1);
    let origin = fixture.facts.start;
    assert!(
        fixture
            .all(json!({"start_utc_ms":origin-1000,"end_utc_ms":origin}))
            .fragments
            .is_empty()
    );
    let clipped = fixture.all(json!({"start_utc_ms":origin+200,"end_utc_ms":origin+800}));
    assert_eq!(clipped.fragments[0].start_utc_ms, origin + 200);
    assert_eq!(clipped.fragments[0].end_utc_ms, origin + 800);
    let instant = fixture.all(json!({"at_utc_ms":origin+500}));
    assert_eq!(instant.fragments[0].start_utc_ms, origin);
    assert_eq!(instant.fragments[0].end_utc_ms, origin + 1000);
}

#[test]
fn oversized_page_items_map_to_the_published_protocol_error() {
    assert_eq!(RunQueryError::PageTooLarge.kind(), ErrorKind::PageTooLarge);
    assert_eq!(
        serde_json::to_value(ProtocolError::new(RunQueryError::PageTooLarge.kind())).expect("wire"),
        json!({"code":"page_too_large"})
    );
}
