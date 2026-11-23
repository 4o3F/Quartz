use super::*;
use crate::{domain_name::DomainName, text::NfcText};
use std::collections::BTreeMap;

fn catalog() -> ReferenceCatalog {
    ReferenceCatalog {
        categories: [
            "development",
            "learning",
            "communication",
            "entertainment",
            "system",
        ]
        .into_iter()
        .map(|key| (format!("builtin:{key}"), format!("uuid-{key}")))
        .chain([("开发".into(), "uuid-development".into())])
        .collect(),
    }
}
fn user(source: &str) -> RuleSet {
    RuleSet::parse(
        "test.toml",
        source,
        ParseContext {
            origin: RuleOrigin::User,
            references: &catalog(),
        },
    )
    .expect("valid test rule set")
}
fn privacy(key: &str, parent: Option<&str>, rules: &str) -> RuleSet {
    let parent = parent
        .map(|p| format!("parent = '{p}'\n"))
        .unwrap_or_default();
    user(&format!(
        "format = 'quartz-rules/1'\n[set]\nkey = '{key}'\nkind = 'privacy'\nname = 'Test'\n{parent}{rules}"
    ))
}
fn rule(id: &str) -> String {
    format!(
        "\n[[rule]]\nid = '{id}'\nlabel = 'Hide'\nwhen = [{{ field = 'title.raw', op = 'contains', value = 'secret' }}]\npreset = 'title_hidden'\n"
    )
}
fn installed(set: RuleSet) -> InstalledSet {
    InstalledSet {
        set,
        version: 1,
        enabled: Enablement::Enabled,
        sibling_order: 0,
    }
}
fn overlay(target: &str) -> String {
    format!(
        "\n[[override]]\ntarget = '{target}'\nlabel = 'Private'\nwhen = [{{ field = 'title.raw', op = 'contains', value = 'secret' }}]\npreset = 'private_window'\n"
    )
}

#[test]
fn builtin_privacy_is_valid_and_all_language_variants_are_present() {
    let set = RuleSet::parse(
        "builtin-privacy.toml",
        BUILTIN_PRIVACY,
        ParseContext {
            origin: RuleOrigin::Builtin,
            references: &catalog(),
        },
    )
    .expect("valid embedded privacy rules");
    assert_eq!(set.header.key, "builtin.privacy");
    assert_eq!(set.content.rule.len(), 12);
    let compiled = Resolution::resolve(&[installed(set)], RuleKind::Privacy)
        .effective
        .compile()
        .expect("compile builtin");
    for (exe, class, title) in [
        ("msedge.exe", "Chrome_WidgetWin_1", "Page - InPrivate"),
        ("chrome.exe", "Chrome_WidgetWin_1", "新建无痕窗口"),
        ("chrome.exe", "Chrome_WidgetWin_1", "Incognito"),
        ("firefox.exe", "MozillaWindowClass", "页面 - 隐私浏览"),
        (
            "firefox.exe",
            "MozillaWindowClass",
            "Page - Private Browsing",
        ),
    ] {
        let snapshot = FieldSnapshot {
            text: BTreeMap::from([
                (Field::ExeName, NfcText::new(exe)),
                (Field::WindowClass, NfcText::new(class)),
                (Field::RawTitle, NfcText::new(title)),
            ]),
            domain: None,
        };
        assert_eq!(
            compiled.evaluate(&snapshot).privacy.web,
            WebAction::None,
            "{exe} {title}"
        );
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractionFixture {
    exe: Option<String>,
    title: Option<String>,
    language: String,
    display_name: Option<String>,
    document: Option<String>,
    project: Option<String>,
    profile: Option<String>,
    dirty: bool,
    normalized_title: Option<String>,
    status: String,
    extractor_rule: Option<String>,
}

#[test]
fn builtin_extraction_matches_multilingual_fixtures_and_rejects_lookalikes() {
    let set = RuleSet::parse(
        "builtin-extraction.toml",
        BUILTIN_EXTRACTION,
        ParseContext {
            origin: RuleOrigin::Builtin,
            references: &catalog(),
        },
    )
    .expect("valid embedded extraction rules");
    set.validate_compilation().expect("bounded builtin regexes");
    let fixtures: Vec<ExtractionFixture> = serde_json::from_str(include_str!(
        "../../tests/fixtures/rules/builtin-extraction.json"
    ))
    .expect("extraction fixtures");
    for rule in &set.content.rule {
        let metadata = rule.builtin.as_ref().expect("builtin metadata");
        assert_eq!(metadata.verified, Some(false), "no live app verification");
        assert!(metadata.detector.is_some(), "detector for {}", rule.id);
        assert!(metadata.label_key.is_some(), "label for {}", rule.id);
        for language in ["zh-CN", "en"] {
            assert!(metadata.languages.iter().any(|value| value == language));
            let explanation = format!("builtin.extraction/{}@1", rule.id);
            assert!(
                fixtures.iter().any(|fixture| fixture.language == language
                    && fixture.extractor_rule.as_deref() == Some(&explanation)),
                "{} needs a {language} fixture",
                rule.id
            );
        }
    }
    let compiled = Resolution::resolve(&[installed(set)], RuleKind::Extraction)
        .effective
        .compile()
        .expect("compile builtin extraction");
    assert!(compiled.fields().all(Field::is_app));
    for fixture in fixtures {
        let mut snapshot = FieldSnapshot::default();
        for (field, value) in [
            (Field::ExeName, &fixture.exe),
            (Field::RawTitle, &fixture.title),
            (Field::DisplayName, &fixture.display_name),
        ] {
            if let Some(value) = value {
                snapshot.text.insert(field, NfcText::new(value));
            }
        }
        let expected_status = match fixture.status.as_str() {
            "extracted" => ExtractionStatus::Extracted,
            "fallback" => ExtractionStatus::Fallback,
            "empty" => ExtractionStatus::Empty,
            "unknown" => ExtractionStatus::Unknown,
            value => panic!("invalid fixture status {value}"),
        };
        let result = compiled.evaluate(&snapshot);
        assert_eq!(
            result.selected, fixture.extractor_rule,
            "provenance for {:?}: {:?}",
            fixture.exe, fixture.title
        );
        assert_eq!(
            result.extraction,
            Some(Extraction {
                document: fixture.document,
                project: fixture.project,
                profile: fixture.profile,
                dirty: fixture.dirty,
                normalized_title: fixture.normalized_title,
                status: expected_status,
            }),
            "outputs for {:?}: {:?}",
            fixture.exe,
            fixture.title
        );
    }
}
#[test]
fn user_files_reject_every_builtin_only_key() {
    for key in [
        "detector = 'builtin.browser_private'",
        "languages = ['en']",
        "verified = true",
        "label_key = 'privacy.label.private_window'",
    ] {
        let source = format!(
            "{}\n{key}\n",
            include_str!("../../tests/fixtures/rules/value.toml")
                .replace("value = \"\"", "value = \"secret\"")
        );
        let diagnostics = RuleSet::parse(
            "user.toml",
            &source,
            ParseContext {
                origin: RuleOrigin::User,
                references: &catalog(),
            },
        )
        .expect_err("builtin-only key")
        .into_diagnostics();
        assert!(
            diagnostics.iter().any(|d| d.code == DiagnosticCode::Key),
            "{key}"
        );
    }
}
#[test]
fn every_error_code_has_a_failing_fixture() {
    let fixtures = [
        ("format", DiagnosticCode::Format),
        ("key", DiagnosticCode::Key),
        ("set", DiagnosticCode::Set),
        ("parent", DiagnosticCode::Parent),
        ("target", DiagnosticCode::Target),
        ("conflict", DiagnosticCode::Conflict),
        ("immutable", DiagnosticCode::Immutable),
        ("field", DiagnosticCode::Field),
        ("op", DiagnosticCode::Op),
        ("value", DiagnosticCode::Value),
        ("regex", DiagnosticCode::Regex),
        ("ref", DiagnosticCode::Ref),
        ("dup", DiagnosticCode::Dup),
    ];
    for (name, code) in fixtures {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/fixtures/rules/{name}.toml"));
        let source = std::fs::read_to_string(&path).expect("fixture exists");
        let diagnostics = match RuleSet::parse(
            &path.to_string_lossy(),
            &source,
            ParseContext {
                origin: RuleOrigin::User,
                references: &catalog(),
            },
        ) {
            Err(error) => error.into_diagnostics(),
            Ok(set) => {
                let mut sets = vec![installed(set)];
                if code == DiagnosticCode::Conflict {
                    sets.push(installed(privacy("user.root", None, &rule("one"))));
                    sets.push(installed(privacy(
                        "user.sibling",
                        Some("user.root"),
                        "[[disable]]\ntarget = 'user.root/one'\n",
                    )));
                }
                Resolution::resolve(&sets, RuleKind::Privacy).diagnostics
            }
        };
        assert!(
            diagnostics.iter().any(|d| d.code == code),
            "{name}: {diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .all(|d| d.location.line > 0 && d.location.column > 0)
        );
    }
}
#[test]
fn diagnostics_collect_independent_failures_and_exact_unicode_locations() {
    let source = "format = 'quartz-rules/1'\n[set]\nkey = 'user.test'\nkind = 'privacy'\nname = '测试'\n[[rule]]\nid = 'a'\nlabel = '中文'; bad = 1\n";
    // TOML syntax errors use the parser's byte span without interpreting its message.
    let error = RuleSet::parse(
        "unicode.toml",
        source,
        ParseContext {
            origin: RuleOrigin::User,
            references: &catalog(),
        },
    )
    .expect_err("syntax error");
    assert_eq!(error.diagnostics()[0].location.line, 8);
    let source = source.replace("label = '中文'; bad = 1", "label = '中文'\npriority = 1001\nbad = 1\nwhen = [{ field = 'app.nope', op = 'nope', value = '' }]\npreset = 'wrong'");
    let errors = RuleSet::parse(
        "unicode.toml",
        &source,
        ParseContext {
            origin: RuleOrigin::User,
            references: &catalog(),
        },
    )
    .expect_err("many independent errors")
    .into_diagnostics();
    for code in [
        DiagnosticCode::Key,
        DiagnosticCode::Field,
        DiagnosticCode::Op,
        DiagnosticCode::Value,
    ] {
        assert!(errors.iter().any(|d| d.code == code), "{errors:?}");
    }
    let unknown = errors
        .iter()
        .find(|d| d.path == "rule[0].bad")
        .expect("unknown key diagnostic");
    assert_eq!((unknown.location.line, unknown.location.column), (10, 1));
    assert_eq!(&source[unknown.location.bytes.clone()], "bad");
    let field = errors
        .iter()
        .find(|d| d.code == DiagnosticCode::Field)
        .expect("field diagnostic");
    assert_eq!(&source[field.location.bytes.clone()], "'app.nope'");
}
#[test]
fn specification_examples_validate_or_report_unavailable_capabilities() {
    let specification = include_str!("../../../../docs/rules/rule-format.md");
    let section = specification
        .split("## 5. 示例")
        .nth(1)
        .expect("examples section")
        .split("## 6. 版本")
        .next()
        .expect("section end");
    let mut count = 0;
    for block in section.split("```toml\n").skip(1) {
        let source = block.split("```").next().expect("example body");
        let parsed = RuleSet::parse(
            "spec.toml",
            source,
            ParseContext {
                origin: RuleOrigin::User,
                references: &catalog(),
            },
        );
        if source.contains("kind = \"precedent\"") {
            assert!(
                parsed
                    .expect_err("M7 requires a later milestone")
                    .diagnostics()
                    .iter()
                    .any(|d| d.code == DiagnosticCode::Set)
            );
        } else if source.contains("field = \"web.") {
            assert!(
                parsed
                    .expect_err("web classification requires a supplied field")
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.code == DiagnosticCode::Field)
            );
        } else {
            parsed.expect("supported specification example");
        }
        count += 1;
    }
    assert_eq!(count, 3);
    let policy = "format = 'quartz-rules/1'\n[set]\nkey = 'user.policy'\nname = 'Policy'\nkind = 'policy_check'\n";
    assert!(
        RuleSet::parse(
            "policy.toml",
            policy,
            ParseContext {
                origin: RuleOrigin::User,
                references: &catalog()
            }
        )
        .expect_err("unsupported M7")
        .diagnostics()
        .iter()
        .any(|d| d.code == DiagnosticCode::Set)
    );
}
#[test]
fn ancestors_only_and_sibling_conflicts() {
    let root = installed(privacy("user.root", None, &rule("one")));
    let child = installed(privacy(
        "user.child",
        Some("user.root"),
        &overlay("user.root/one"),
    ));
    let leaf = installed(privacy(
        "user.leaf",
        Some("user.child"),
        "[[disable]]\ntarget = 'user.root/one'\n",
    ));
    let resolved = Resolution::resolve(&[root.clone(), child.clone(), leaf], RuleKind::Privacy);
    assert!(
        resolved.diagnostics.is_empty(),
        "{:?}",
        resolved.diagnostics
    );
    assert!(resolved.effective.rules.is_empty());
    assert_eq!(resolved.effective.suppressed.len(), 2);
    let sibling = installed(privacy(
        "user.sibling",
        Some("user.root"),
        "[[disable]]\ntarget = 'user.root/one'\n",
    ));
    assert!(
        Resolution::resolve(&[root, child, sibling], RuleKind::Privacy)
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::Conflict)
    );
    let own = installed(privacy(
        "user.own",
        None,
        &(rule("one") + &overlay("user.own/one")),
    ));
    assert!(
        Resolution::resolve(&[own], RuleKind::Privacy)
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::Target)
    );
}
#[test]
fn cycles_missing_parents_and_mixed_kinds_are_reported() {
    let a = installed(privacy("user.a", Some("user.b"), &rule("a")));
    let b = installed(privacy("user.b", Some("user.a"), &rule("b")));
    assert_eq!(
        Resolution::resolve(&[a, b], RuleKind::Privacy)
            .diagnostics
            .iter()
            .filter(|d| d.code == DiagnosticCode::Parent)
            .count(),
        2
    );
    let mut parent = installed(privacy("user.parent", None, &rule("a")));
    parent.set.header.kind = RuleKind::Extraction;
    let child = installed(privacy("user.child", Some("user.parent"), &rule("b")));
    assert!(
        Resolution::resolve(&[parent, child], RuleKind::Privacy)
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::Set)
    );
}
#[test]
fn priority_depth_preorder_and_hash_reversion() {
    let mut root = installed(privacy(
        "user.root",
        None,
        &(rule("first")
            + &rule("high").replace("label = 'Hide'", "label = 'Hide'\npriority = 100")),
    ));
    let child = installed(privacy("user.child", Some("user.root"), &rule("child")));
    let mut sibling = installed(privacy("user.sibling", Some("user.root"), &rule("sibling")));
    sibling.sibling_order = 1;
    let sets = vec![root.clone(), child, sibling];
    let effective = Resolution::resolve(&sets, RuleKind::Privacy).effective;
    assert_eq!(
        effective
            .rules
            .iter()
            .map(|r| r.rule.id.as_str())
            .collect::<Vec<_>>(),
        ["high", "child", "sibling", "first"]
    );
    let hash = effective.version_hash.clone();
    let mut changed = sets.clone();
    changed[1].sibling_order = 2;
    assert_ne!(
        hash,
        Resolution::resolve(&changed, RuleKind::Privacy)
            .effective
            .version_hash
    );
    changed = sets.clone();
    changed[0].enabled = Enablement::Disabled;
    assert!(
        Resolution::resolve(&changed, RuleKind::Privacy)
            .effective
            .rules
            .is_empty()
    );
    assert_ne!(
        hash,
        Resolution::resolve(&changed, RuleKind::Privacy)
            .effective
            .version_hash
    );
    changed = sets.clone();
    changed[0].set.content.rule.swap(0, 1);
    assert_ne!(
        hash,
        Resolution::resolve(&changed, RuleKind::Privacy)
            .effective
            .version_hash
    );
    root.set.content.rule[0].label = Some("Edited".into());
    changed[0] = root;
    assert_ne!(
        hash,
        Resolution::resolve(&changed, RuleKind::Privacy)
            .effective
            .version_hash
    );
    assert_eq!(
        hash,
        Resolution::resolve(&sets, RuleKind::Privacy)
            .effective
            .version_hash
    );
}
#[test]
fn content_hash_ignores_toml_layout_and_normalizes_idn() {
    let source = include_str!("../../tests/fixtures/rules/value.toml")
        .replace("value = \"\"", "value = \"secret\"");
    let one = user(&source);
    let two = user(&format!(
        "# comment\n{}",
        source.replace("name = \"Fixture\"", "name='Fixture'")
    ));
    assert_eq!(one.content.content_hash(), two.content.content_hash());
    assert_eq!(one.content.content_hash().as_str().len(), 64);
}
fn evaluate_condition(
    field: &str,
    op: &str,
    value: &str,
    extra: &str,
    snapshot: &FieldSnapshot,
) -> Evaluation {
    let source = format!(
        "[[rule]]\nid='one'\nlabel='Hide'\nwhen=[{{field='{field}',op='{op}',value='{value}'{extra}}}]\npreset='title_hidden'\n"
    );
    Resolution::resolve(
        &[installed(privacy("user.test", None, &source))],
        RuleKind::Privacy,
    )
    .effective
    .compile()
    .expect("compile")
    .evaluate(snapshot)
}
#[test]
fn literal_operators_fold_multilingual_text_and_respect_case() {
    let snapshot = FieldSnapshot {
        text: BTreeMap::from([(Field::RawTitle, NfcText::new("ＶＳ Straße 中文 e\u{301}"))]),
        domain: None,
    };
    for (op, value) in [
        ("contains", "strasse"),
        ("starts_with", "vs"),
        ("ends_with", "É"),
        ("contains", "中文"),
        ("equals", "vs STRASSE 中文 é"),
    ] {
        assert_eq!(
            evaluate_condition("title.raw", op, value, "", &snapshot)
                .privacy
                .window_title,
            Action::Drop,
            "{op}: {value}"
        );
    }
    assert_eq!(
        evaluate_condition(
            "title.raw",
            "starts_with",
            "vs",
            ",case_sensitive=true",
            &snapshot
        )
        .privacy
        .window_title,
        Action::Keep
    );
    assert_eq!(
        evaluate_condition(
            "title.raw",
            "ends_with",
            "é",
            ",case_sensitive=true",
            &snapshot
        )
        .privacy
        .window_title,
        Action::Drop
    );
}
#[test]
fn regex_uses_nfc_and_its_own_case_flag() {
    let snapshot = FieldSnapshot {
        text: BTreeMap::from([(Field::RawTitle, NfcText::new("CAFÉ"))]),
        domain: None,
    };
    assert_eq!(
        evaluate_condition("title.raw", "regex", "café", "", &snapshot)
            .privacy
            .window_title,
        Action::Keep
    );
    assert_eq!(
        evaluate_condition(
            "title.raw",
            "regex",
            "(?i)café",
            ",case_sensitive=true",
            &snapshot
        )
        .privacy
        .window_title,
        Action::Drop
    );
}
#[test]
fn domains_are_idna_normalized_and_suffixes_have_label_boundaries() {
    let exact = FieldSnapshot {
        text: BTreeMap::new(),
        domain: Some(DomainName::parse("例子.测试").expect("valid IDN")),
    };
    for op in ["equals", "contains", "starts_with", "ends_with"] {
        assert_eq!(
            evaluate_condition("web.domain", op, "例子.测试", "", &exact)
                .privacy
                .window_title,
            Action::Drop,
            "{op}"
        );
    }
    for (domain, action) in [
        ("www.xn--fsqu00a.xn--0zwm56d", Action::Drop),
        ("not例子.测试", Action::Keep),
    ] {
        let snapshot = FieldSnapshot {
            text: BTreeMap::new(),
            domain: Some(DomainName::parse(domain).expect("valid IDN")),
        };
        assert_eq!(
            evaluate_condition("web.domain", "domain_suffix", "例子.测试", "", &snapshot)
                .privacy
                .window_title,
            action
        );
    }
}
#[test]
fn missing_fields_negation_and_match_any() {
    let snapshot = FieldSnapshot::default();
    assert_eq!(
        evaluate_condition("title.raw", "contains", "x", "", &snapshot)
            .privacy
            .window_title,
        Action::Keep
    );
    let output = evaluate_condition("title.raw", "contains", "x", ",negate=true", &snapshot);
    assert_eq!(output.privacy.window_title, Action::Drop);
    assert!(!output.rules[0].conditions[0].present);
}
#[test]
fn regex_syntax_memory_and_utf16_input_limits() {
    for pattern in [r"(?=x)", r"(x)\1", r"\w{10000}"] {
        let source = format!(
            "[[rule]]\nid='one'\nlabel='Hide'\nwhen=[{{field='title.raw',op='regex',value='{pattern}'}}]\npreset='title_hidden'"
        );
        let source = format!(
            "format='quartz-rules/1'\n[set]\nkey='user.test'\nkind='privacy'\nname='Test'\n{source}"
        );
        assert!(
            RuleSet::parse(
                "regex.toml",
                &source,
                ParseContext {
                    origin: RuleOrigin::User,
                    references: &catalog()
                }
            )
            .expect_err("unsafe expression")
            .diagnostics()
            .iter()
            .any(|d| d.code == DiagnosticCode::Regex),
            "{pattern}"
        );
    }
    let snapshot = FieldSnapshot {
        text: BTreeMap::from([(
            Field::RawTitle,
            NfcText::new(&format!("{}secret", "😀".repeat(2048))),
        )]),
        domain: None,
    };
    assert_eq!(
        evaluate_condition("title.raw", "contains", "secret", "", &snapshot)
            .privacy
            .window_title,
        Action::Keep
    );
    assert_eq!(
        evaluate_condition("title.raw", "regex", "secret", "", &snapshot)
            .privacy
            .window_title,
        Action::Keep
    );
}
#[test]
fn extraction_and_shadowed_classification_have_structured_explanations() {
    let source = "format='quartz-rules/1'\n[set]\nkey='user.extract'\nkind='extraction'\nname='Extract'\n[[rule]]\nid='code'\nlabel='Code title'\napp=[{field='app.exe_name',op='equals',value='code.exe'}]\npattern='^(?P<dirty>● )?(?P<document>.+?) - (?P<project>.+?) - Visual Studio Code$'\n";
    let snapshot = FieldSnapshot {
        text: BTreeMap::from([
            (Field::ExeName, NfcText::new("CODE.EXE")),
            (
                Field::RawTitle,
                NfcText::new("● main.rs - Quartz - Visual Studio Code"),
            ),
        ]),
        domain: None,
    };
    let extracted = Resolution::resolve(&[installed(user(source))], RuleKind::Extraction)
        .effective
        .compile()
        .expect("compile")
        .evaluate(&snapshot);
    assert_eq!(
        extracted.extraction.expect("captured fields"),
        Extraction {
            document: Some("main.rs".into()),
            project: Some("Quartz".into()),
            profile: None,
            dirty: true,
            normalized_title: Some("main.rs - Quartz".into()),
            status: ExtractionStatus::Extracted,
        }
    );
    let source = include_str!("../../tests/fixtures/rules/ref.toml")
        .replace("Missing category", "builtin:development");
    let source = format!(
        "{source}{}",
        source
            .split("[[rule]]")
            .nth(1)
            .map(|rule| format!("[[rule]]{}", rule.replace("id = \"one\"", "id = \"two\"")))
            .expect("rule")
    );
    let snapshot = FieldSnapshot {
        text: BTreeMap::from([(Field::RawTitle, NfcText::new("secret"))]),
        domain: None,
    };
    let output = Resolution::resolve(&[installed(user(&source))], RuleKind::Classification)
        .effective
        .compile()
        .expect("compile")
        .evaluate(&snapshot);
    assert_eq!(output.category_uuid.as_deref(), Some("uuid-development"));
    assert!(matches!(
        output.rules[1].status,
        RuleStatus::Shadowed {
            reason: ShadowReason::Order,
            ..
        }
    ));
}

#[test]
fn privacy_merges_strict_actions_and_explains_field_sources() {
    let excluded = PrivacyAction {
        app: Action::Drop,
        window_title: Action::Keep,
        web: WebAction::Full,
    };
    assert_eq!(
        PrivacyAction::default().merge(excluded),
        PrivacyAction {
            app: Action::Drop,
            window_title: Action::Drop,
            web: WebAction::None
        }
    );
    let mut set = privacy("user.test", None, &rule("one"));
    set.content.rule[0].body = RuleBody::Privacy {
        action: excluded,
        mode: MatchMode::All,
        when: vec![Condition {
            field: Field::RawTitle,
            op: Operator::Contains,
            value: "secret".into(),
            case: CaseSensitivity::Folded,
            negate: false,
        }],
    };
    let snapshot = FieldSnapshot {
        text: BTreeMap::from([(Field::RawTitle, NfcText::new("secret"))]),
        domain: None,
    };
    let result = Resolution::resolve(&[installed(set)], RuleKind::Privacy)
        .effective
        .compile()
        .expect("compile")
        .evaluate(&snapshot);
    assert_eq!(result.privacy.web, WebAction::None);
    assert_eq!(
        result.privacy_sources.app.as_deref(),
        Some("user.test/one@1")
    );
    assert_eq!(result.privacy_sources.web, result.privacy_sources.app);
    let snapshot = FieldSnapshot {
        domain: Some(DomainName::parse("bank.example").expect("valid domain")),
        text: BTreeMap::new(),
    };
    assert_eq!(
        evaluate_condition("web.domain", "equals", "bank.example", "", &snapshot)
            .privacy
            .web,
        WebAction::DomainOnly
    );
}
#[test]
fn interleaved_rule_and_override_entries_preserve_source_order() {
    let root = installed(privacy("user.root", None, &rule("one")));
    let first = installed(privacy(
        "user.child",
        Some("user.root"),
        &(overlay("user.root/one") + &rule("two")),
    ));
    let second = installed(privacy(
        "user.child",
        Some("user.root"),
        &(rule("two") + &overlay("user.root/one")),
    ));
    let a = Resolution::resolve(&[root.clone(), first], RuleKind::Privacy).effective;
    let b = Resolution::resolve(&[root, second], RuleKind::Privacy).effective;
    assert_eq!(a.rules[0].rule.id, "one");
    assert_eq!(b.rules[0].rule.id, "two");
    assert_ne!(a.version_hash, b.version_hash);
}

#[test]
fn vocabulary_agrees_with_the_published_format() {
    let spec = include_str!("../../../../docs/rules/rule-format.md");
    for field in Field::ALL {
        assert!(
            spec.contains(&format!("`{}`", field.as_str())),
            "missing documented field {field:?}"
        );
    }
    for op in Operator::ALL {
        assert!(
            spec.contains(&format!("`{}`", op.as_str())),
            "missing documented operator {op:?}"
        );
    }
    for kind in RuleKind::ALL {
        assert!(
            spec.contains(kind.as_str()),
            "missing documented kind {kind:?}"
        );
    }
}
#[test]
fn condition_count_and_utf16_value_lengths_are_validated() {
    let condition = "{field='title.raw',op='contains',value='x'}";
    for when in [
        String::new(),
        std::iter::repeat_n(condition, 33)
            .collect::<Vec<_>>()
            .join(","),
    ] {
        let source = format!(
            "format='quartz-rules/1'\n[set]\nkey='user.test'\nkind='privacy'\nname='Test'\n[[rule]]\nid='one'\nlabel='Test'\npreset='title_hidden'\nwhen=[{when}]"
        );
        assert!(
            RuleSet::parse(
                "count.toml",
                &source,
                ParseContext {
                    origin: RuleOrigin::User,
                    references: &catalog()
                }
            )
            .expect_err("invalid count")
            .diagnostics()
            .iter()
            .any(|d| d.code == DiagnosticCode::Value)
        );
    }
    let source = include_str!("../../tests/fixtures/rules/value.toml")
        .replace("value = \"\"", &format!("value = '{}'", "😀".repeat(2049)));
    assert!(
        RuleSet::parse(
            "long.toml",
            &source,
            ParseContext {
                origin: RuleOrigin::User,
                references: &catalog()
            }
        )
        .expect_err("UTF-16 limit")
        .diagnostics()
        .iter()
        .any(|d| d.code == DiagnosticCode::Value)
    );
}
#[test]
fn any_mode_combines_conditions_and_present_negation_works() {
    let snapshot = FieldSnapshot {
        text: BTreeMap::from([(Field::RawTitle, NfcText::new("public"))]),
        domain: None,
    };
    let output = evaluate_condition("title.raw", "contains", "secret", ",negate=true", &snapshot);
    assert_eq!(output.privacy.window_title, Action::Drop);
    let source = rule("one").replace(
        "when = [",
        "match = 'any'\nwhen = [{ field = 'app.exe_name', op = 'equals', value = 'code.exe' }, ",
    );
    let snapshot = FieldSnapshot {
        text: BTreeMap::from([(Field::RawTitle, NfcText::new("secret"))]),
        domain: None,
    };
    let output = Resolution::resolve(
        &[installed(privacy("user.any", None, &source))],
        RuleKind::Privacy,
    )
    .effective
    .compile()
    .expect("compile any rule")
    .evaluate(&snapshot);
    assert_eq!(output.privacy.window_title, Action::Drop);
    assert_eq!(output.rules[0].conditions.len(), 2);
}

#[test]
fn extraction_fallback_only_strips_the_app_suffix() {
    let compiled = Resolution::resolve(&[], RuleKind::Extraction)
        .effective
        .compile()
        .expect("empty extractor");
    let snapshot = FieldSnapshot {
        text: BTreeMap::from([
            (Field::RawTitle, NfcText::new("Report (2026) - Editor")),
            (Field::DisplayName, NfcText::new("Editor")),
        ]),
        domain: None,
    };
    let output = compiled
        .evaluate(&snapshot)
        .extraction
        .expect("generic fallback");
    assert_eq!(output.document.as_deref(), Some("Report (2026)"));
    assert_eq!(output.normalized_title, output.document);
    assert_eq!(output.status, ExtractionStatus::Fallback);
    assert_eq!(
        compiled
            .evaluate(&FieldSnapshot::default())
            .extraction
            .expect("unknown input")
            .status,
        ExtractionStatus::Unknown
    );
    let snapshot = FieldSnapshot {
        text: BTreeMap::from([(Field::RawTitle, NfcText::new(""))]),
        domain: None,
    };
    assert_eq!(
        compiled
            .evaluate(&snapshot)
            .extraction
            .expect("empty input")
            .status,
        ExtractionStatus::Empty
    );
}
#[test]
fn privacy_label_sources_prefer_users_independent_of_priority() {
    let mut builtin = privacy("user.builtin", None, &rule("one"));
    builtin.header.key = "builtin.test".into();
    builtin.content.rule[0].priority = 1000;
    builtin.content.rule[0].builtin = Some(BuiltinMetadata {
        detector: None,
        languages: Vec::new(),
        verified: None,
        label_key: Some("privacy.label.private_window".into()),
    });
    let user = privacy("user.preference", None, &rule("one"));
    let snapshot = FieldSnapshot {
        text: BTreeMap::from([(Field::RawTitle, NfcText::new("secret"))]),
        domain: None,
    };
    let output = Resolution::resolve(&[installed(builtin), installed(user)], RuleKind::Privacy)
        .effective
        .compile()
        .expect("compile privacy")
        .evaluate(&snapshot);
    assert_eq!(
        output.privacy_sources.window_title.as_deref(),
        Some("user.preference/one@1")
    );
}

#[test]
fn malformed_overlay_targets_have_target_diagnostics_at_the_target_value() {
    for section in ["override", "disable"] {
        let body = if section == "override" {
            overlay("malformed")
        } else {
            "[[disable]]\ntarget='malformed'\n".into()
        };
        let source = format!(
            "format='quartz-rules/1'\n[set]\nkey='user.test'\nkind='privacy'\nname='Test'\n{body}"
        );
        let errors = RuleSet::parse(
            "target.toml",
            &source,
            ParseContext {
                origin: RuleOrigin::User,
                references: &catalog(),
            },
        )
        .expect_err("malformed target")
        .into_diagnostics();
        let target = errors
            .iter()
            .find(|d| d.code == DiagnosticCode::Target)
            .expect("target diagnostic");
        assert_eq!(&source[target.location.bytes.clone()], "'malformed'");
        assert!(!errors.iter().any(|d| d.path.ends_with(".id")));
    }
}

#[test]
fn parse_errors_preserve_nonempty_diagnostics_and_implement_standard_error() {
    fn assert_error<T: std::error::Error + Send + Sync + 'static>() {}
    assert_error::<ParseRuleSetError>();

    for source in [
        "[set",
        include_str!("../../tests/fixtures/rules/field.toml"),
    ] {
        let error = RuleSet::parse(
            "invalid.toml",
            source,
            ParseContext {
                origin: RuleOrigin::User,
                references: &catalog(),
            },
        )
        .expect_err("invalid syntax or field");
        let diagnostics = error.diagnostics();
        assert!(!diagnostics.is_empty());
        assert_eq!(
            error.to_string(),
            format!("rule set has {} validation errors", diagnostics.len())
        );
        assert_eq!(
            diagnostics[0].location.file,
            std::path::Path::new("invalid.toml")
        );
        let expected = diagnostics.to_vec();
        assert_eq!(error.into_diagnostics(), expected);
    }
}

#[test]
fn extraction_without_named_spans_preserves_only_the_available_title() {
    let source = "format='quartz-rules/1'\n[set]\nkey='user.extract'\nkind='extraction'\nname='Extract'\n[[rule]]\nid='title'\nlabel='Title'\napp=[{field='app.exe_name',op='equals',value='code.exe'}]\npattern='^(?P<dirty>● )?Report$'\n";
    let compiled = Resolution::resolve(&[installed(user(source))], RuleKind::Extraction)
        .effective
        .compile()
        .expect("compile extractor");
    let mut snapshot = FieldSnapshot {
        text: BTreeMap::from([
            (Field::ExeName, NfcText::new("code.exe")),
            (Field::RawTitle, NfcText::new("● Report")),
        ]),
        domain: None,
    };
    let extracted = compiled
        .evaluate(&snapshot)
        .extraction
        .expect("matched title");
    assert_eq!(extracted.normalized_title.as_deref(), Some("● Report"));
    assert!(extracted.dirty);
    assert_eq!(extracted.document, None);
    snapshot.text.remove(&Field::RawTitle);
    let hidden = compiled
        .evaluate(&snapshot)
        .extraction
        .expect("unknown title");
    assert_eq!(hidden.normalized_title, None);
    assert_eq!(hidden.status, ExtractionStatus::Unknown);
}

#[test]
fn override_references_keep_array_wire_format_and_validate_set_keys() {
    let reference = OverrideRef {
        set: "user.overlay".parse().expect("rule-set key"),
        version: RuleVersion::new(3),
    };
    let serialized = serde_json::to_string(&reference).expect("serialize override reference");
    assert_eq!(serialized, r#"["user.overlay",3]"#);
    assert_eq!(
        serde_json::from_str::<OverrideRef>(&serialized).expect("decode reference"),
        reference
    );
    assert_eq!(reference.version.get(), 3);
    for invalid in [
        r#"["INVALID",3]"#,
        r#"["user.overlay"]"#,
        r#"["user.overlay",3,4]"#,
    ] {
        assert!(
            serde_json::from_str::<OverrideRef>(invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn required_field_diagnostics_preserve_messages_order_and_locations() {
    let mut sources = vec![
        "format='quartz-rules/1'".to_owned(),
        "format='quartz-rules/1'\n[set]".to_owned(),
        "format='quartz-rules/1'\n[set]\nkey=3\nkind=false\nname=[]".to_owned(),
        "format='quartz-rules/1'\n[set]\nkey=''\nkind=''\nname=''".to_owned(),
    ];
    for kind in [
        "classification",
        "extraction",
        "privacy",
        "precedent",
        "unknown",
    ] {
        let header = format!(
            "format='quartz-rules/1'\n[set]\nkey='user.invalid'\nkind='{kind}'\nname='Invalid'\n"
        );
        for entries in [
            "[[rule]]\nlabel='Missing fields'\n",
            "[[rule]]\nid=3\ncategory=false\npattern=3\npriority=70000\n",
            "[[rule]]\nid=''\ncategory=''\npattern='['\npriority=-1\n",
            "[[rule]]\nlabel='First'\n[[rule]]\nlabel='Second'\n[[rule]]\nid=''\nlabel='Empty'\n",
            "[[override]]\nlabel='Missing target'\n",
            "[[override]]\ntarget='invalid'\nlabel='Malformed target'\n",
            "[[disable]]\nreason=3\n[[disable]]\ntarget=3\nreason=3\n",
        ] {
            sources.push(format!("{header}{entries}"));
        }
    }
    let references = catalog();
    let diagnostics: Vec<_> = sources
        .iter()
        .map(|source| {
            RuleSet::parse(
                "required-fields.toml",
                source,
                ParseContext {
                    origin: RuleOrigin::User,
                    references: &references,
                },
            )
            .expect_err("incomplete source")
            .into_diagnostics()
        })
        .collect();
    let serialized = serde_json::to_string(&diagnostics).expect("diagnostic serialization");
    assert_eq!(
        ContentHash::of(&serialized).as_str(),
        "df2dc9ff90d1c59ec39ca9a005fa48817197f443433ba935166a9b6e81f4c23f",
        "required-field diagnostic messages, order, and source locations changed: {diagnostics:#?}"
    );
}

#[test]
fn builtin_classification_validates_and_matches_only_current_fields_and_known_categories() {
    let set = RuleSet::parse(
        "builtin-classification.toml",
        BUILTIN_CLASSIFICATION,
        ParseContext {
            origin: RuleOrigin::Builtin,
            references: &catalog(),
        },
    )
    .expect("valid embedded classification");
    assert_eq!(set.content.rule.len(), 50);
    for rule in &set.content.rule {
        let RuleBody::Classification { category, when, .. } = &rule.body else {
            panic!("classification body");
        };
        assert!(
            catalog()
                .categories
                .values()
                .any(|uuid| uuid == &category.uuid.0)
        );
        assert!(when.iter().all(|condition| matches!(
            condition.field,
            Field::IdentityKey | Field::ExeName | Field::DisplayName | Field::RawTitle
        )));
    }
    let effective = Resolution::resolve(&[installed(set)], RuleKind::Classification)
        .effective
        .compile()
        .expect("compiled classification");
    for exe in [
        "code.exe",
        "CODE.EXE",
        "ＷＩＮＷＯＲＤ.exe",
        "msedge.exe",
        "unknown.exe",
    ] {
        let fields = FieldSnapshot {
            text: BTreeMap::from([(Field::ExeName, NfcText::new(exe))]),
            domain: None,
        };
        let fast = effective.classify(&fields);
        let explained = effective.evaluate(&fields);
        assert_eq!(
            fast.as_ref().map(|matched| matched.rule.as_str()),
            explained.selected.as_deref(),
            "{exe}"
        );
        assert_eq!(
            fast.as_ref().map(|matched| matched.category.0.as_str()),
            explained.category_uuid.as_deref(),
            "{exe}"
        );
    }
    for field in [
        "app.name",
        "window.class",
        "title.normalized",
        "doc.document",
        "web.domain",
        "browser.profile_label",
    ] {
        let source = format!(
            "format='quartz-rules/1'\n[set]\nkey='user.future'\nkind='classification'\nname='Future'\n[[rule]]\nid='future'\nlabel='Future'\ncategory='builtin:learning'\nwhen=[{{field='{field}',op='equals',value='example'}}]"
        );
        let error = RuleSet::parse(
            "future.toml",
            &source,
            ParseContext {
                origin: RuleOrigin::User,
                references: &catalog(),
            },
        )
        .expect_err("unavailable field");
        assert!(
            error
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::Field),
            "{field}"
        );
    }
}

#[test]
fn first_match_classification_preserves_explained_literal_regex_and_missing_field_semantics() {
    let values = [
        None,
        Some("CAFÉ"),
        Some("Ｃａｆｅ\u{301} notes"),
        Some("other"),
    ];
    for operator in [
        Operator::Equals,
        Operator::Contains,
        Operator::StartsWith,
        Operator::EndsWith,
        Operator::Regex,
    ] {
        for case in [CaseSensitivity::Sensitive, CaseSensitivity::Folded] {
            for negate in [false, true] {
                let set = user(&format!(
                    r#"format='quartz-rules/1'
[set]
key='user.comparison'
kind='classification'
name='Comparison'
[[rule]]
id='comparison'
label='Compare a title'
category='builtin:learning'
when=[{{field='title.raw',op='{}',value='Café',case_sensitive={},negate={}}}]
"#,
                    operator.as_str(),
                    case == CaseSensitivity::Sensitive,
                    negate
                ));
                let compiled = Resolution::resolve(&[installed(set)], RuleKind::Classification)
                    .effective
                    .compile()
                    .expect("compiled rule");
                for value in values {
                    let mut fields = FieldSnapshot::default();
                    if let Some(value) = value {
                        fields.text.insert(Field::RawTitle, NfcText::new(value));
                    }
                    let fast = compiled.classify(&fields);
                    let explained = compiled.evaluate(&fields);
                    assert_eq!(
                        fast.as_ref().map(|matched| matched.rule.as_str()),
                        explained.selected.as_deref(),
                        "{operator:?} {case:?} negate={negate} value={value:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn matching_bounds_normalized_text_at_utf16_character_boundaries() {
    let prefix = "x".repeat(super::model::MAX_TEXT_UTF16 - 1);
    let snapshot = FieldSnapshot {
        text: BTreeMap::from([(Field::RawTitle, NfcText::new(&format!("{prefix}é😀secret")))]),
        domain: None,
    };
    for operator in ["ends_with", "regex"] {
        let pattern = if operator == "regex" { "é$" } else { "é" };
        let result = evaluate_condition(
            "title.raw",
            operator,
            pattern,
            ",case_sensitive=true",
            &snapshot,
        );
        assert_eq!(result.privacy.window_title, Action::Drop, "{operator}");
    }
    let result = evaluate_condition("title.raw", "contains", "secret", "", &snapshot);
    assert_eq!(result.privacy.window_title, Action::Keep);
}
