use super::*;
use crate::rules::{
    Field, FieldSnapshot,
    registry::{RegistryCommand, RegistryState, RuleRegistry},
};
use crate::text::NfcText;
use std::time::Duration;
use std::{collections::BTreeSet, sync::Arc};

fn utc(ms: i64) -> UtcInstant {
    UtcInstant::try_from(ms).expect("test UTC")
}
fn mono(ms: u64) -> MonoInstant {
    MonoInstant::from_millis(ms)
}
fn at(ms: u64) -> Timestamp {
    Timestamp {
        utc: utc(i64::try_from(ms).expect("test time")),
        mono: mono(ms),
    }
}
fn window(handle: u64, title: &str) -> Foreground {
    Foreground::Window {
        facts: Box::new(WindowFacts {
            handle: WindowHandle::new(handle),
            process: ProcessId::new(7),
            process_created_utc: Some(utc(0)),
            class: "TestWindow".into(),
            app: AppIdentity::Exe("c:\\app.exe".into()),
            exe_name: Some("app.exe".into()),
            display_name: "App".into(),
            title: Title::readable(title),
        }),
        privacy: PrivacyContext::default(),
    }
}
fn reading(generation: u64, source: ObservationSource, foreground: Foreground) -> CaptureEvent {
    CaptureEvent::Foreground {
        generation: ForegroundGeneration::new(generation),
        source,
        foreground,
    }
}

struct Fixture {
    core: CaptureCore,
    rules: RuleRegistry,
    writes: Vec<CaptureWrite>,
    now: Timestamp,
}
impl Fixture {
    fn new(start: Timestamp) -> Self {
        let StartedCapture { core, step } =
            CaptureCore::start(RunId::new(1), start).expect("valid capture startup");
        Self {
            core,
            rules: RuleRegistry::from_state(RegistryState::default()).expect("empty registry"),
            writes: step.writes,
            now: start,
        }
    }
    fn active() -> Self {
        let mut fixture = Self::new(at(0));
        let _ = fixture.send(
            at(0),
            CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
        );
        let _ = fixture.review(at(0), 1, window(1, "initial"));
        fixture
    }
    fn send(&mut self, now: Timestamp, event: CaptureEvent) -> CaptureStep {
        self.now = now;
        let step = self
            .core
            .step(CaptureInput {
                at: now,
                rules: self.rules.snapshots().load(),
                event,
            })
            .expect("valid capture event");
        self.writes.extend(step.writes.clone());
        step
    }
    fn review(&mut self, now: Timestamp, generation: u64, foreground: Foreground) -> CaptureStep {
        self.send(
            now,
            reading(generation, ObservationSource::Review, foreground),
        )
    }
    fn hook(&mut self, now: Timestamp, generation: u64, foreground: Foreground) -> CaptureStep {
        self.send(
            now,
            reading(generation, ObservationSource::Hook, foreground),
        )
    }
    fn policy(&mut self, text: &str, version: u32) {
        let decision = self.rules.decide(RegistryCommand::Apply {
            key: "user.capture".parse().expect("key"),
            file: "capture-test.toml".into(),
            text: text.into(),
            base_version: version,
        });
        let (_, reply, outcome) = decision.into_parts();
        reply.expect("valid rule input");
        self.rules
            .committed(outcome.expect("commit token"))
            .expect("sequential rule commit");
    }
    fn finish(mut self, now: Timestamp) -> Vec<CaptureWrite> {
        let _ = self.send(now, CaptureEvent::Shutdown);
        assert_conservation(&self.writes);
        self.writes
    }
}

fn rule(preset: &str, condition: &str) -> String {
    format!(
        "format='quartz-rules/1'\n[set]\nkey='user.capture'\nkind='privacy'\nname='Capture tests'\n[[rule]]\nid='hide'\nlabel='Sensitive'\npreset='{preset}'\nwhen=[{condition}]\n"
    )
}
fn interval_opens(step: &CaptureStep) -> Vec<&OpenInterval> {
    step.writes
        .iter()
        .filter_map(|write| match write {
            CaptureWrite::OpenInterval(row) => Some(row),
            _ => None,
        })
        .collect()
}
fn fields(step: &CaptureStep) -> &CapturedFields {
    step.writes
        .iter()
        .rev()
        .find_map(|write| match write {
            CaptureWrite::OpenInterval(OpenInterval {
                fact: IntervalFact::Window { fields, .. },
                ..
            }) => Some(fields),
            _ => None,
        })
        .expect("new window interval")
}

use super::test_support::assert_conservation;

#[test]
fn rapid_switch_identical_title_windows_and_tab_changes_keep_every_boundary() {
    let mut fixture = Fixture::active();
    for (time, generation, handle, title) in [
        (50, 2, 2, "initial"),
        (100, 3, 1, "initial"),
        (150, 3, 1, "tab 2"),
    ] {
        let step = fixture.hook(at(time), generation, window(handle, title));
        let close = step
            .writes
            .iter()
            .position(|write| matches!(write, CaptureWrite::CloseInterval { .. }))
            .expect("close");
        assert!(
            matches!(step.writes[close + 1], CaptureWrite::OpenInterval(_)),
            "close/open pair must be adjacent"
        );
        assert_eq!(interval_opens(&step).len(), 1);
    }
    let dedup = fixture.hook(at(160), 3, window(1, "tab 2"));
    assert!(interval_opens(&dedup).is_empty());
    let stale = fixture.hook(at(100), 2, window(2, "stale"));
    assert!(stale.writes.is_empty() && stale.effects.is_empty());
    fixture.finish(at(200));
}

#[test]
fn title_frequency_and_storm_change_batch_hint_without_discarding_rows() {
    let mut fixture = Fixture::active();
    for index in 1..=12 {
        let step = fixture.hook(at(index * 1_000), 1, window(1, &format!("second {index}")));
        assert_eq!(step.title_storm, TitleActivity::Ordinary);
        assert_eq!(interval_opens(&step).len(), 1);
    }
    let mut storms = 0;
    for index in 1..=60 {
        let step = fixture.hook(
            at(12_000 + index * 100),
            1,
            window(1, &format!("storm {index}")),
        );
        assert_eq!(interval_opens(&step).len(), 1);
        storms += usize::from(step.title_storm == TitleActivity::Storm);
    }
    assert!(storms > 0);
    fixture.finish(at(19_000));
}

#[test]
fn secure_desktop_and_unreadable_titles_remain_explicit_facts() {
    let mut fixture = Fixture::active();
    let step = fixture.review(
        at(10),
        2,
        Foreground::None(NoForegroundReason::UnknownDesktop),
    );
    assert_eq!(
        interval_opens(&step)[0].fact,
        IntervalFact::NoForeground(NoForegroundReason::UnknownDesktop)
    );
    let mut foreground = window(1, "ignored");
    if let Foreground::Window { facts, .. } = &mut foreground {
        facts.title = Title::unreadable();
        facts.app = AppIdentity::Aumid("Package!App".into());
    }
    let step = fixture.review(at(20), 3, foreground);
    assert_eq!(fields(&step).title_status, TitleStatus::Unreadable);
    assert!(fields(&step).title.is_none());
    fixture.finish(at(30));
}

#[test]
fn stop_reason_union_requires_review_and_preserves_overlaps() {
    for reasons in [
        [StopReason::Paused, StopReason::Locked],
        [StopReason::Suspended, StopReason::Disconnected],
        [StopReason::Locked, StopReason::Disconnected],
    ] {
        let mut fixture = Fixture::active();
        let _ = fixture.send(at(10), CaptureEvent::Stop(StopChange::Begin(reasons[0])));
        let _ = fixture.send(at(20), CaptureEvent::Stop(StopChange::Begin(reasons[1])));
        let _ = fixture.send(at(30), CaptureEvent::Stop(StopChange::End(reasons[0])));
        let ignored = fixture.review(at(35), 2, window(2, "while stopped"));
        assert!(interval_opens(&ignored).is_empty());
        let _ = fixture.send(at(40), CaptureEvent::Stop(StopChange::End(reasons[1])));
        let ignored = fixture.hook(at(45), 3, window(2, "requires review"));
        assert!(interval_opens(&ignored).is_empty());
        let _ = fixture.review(at(50), 3, window(2, "reviewed"));
        fixture.finish(at(60));
    }
}

#[test]
fn startup_waits_for_session_query_and_registration_retry() {
    for reason in [StopReason::Locked, StopReason::Disconnected] {
        let mut fixture = Fixture::new(at(0));
        let _ = fixture.send(at(1), CaptureEvent::Session(SessionStatus::Unavailable));
        let early = fixture.review(at(2), 1, window(1, "early"));
        assert!(interval_opens(&early).is_empty());
        assert!(matches!(
            fixture.core.state(),
            CaptureState::Stopped {
                session: SessionReadiness::AwaitingSession,
                ..
            }
        ));
        let _ = fixture.send(
            at(3),
            CaptureEvent::Session(SessionStatus::Ready(BTreeSet::from([reason]))),
        );
        let locked = fixture.review(at(4), 2, window(1, "locked"));
        assert!(interval_opens(&locked).is_empty());
        let _ = fixture.send(at(10), CaptureEvent::Stop(StopChange::End(reason)));
        let _ = fixture.review(at(11), 3, window(1, "ready"));
        fixture.finish(at(20));
    }
}

#[test]
fn window_retirement_class_change_and_process_reuse_allocate_new_generations() {
    let mut instances = WindowInstances::default();
    let Foreground::Window { mut facts, .. } = window(1, "same") else {
        panic!("window fixture")
    };
    let first = instances.observe(&facts, at(0));
    assert_eq!(instances.observe(&facts, at(1)).id, first.id);
    instances.retire(first.id);
    let second = instances.observe(&facts, at(2));
    assert_eq!(second.generation.get(), 1);
    assert_ne!(second.id, first.id);
    instances.retire(first.id);
    assert_eq!(instances.observe(&facts, at(3)).id, second.id);
    facts.class = "OtherClass".into();
    let third = instances.observe(&facts, at(4));
    assert_eq!(third.generation.get(), 2);
    facts.process_created_utc = None;
    let fourth = instances.observe(&facts, at(5));
    assert_eq!(fourth.generation.get(), 3);
    facts.handle = WindowHandle::new(2);
    assert_eq!(instances.observe(&facts, at(6)).generation.get(), 0);
}

#[test]
fn clock_rollback_projects_old_end_and_reopens_stop_masks() {
    let mut fixture = Fixture::active();
    let _ = fixture.send(
        at(100),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
    );
    let jump = Timestamp {
        utc: utc(-5_000),
        mono: mono(200),
    };
    let step = fixture.send(jump, CaptureEvent::ClockChanged(ClockChange::TimeChange));
    assert!(step.writes.iter().any(|write| matches!(
        write,
        CaptureWrite::CloseClockSegment {
            at: point,
            ..
        } if *point == at(200)
    )));
    assert!(step.writes.iter().any(|write| matches!(write, CaptureWrite::OpenAvailability(OpenAvailability { segment, start, .. }) if segment.get() == 2 && *start == jump)));
    let _ = fixture.send(
        Timestamp {
            utc: utc(-4_900),
            mono: mono(300),
        },
        CaptureEvent::Stop(StopChange::End(StopReason::Locked)),
    );
    let _ = fixture.review(
        Timestamp {
            utc: utc(-4_800),
            mono: mono(400),
        },
        2,
        window(2, "after rollback"),
    );
    fixture.finish(Timestamp {
        utc: utc(-4_700),
        mono: mono(500),
    });
}

#[test]
fn reviews_detect_strictly_greater_than_two_second_drift() {
    let mut fixture = Fixture::active();
    let tolerated = fixture.review(
        Timestamp {
            utc: utc(2_100),
            mono: mono(100),
        },
        1,
        window(1, "initial"),
    );
    assert!(
        !tolerated
            .writes
            .iter()
            .any(|write| matches!(write, CaptureWrite::CloseClockSegment { .. }))
    );
    let drift = fixture.review(
        Timestamp {
            utc: utc(2_201),
            mono: mono(200),
        },
        1,
        window(1, "initial"),
    );
    assert!(drift.writes.iter().any(|write| matches!(
        write,
        CaptureWrite::CloseClockSegment {
            reason: ClockEndReason::Drift,
            ..
        }
    )));
    fixture.finish(Timestamp {
        utc: utc(2_301),
        mono: mono(300),
    });
}

#[test]
fn midnight_splits_foreground_session_and_long_stop_rows() {
    let start = Timestamp {
        utc: utc(86_400_000 - 100),
        mono: mono(0),
    };
    let mut fixture = Fixture::new(start);
    let _ = fixture.send(
        start,
        CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
    );
    let _ = fixture.review(start, 1, window(1, "midnight"));
    let step = fixture.send(
        start.advance(Duration::from_millis(200)),
        CaptureEvent::Wake,
    );
    let new = interval_opens(&step)[0];
    assert_eq!(new.start.utc, utc(86_400_000));
    let previous = fixture
        .writes
        .iter()
        .filter_map(|write| match write {
            CaptureWrite::OpenInterval(row) if matches!(row.fact, IntervalFact::Window { .. }) => {
                Some(row.session)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(previous[0], previous[1]);
    let _ = fixture.send(
        start.advance(Duration::from_millis(300)),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
    );
    let _ = fixture.send(
        start.advance(Duration::from_millis(3 * 86_400_000_u64)),
        CaptureEvent::Stop(StopChange::End(StopReason::Locked)),
    );
    let _ = fixture.review(
        start.advance(Duration::from_millis(3 * 86_400_000_u64 + 1)),
        2,
        window(1, "back"),
    );
    fixture.finish(start.advance(Duration::from_millis(3 * 86_400_000_u64 + 100)));
    let pieces = TimeRange {
        start: Timestamp {
            utc: utc(-100),
            mono: mono(0),
        },
        end: Timestamp {
            utc: utc(86_400_000 + 100),
            mono: mono(86_400_000_u64 + 200),
        },
    }
    .split_midnights()
    .expect("ordered segment endpoints");
    assert_eq!(pieces.len(), 3);
    assert_eq!(pieces[1].start.utc, utc(0));
}

#[test]
fn idle_threshold_wrap_errors_exceptions_and_separate_mask() {
    let policy = IdlePolicy::default();
    let sample = IdleSample {
        tick: Duration::from_millis(179_999),
        last_input: LastInput::Tick(0),
        policy,
        exceptions: vec![],
    };
    assert_eq!(sample.decide(at(179_999)).next_check_mono, mono(180_000));
    assert!(sample.decide(at(179_999)).idle().is_none());
    let sample = IdleSample {
        tick: Duration::from_millis(180_000),
        ..sample
    };
    let record = sample
        .decide(at(180_000))
        .idle()
        .expect("exact boundary idle");
    assert_eq!(record.start, at(180_000));
    let wrap = IdleSample {
        tick: Duration::from_millis(u64::from(u32::MAX) + 101),
        last_input: LastInput::Tick(u32::MAX - 100),
        policy: IdlePolicy {
            threshold: Duration::from_millis(200),
            version: 2,
        },
        exceptions: vec![],
    };
    assert!(wrap.decide(at(1_000)).idle().is_some());
    let future = IdleSample {
        tick: Duration::from_millis(1_000),
        last_input: LastInput::Tick(1_001),
        ..sample.clone()
    };
    assert_eq!(
        future.decide(at(1_000)).error(),
        Some(ReadLastInputError::FutureLastInput)
    );
    let failed = IdleSample {
        last_input: LastInput::ApiFailed,
        ..sample.clone()
    };
    assert_eq!(
        failed.decide(at(180_000)).error(),
        Some(ReadLastInputError::ApiFailed)
    );
    let mut fixture = Fixture::active();
    let _ = fixture.send(at(180_000), CaptureEvent::Idle(sample.clone()));
    assert!(matches!(fixture.core.state(), CaptureState::Tracking(_)));
    let grant = ExceptionGrant {
        kind: IdleException::AppExempt,
        started_utc: utc(180_100),
        until_utc: UtcInstant::new(jiff::Timestamp::MAX),
    };
    assert_eq!(grant.expires_at(), utc(7_380_100));
    let excepted = IdleSample {
        tick: Duration::from_millis(180_100),
        exceptions: vec![grant],
        ..sample
    };
    let step = fixture.send(at(180_100), CaptureEvent::Idle(excepted));
    assert!(step.writes.iter().any(|write| matches!(
        write,
        CaptureWrite::CloseAvailability {
            reason: AvailabilityEndReason::Exception(IdleException::AppExempt),
            ..
        }
    )));
    fixture.finish(at(181_000));
}

#[test]
fn checkpoint_confirms_all_open_targets_and_stop_settles_immediately() {
    let mut fixture = Fixture::active();
    let _ = fixture.send(
        at(10),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Paused)),
    );
    let _ = fixture.send(
        at(20),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
    );
    let step = fixture.send(at(59_000), CaptureEvent::Wake);
    let point = step
        .writes
        .iter()
        .find_map(|write| match write {
            CaptureWrite::Checkpoint(point) => Some(point),
            _ => None,
        })
        .expect("checkpoint");
    assert!(point.interval.is_none());
    assert_eq!(point.availability.len(), 2);
    assert_eq!(point.commit_by_mono, mono(60_000));
    assert!(step.effects.contains(&CaptureEffect::ReviewForeground));
    assert!(
        fixture
            .writes
            .iter()
            .filter(|write| matches!(write, CaptureWrite::Checkpoint(_)))
            .count()
            >= 3
    );
    fixture.finish(at(59_100));
}

#[test]
fn overload_gap_waits_for_review_and_splits_at_clock_boundary() {
    let mut fixture = Fixture::active();
    let _ = fixture.hook(at(10), 1, window(1, "trusted"));
    let overload = fixture.send(at(20), CaptureEvent::EventOverload);
    assert!(
        overload
            .effects
            .contains(&CaptureEffect::DiscardQueuedObservations)
    );
    assert!(overload.writes.iter().any(|write| matches!(
        write,
        CaptureWrite::CloseInterval {
            at: point,
            reason: IntervalEndReason::CaptureGap,
            ..
        } if point.mono == mono(10)
    )));
    let _ = fixture.hook(at(25), 2, window(2, "untrusted"));
    let _ = fixture.send(
        Timestamp {
            utc: utc(-100),
            mono: mono(30),
        },
        CaptureEvent::ClockChanged(ClockChange::TimeChange),
    );
    let recovered = fixture.review(
        Timestamp {
            utc: utc(-90),
            mono: mono(40),
        },
        3,
        window(2, "recovered"),
    );
    assert!(recovered.writes.iter().any(|write| matches!(
        write,
        CaptureWrite::RecordGap(CaptureGap {
            segment,
            ..
        }) if segment.get() == 2
    )));
    fixture.finish(Timestamp {
        utc: utc(-80),
        mono: mono(50),
    });
}

#[test]
fn overload_and_stop_union_do_not_double_count_coverage() {
    let mut fixture = Fixture::active();
    let _ = fixture.send(at(10), CaptureEvent::EventOverload);
    let _ = fixture.send(
        at(20),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
    );
    let _ = fixture.send(
        at(30),
        CaptureEvent::Stop(StopChange::End(StopReason::Locked)),
    );
    let _ = fixture.review(at(40), 2, window(1, "restored"));
    fixture.finish(at(50));
}

#[test]
fn truncation_precedes_privacy_and_dropped_titles_never_leave_core() {
    let mut fixture = Fixture::active();
    fixture.policy(
        &rule(
            "title_hidden",
            "{field='title.raw',op='contains',value='secret'}",
        ),
        0,
    );
    let beyond = format!("{}secret", "a".repeat(4_096));
    let step = fixture.hook(at(10), 2, window(1, &beyond));
    assert_eq!(fields(&step).level, CaptureLevel::Full);
    assert_eq!(fields(&step).title_status, TitleStatus::Truncated);
    assert_eq!(
        fields(&step)
            .title
            .as_ref()
            .expect("retained")
            .encode_utf16()
            .count(),
        4_096
    );
    let step = fixture.hook(at(20), 2, window(1, "private secret title"));
    assert_eq!(fields(&step).level, CaptureLevel::TitleHidden);
    assert!(fields(&step).title.is_none());
    assert_eq!(
        fields(&step).privacy.as_ref().expect("explanation").rule_id,
        "user.capture/hide@1"
    );
    assert_eq!(
        fields(&step).privacy.as_ref().expect("explanation").label,
        Some(PrivacyLabel::Text("Sensitive".into()))
    );
    assert!(!format!("{step:?}").contains("private secret title"));
    let changed = fixture.hook(at(30), 2, window(1, "another secret title"));
    assert_eq!(interval_opens(&changed).len(), 1);
    assert!(!format!("{changed:?}").contains("another secret title"));
    assert_eq!(
        Title::readable(&format!("{}😀", "a".repeat(4_095)))
            .text()
            .expect("bounded")
            .encode_utf16()
            .count(),
        4_095
    );
    fixture.finish(at(40));
}

#[test]
fn excluded_fields_system_signals_and_policy_boundaries_are_sanitized() {
    let mut fixture = Fixture::active();
    fixture.policy(
        &rule(
            "excluded",
            "{field='app.exe_name',op='equals',value='app.exe'}",
        ),
        0,
    );
    let step = fixture.hook(at(10), 2, window(1, "excluded secret"));
    assert_eq!(fields(&step).level, CaptureLevel::Excluded);
    assert!(
        !step
            .writes
            .iter()
            .any(|write| matches!(write, CaptureWrite::ObserveWindow { .. }))
    );
    assert!(!format!("{step:?}").contains("app.exe"));
    assert!(step.writes.iter().any(|write| matches!(
        write,
        CaptureWrite::CloseInterval {
            reason: IntervalEndReason::PolicyChanged,
            ..
        }
    )));
    fixture.finish(at(20));
    let mut fixture = Fixture::active();
    let mut foreground = window(2, "display secret");
    if let Foreground::Window { privacy, .. } = &mut foreground {
        privacy.capture_excluded = SystemSignal::Present;
    }
    let step = fixture.hook(at(10), 2, foreground);
    assert_eq!(fields(&step).title_status, TitleStatus::Redacted);
    assert!(!format!("{step:?}").contains("display secret"));
    fixture.finish(at(20));
}

#[test]
fn browser_privacy_wait_has_no_output_title_and_timeout_discards_it() {
    let mut fixture = Fixture::active();
    fixture.policy(
        &rule(
            "title_hidden",
            "{field='web.domain',op='domain_suffix',value='bank.example'}",
        ),
        0,
    );
    let mut foreground = window(2, "bank secret");
    if let Foreground::Window { privacy, .. } = &mut foreground {
        privacy.browser = BrowserContext::Pending;
    }
    let pending = fixture.hook(at(10), 2, foreground);
    assert!(!format!("{pending:?}").contains("bank secret"));
    assert!(matches!(
        fixture.core.state(),
        CaptureState::AwaitingBrowserContext
    ));
    let timeout = fixture.send(at(2_010), CaptureEvent::Wake);
    assert_eq!(fields(&timeout).level, CaptureLevel::TitleHidden);
    assert!(!format!("{timeout:?}").contains("bank secret"));
    fixture.finish(at(2_020));
}

#[test]
fn browser_context_evaluates_delayed_titles_and_domain_only_drops_windows_title() {
    let mut fixture = Fixture::active();
    fixture.policy(
        &rule(
            "title_hidden",
            "{field='web.domain',op='domain_suffix',value='bank.example'}",
        ),
        0,
    );
    let mut foreground = window(2, "bank secret");
    if let Foreground::Window { privacy, .. } = &mut foreground {
        privacy.browser = BrowserContext::Pending;
    }
    let _ = fixture.hook(at(10), 2, foreground.clone());
    let mut web = FieldSnapshot {
        domain: Some(crate::domain_name::DomainName::parse("bank.example").expect("domain")),
        ..FieldSnapshot::default()
    };
    web.text
        .insert(Field::TabTitle, NfcText::new("bank secret"));
    if let Foreground::Window { privacy, .. } = &mut foreground {
        privacy.browser = BrowserContext::Ready(web);
    }
    let ready = fixture.hook(at(20), 2, foreground);
    assert_eq!(fields(&ready).web, crate::rules::WebAction::DomainOnly);
    assert!(!format!("{ready:?}").contains("bank secret"));
    fixture.finish(at(30));
}

#[test]
fn deadline_scheduler_expiration_cancellation_empty_and_clock_recalculation() {
    let mut deadlines = DeadlineScheduler::default();
    assert_eq!(deadlines.next(at(0)).expect("valid timer deadline"), None);
    deadlines.register(TimedPolicyKind::Pause, Deadline::Utc(utc(100)));
    deadlines.register(TimedPolicyKind::KeepTiming, Deadline::Mono(mono(200)));
    assert_eq!(
        deadlines.next(at(0)).expect("valid timer deadline"),
        Some(mono(100))
    );
    assert_eq!(
        deadlines
            .next(Timestamp {
                utc: utc(-100),
                mono: mono(50)
            })
            .expect("valid timer deadline"),
        Some(mono(200))
    );
    assert_eq!(
        deadlines.take_due(at(100)),
        vec![DeadlineKind::Policy(TimedPolicyKind::Pause)]
    );
    deadlines.cancel(TimedPolicyKind::KeepTiming);
    assert_eq!(deadlines.next(at(100)).expect("valid timer deadline"), None);
    let mut fixture = Fixture::active();
    let step = fixture.send(
        at(10),
        CaptureEvent::SetTimedState(TimedState {
            policy: TimedPolicy::Privacy(PrivacyMode::Private),
            started: utc(0),
            until: utc(5),
        }),
    );
    assert!(
        step.effects
            .contains(&CaptureEffect::PolicyExpired(TimedPolicyKind::Privacy))
    );
    fixture.finish(at(20));
}

fn batch(id: u64, start: u64, end: u64, count: usize) -> WriteBatch {
    WriteBatch {
        id: ids::IdAllocator::test_batchid(id),
        run: RunId::new(1),
        coverage: vec![SegmentRange {
            segment: ids::IdAllocator::test_segmentid(1),
            range: TimeRange {
                start: at(start),
                end: at(end),
            },
        }],
        writes: vec![
            CaptureWrite::CloseInterval {
                id: ids::IdAllocator::test_rowid(1),
                at: at(end),
                reason: IntervalEndReason::Title
            };
            count
        ],
        title_storm: TitleActivity::Ordinary,
    }
}
#[test]
fn backlog_count_and_time_limits_record_exact_gap_ranges_and_flush_first() {
    let mut backlog = WriteBacklog::new();
    backlog.failed(BacklogFailure::Database);
    assert!(
        backlog
            .push(batch(1, 0, 100, 4_096), at(100))
            .gaps
            .is_empty()
    );
    let overflow = backlog.push(batch(2, 100, 200, 1), at(200));
    assert_eq!(
        overflow.discarded,
        vec![
            ids::IdAllocator::test_batchid(1),
            ids::IdAllocator::test_batchid(2)
        ]
    );
    assert_eq!(backlog.command_count(), 0);
    let StoredGap { run, gap } = backlog.first_gap().expect("gap first");
    assert_eq!(
        gap.range,
        TimeRange {
            start: at(0),
            end: at(200)
        }
    );
    assert_eq!(gap.reason, GapReason::DatabaseError);
    let _ = backlog.push(batch(3, 200, 300, 1), at(300));
    assert!(backlog.front().is_none());
    assert!(backlog.committed_gap(run, gap));
    assert_eq!(
        backlog.front().expect("normal after gap").id,
        ids::IdAllocator::test_batchid(3)
    );
    assert!(backlog.committed(ids::IdAllocator::test_batchid(3)));
    let _ = backlog.push(batch(4, 300, 400, 1), at(400));
    assert!(backlog.advance(at(600_399)).gaps.is_empty());
    let expired = backlog.advance(at(600_400));
    assert_eq!(
        expired.gaps[0].range,
        TimeRange {
            start: at(300),
            end: at(400)
        }
    );
}

#[test]
fn backlog_coalesces_only_replaceable_checkpoint_targets_and_detects_missed_commit_deadline() {
    let mut backlog = WriteBacklog::new();
    let point = Checkpoint {
        run: RunId::new(1),
        segment: ids::IdAllocator::test_segmentid(1),
        interval: Some(ids::IdAllocator::test_rowid(1)),
        availability: vec![],
        at: at(59_000),
        commit_by_mono: mono(60_000),
        web_extension: WebRowExtension::PendingM5,
    };
    let mut first = batch(1, 0, 59_000, 0);
    first.writes.push(CaptureWrite::Checkpoint(point.clone()));
    let _ = backlog.push(first.clone(), at(59_000));
    let mut replacement = first;
    replacement.id = ids::IdAllocator::test_batchid(2);
    replacement.writes = vec![CaptureWrite::Checkpoint(Checkpoint {
        at: at(59_100),
        commit_by_mono: mono(60_100),
        web_extension: WebRowExtension::PendingM5,
        ..point
    })];
    assert_eq!(
        backlog.push(replacement, at(59_100)).coalesced,
        vec![ids::IdAllocator::test_batchid(1)]
    );
    assert_eq!(backlog.command_count(), 1);
    assert_eq!(
        backlog.advance(at(60_101)).gaps[0].reason,
        GapReason::WriteQueueOverload
    );
}

#[test]
fn seeded_event_sequences_conserve_duration_per_run_and_clock_segment() {
    let mut seed = 0x45f1_ba8du64;
    for _case in 0..16 {
        let mut fixture = Fixture::active();
        let mut now = at(0);
        let mut generation = 1;
        for _ in 0..300 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            now = now.advance(Duration::from_millis(1 + ((seed >> 32) % 200)));
            let reason = match (seed >> 16) % 4 {
                0 => StopReason::Locked,
                1 => StopReason::Disconnected,
                2 => StopReason::Suspended,
                _ => StopReason::Paused,
            };
            match seed % 10 {
                0 => {
                    let _ = fixture.send(now, CaptureEvent::Stop(StopChange::Begin(reason)));
                }
                1 => {
                    let _ = fixture.send(now, CaptureEvent::Stop(StopChange::End(reason)));
                }
                2 => {
                    let _ = fixture.send(now, CaptureEvent::EventOverload);
                }
                3 => {
                    now.utc = now
                        .utc
                        .checked_sub(Duration::from_millis(3_000))
                        .expect("rollback fits test range");
                    let _ = fixture.send(now, CaptureEvent::ClockChanged(ClockChange::TimeChange));
                }
                4 => {
                    let _ = fixture.send(now, CaptureEvent::Wake);
                }
                5 => {
                    generation += 1;
                    let _ = fixture.review(
                        now,
                        generation,
                        Foreground::None(NoForegroundReason::UnknownDesktop),
                    );
                }
                _ => {
                    generation += 1;
                    let _ = fixture.review(
                        now,
                        generation,
                        window((seed >> 8) % 3 + 1, &format!("title {}", seed % 7)),
                    );
                }
            }
        }
        fixture.finish(now.advance(Duration::from_millis(100)));
    }
}

#[test]
fn failed_buffer_repair_and_core_tail_gap_conserve_persisted_duration() {
    let mut fixture = Fixture::active();
    let mut persisted = fixture.writes.clone();
    let lost = fixture.hook(at(10), 2, window(2, "lost private fact"));
    let CaptureParts { batch, .. } = lost.into_parts(RunId::new(1));
    let mut backlog = WriteBacklog::new();
    backlog.failed(BacklogFailure::Database);
    let _ = backlog.push(batch.expect("write batch"), at(10));
    let outcome = backlog.advance(at(600_010));
    assert_eq!(outcome.gaps.len(), 1);
    persisted.extend(backlog.recovery_writes());
    let reset = fixture.send(
        at(600_010),
        CaptureEvent::PersistenceGap {
            reason: GapReason::DatabaseError,
            lost_through: at(10),
        },
    );
    assert!(
        !reset
            .writes
            .iter()
            .any(|write| matches!(write, CaptureWrite::CloseInterval { .. }))
    );
    persisted.extend(reset.writes);
    persisted.extend(
        fixture
            .review(at(600_020), 3, window(1, "after recovery"))
            .writes,
    );
    persisted.extend(fixture.send(at(600_030), CaptureEvent::Shutdown).writes);
    assert_conservation(&persisted);
}

#[test]
fn clock_boundary_batch_loss_preserves_segments_and_identity_free_gaps() {
    let mut fixture = Fixture::active();
    let mut persisted = fixture.writes.clone();
    let jump = fixture.send(
        Timestamp {
            utc: utc(-100),
            mono: mono(20),
        },
        CaptureEvent::ClockChanged(ClockChange::TimeChange),
    );
    assert_eq!(jump.coverage.len(), 2);
    let CaptureParts { batch, .. } = jump.into_parts(RunId::new(1));
    let mut backlog = WriteBacklog::new();
    let _ = backlog.push(batch.expect("boundary batch"), at(20));
    let loss = backlog.advance(at(600_020));
    assert_eq!(loss.gaps.len(), 2);
    while let Some(StoredGap { run, gap }) = backlog.first_gap() {
        persisted.extend(backlog.recovery_writes());
        assert!(backlog.committed_gap(run, gap));
    }
    let now = Timestamp {
        utc: utc(599_900),
        mono: mono(600_020),
    };
    persisted.extend(
        fixture
            .send(
                now,
                CaptureEvent::PersistenceGap {
                    reason: GapReason::WriteQueueOverload,
                    lost_through: Timestamp {
                        utc: utc(-100),
                        mono: mono(20),
                    },
                },
            )
            .writes,
    );
    persisted.extend(
        fixture
            .review(
                now.advance(Duration::from_millis(10)),
                2,
                window(1, "restored"),
            )
            .writes,
    );
    persisted.extend(
        fixture
            .send(
                now.advance(Duration::from_millis(20)),
                CaptureEvent::Shutdown,
            )
            .writes,
    );
    assert_conservation(&persisted);
}

#[test]
fn discarded_batch_feedback_allows_later_commits_without_database_row_ids() {
    let StartedCapture {
        mut core,
        step: initial,
    } = CaptureCore::start(RunId::new(1), at(0)).expect("valid capture startup");
    let rules = RuleRegistry::from_state(RegistryState::default())
        .expect("registry")
        .snapshots()
        .load();
    let first = initial.batch.expect("initial batch");
    let settled = core.step(CaptureInput {
        at: at(1),
        rules: Arc::clone(&rules),
        event: CaptureEvent::BatchDiscarded(first),
    });
    assert!(settled.is_ok());
    let next = core
        .step(CaptureInput {
            at: at(2),
            rules: Arc::clone(&rules),
            event: CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
        })
        .expect("valid capture event");
    assert!(
        next.writes.is_empty(),
        "session query alone must not open an interval"
    );
    let next = core
        .step(CaptureInput {
            at: at(2),
            rules: Arc::clone(&rules),
            event: reading(
                1,
                ObservationSource::Review,
                window(1, "first reviewed window"),
            ),
        })
        .expect("valid capture event");
    let batch = next.batch.expect("review writes");
    let settled = core.step(CaptureInput {
        at: at(3),
        rules: Arc::clone(&rules),
        event: CaptureEvent::Committed(batch),
    });
    assert!(settled.is_ok());
    assert_eq!(core.committed_batch(), batch);
    let stale = core.step(CaptureInput {
        at: at(4),
        rules,
        event: CaptureEvent::Committed(batch),
    });
    assert!(matches!(
        stale,
        Err(StepCaptureError::InvalidAcknowledgement)
    ));
}

#[test]
fn idle_exception_expiry_does_not_backdate_idle_through_suppressed_time() {
    let mut fixture = Fixture::active();
    let base = IdleSample {
        tick: Duration::from_millis(180_000),
        last_input: LastInput::Tick(0),
        policy: IdlePolicy::default(),
        exceptions: vec![],
    };
    let _ = fixture.send(at(180_000), CaptureEvent::Idle(base.clone()));
    let grant = ExceptionGrant {
        kind: IdleException::KeepTiming,
        started_utc: utc(180_100),
        until_utc: utc(200_000),
    };
    let _ = fixture.send(
        at(180_100),
        CaptureEvent::Idle(IdleSample {
            tick: Duration::from_millis(180_100),
            exceptions: vec![grant],
            ..base.clone()
        }),
    );
    let resumed = fixture.send(
        at(200_000),
        CaptureEvent::Idle(IdleSample {
            tick: Duration::from_millis(200_000),
            exceptions: vec![grant],
            ..base
        }),
    );
    let row = resumed
        .writes
        .iter()
        .find_map(|write| match write {
            CaptureWrite::OpenAvailability(row) => Some(row),
            _ => None,
        })
        .expect("idle reopened");
    assert_eq!(row.start, at(200_000));
    fixture.finish(at(200_100));
}

#[test]
fn privacy_relaxation_does_not_restore_cached_dropped_title_and_expiry_splits_now() {
    let mut fixture = Fixture::active();
    let mut foreground = window(2, "never restore this title");
    if let Foreground::Window { privacy, .. } = &mut foreground {
        privacy.mode = PrivacyMode::Private;
    }
    let _ = fixture.hook(at(10), 2, foreground);
    let _ = fixture.send(
        at(20),
        CaptureEvent::SetTimedState(TimedState {
            policy: TimedPolicy::Privacy(PrivacyMode::Private),
            started: utc(20),
            until: utc(30),
        }),
    );
    let expiry = fixture.send(at(30), CaptureEvent::Wake);
    assert!(expiry.writes.iter().any(|write| matches!(
        write,
        CaptureWrite::CloseInterval {
            reason: IntervalEndReason::PolicyChanged,
            ..
        }
    )));
    assert_eq!(fields(&expiry).level, CaptureLevel::TitleHidden);
    assert!(fields(&expiry).title.is_none());
    assert!(!format!("{expiry:?}").contains("never restore this title"));
    fixture.finish(at(40));
}

#[test]
fn suspend_resume_uses_wall_time_for_midnight_masks_and_unbiased_time_for_foreground() {
    let start = Timestamp {
        utc: utc(86_400_000 - 100),
        mono: mono(0),
    };
    let mut fixture = Fixture::new(start);
    let _ = fixture.send(
        start,
        CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
    );
    let _ = fixture.review(start, 1, window(1, "before sleep"));
    let _ = fixture.send(
        start.advance(Duration::from_millis(10)),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Suspended)),
    );
    let resume = Timestamp {
        utc: utc(2 * 86_400_000 + 100),
        mono: mono(20),
    };
    let step = fixture.send(
        resume,
        CaptureEvent::Stop(StopChange::End(StopReason::Suspended)),
    );
    assert!(step.writes.iter().any(|write| matches!(
        write,
        CaptureWrite::CloseAvailability {
            reason: AvailabilityEndReason::MidnightSplit,
            ..
        }
    )));
    assert!(step.writes.iter().any(|write| matches!(write, CaptureWrite::OpenClockSegment { segment, at } if segment.get() == 2 && *at == resume)));
    let _ = fixture.review(
        resume.advance(Duration::from_millis(1)),
        2,
        window(1, "after sleep"),
    );
    fixture.finish(resume.advance(Duration::from_millis(10)));
}

#[test]
fn crash_recovery_uses_each_rows_confirmation_and_splits_long_locked_mask() {
    let start = Timestamp {
        utc: utc(86_400_000 - 100),
        mono: mono(0),
    };
    let row = OpenAvailability {
        id: ids::IdAllocator::test_rowid(1),
        segment: ids::IdAllocator::test_segmentid(1),
        reason: AvailabilityReason::Stopped(StopReason::Locked),
        start,
        idle: None,
    };
    let confirmed = start.advance(Duration::from_millis(2 * 86_400_000_u64));
    let mut persisted = vec![
        CaptureWrite::OpenRun {
            run: RunId::new(1),
            at: start,
        },
        CaptureWrite::OpenClockSegment {
            segment: ids::IdAllocator::test_segmentid(1),
            at: start,
        },
        CaptureWrite::OpenAvailability(row.clone()),
    ];
    let plan = RecoverableRun {
        web_extension: WebRowExtension::PendingM5,
        run: RunId::new(1),
        confirmed,
        next_row: ids::IdAllocator::test_rowid(2),
        intervals: vec![],
        availability: vec![ConfirmedAvailability { row, confirmed }],
        segments: vec![ConfirmedSegment {
            segment: ids::IdAllocator::test_segmentid(1),
            confirmed,
        }],
    }
    .settle();
    assert!(matches!(
        plan.last(),
        Some(CaptureWrite::CloseRun {
            reason: RunEndReason::CrashRecovered,
            ..
        })
    ));
    assert_eq!(
        plan.iter()
            .filter(|write| matches!(write, CaptureWrite::CloseAvailability { .. }))
            .count(),
        3
    );
    persisted.extend(plan);
    assert_conservation(&persisted);
    let mut fixture = Fixture::active();
    let row = fixture
        .writes
        .iter()
        .rev()
        .find_map(|write| match write {
            CaptureWrite::OpenInterval(row) => Some(row.clone()),
            _ => None,
        })
        .expect("foreground row");
    let plan = RecoverableRun {
        web_extension: WebRowExtension::PendingM5,
        run: RunId::new(1),
        confirmed: at(59_000),
        next_row: ids::IdAllocator::test_rowid(100),
        intervals: vec![ConfirmedInterval {
            row,
            confirmed: at(58_000),
        }],
        availability: vec![],
        segments: vec![ConfirmedSegment {
            segment: ids::IdAllocator::test_segmentid(1),
            confirmed: at(59_000),
        }],
    }
    .settle();
    assert!(plan.iter().any(|write| matches!(
        write,
        CaptureWrite::CloseInterval {
            at: point,
            reason: IntervalEndReason::CrashRecovered,
            ..
        } if point.mono == mono(58_000)
    )));
    let _ = fixture.send(at(1), CaptureEvent::Shutdown);
}

#[test]
fn policy_change_during_resume_wait_does_not_reopen_cached_window() {
    let mut fixture = Fixture::active();
    let _ = fixture.send(
        at(10),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Paused)),
    );
    let _ = fixture.send(
        at(20),
        CaptureEvent::Stop(StopChange::End(StopReason::Paused)),
    );
    let policy = fixture.send(
        at(30),
        CaptureEvent::PolicyChanged(PolicyObservation::Refresh),
    );
    assert!(
        interval_opens(&policy)
            .iter()
            .all(|row| matches!(row.fact, IntervalFact::NoForeground(_)))
    );
    let ignored = fixture.hook(at(40), 2, window(2, "still waiting"));
    assert!(interval_opens(&ignored).is_empty());
    let _ = fixture.review(at(50), 2, window(2, "reviewed"));
    fixture.finish(at(60));
}

#[test]
fn overload_across_midnight_ends_at_trusted_time_without_fabricated_foreground() {
    let start = Timestamp {
        utc: utc(86_400_000 - 100),
        mono: mono(0),
    };
    let mut fixture = Fixture::new(start);
    let _ = fixture.send(
        start,
        CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
    );
    let _ = fixture.review(start, 1, window(1, "trusted"));
    let overloaded = fixture.send(
        start.advance(Duration::from_millis(200)),
        CaptureEvent::EventOverload,
    );
    assert!(
        !overloaded
            .writes
            .iter()
            .any(|write| matches!(write, CaptureWrite::OpenInterval(_)))
    );
    let _ = fixture.review(
        start.advance(Duration::from_millis(300)),
        2,
        window(1, "restored"),
    );
    fixture.finish(start.advance(Duration::from_millis(400)));
}

#[test]
fn backlog_has_one_adaptive_deadline_and_coalescing_preserves_oldest_commit_budget() {
    let mut backlog = WriteBacklog::new();
    assert_eq!(backlog.next_deadline(), None);
    let _ = backlog.push(batch(1, 0, 100, 1), at(100));
    assert_eq!(backlog.next_deadline(), Some(mono(600_100)));
    assert!(backlog.committed(ids::IdAllocator::test_batchid(1)));
    assert_eq!(backlog.next_deadline(), None);
    let mut first = batch(2, 0, 59_000, 0);
    first.writes.push(CaptureWrite::Checkpoint(Checkpoint {
        run: RunId::new(1),
        segment: ids::IdAllocator::test_segmentid(1),
        interval: Some(ids::IdAllocator::test_rowid(1)),
        availability: vec![],
        at: at(59_000),
        commit_by_mono: mono(60_000),
        web_extension: WebRowExtension::PendingM5,
    }));
    let _ = backlog.push(first, at(59_000));
    let mut latest = batch(3, 59_000, 59_100, 0);
    latest.writes.push(CaptureWrite::Checkpoint(Checkpoint {
        run: RunId::new(1),
        segment: ids::IdAllocator::test_segmentid(1),
        interval: Some(ids::IdAllocator::test_rowid(1)),
        availability: vec![],
        at: at(59_100),
        commit_by_mono: mono(60_100),
        web_extension: WebRowExtension::PendingM5,
    }));
    let coalesced = backlog.push(latest, at(59_100));
    assert_eq!(coalesced.coalesced, vec![ids::IdAllocator::test_batchid(2)]);
    assert_eq!(backlog.next_deadline(), Some(mono(60_001)));
    assert_eq!(
        backlog.front().expect("replacement").coverage[0]
            .range
            .start,
        at(0)
    );
}

#[test]
fn overdue_maintenance_marks_unknown_range_instead_of_confirming_an_unreviewed_window() {
    let mut fixture = Fixture::active();
    let step = fixture.send(at(60_001), CaptureEvent::Wake);
    assert!(matches!(
        fixture.core.state(),
        CaptureState::Gap(GapReason::EventOverload)
    ));
    assert!(step.writes.iter().any(|write| matches!(
        write,
        CaptureWrite::CloseInterval {
            at: point,
            reason: IntervalEndReason::CaptureGap,
            ..
        } if point.mono == mono(0)
    )));
    let _ = fixture.review(at(60_010), 2, window(1, "fresh after stall"));
    fixture.finish(at(60_020));
}

#[test]
fn no_interval_is_emitted_until_session_and_fresh_foreground_review_both_arrive() {
    let mut fixture = Fixture::new(at(0));
    let ready = fixture.send(
        at(0),
        CaptureEvent::Session(SessionStatus::Ready(BTreeSet::new())),
    );
    assert!(interval_opens(&ready).is_empty());
    let policy = fixture.send(
        at(10),
        CaptureEvent::PolicyChanged(PolicyObservation::Refresh),
    );
    assert!(interval_opens(&policy).is_empty());
    let locked = fixture.send(
        at(20),
        CaptureEvent::Stop(StopChange::Begin(StopReason::Locked)),
    );
    assert!(interval_opens(&locked).is_empty());
    let resumed = fixture.send(
        at(30),
        CaptureEvent::Stop(StopChange::End(StopReason::Locked)),
    );
    assert!(interval_opens(&resumed).is_empty());
    let hook = fixture.hook(at(40), 2, window(1, "not reviewed"));
    assert!(interval_opens(&hook).is_empty());
    let reviewed = fixture.review(at(50), 2, window(1, "reviewed"));
    assert!(!interval_opens(&reviewed).is_empty());
    fixture.finish(at(60));
}

#[test]
fn browser_context_authorizes_only_its_correlated_snapshot_and_late_replies_cannot_restore_titles()
{
    let mut fixture = Fixture::active();
    fixture.policy(
        &rule(
            "title_hidden",
            "{field='web.domain',op='domain_suffix',value='bank.example'}",
        ),
        0,
    );
    let pending = |title: &str| {
        let mut foreground = window(2, title);
        if let Foreground::Window { privacy, .. } = &mut foreground {
            privacy.browser = BrowserContext::Pending;
        }
        foreground
    };
    let first = fixture.hook(at(10), 2, pending("secret older page"));
    let second = fixture.hook(at(20), 2, pending("allowed current page"));
    let observation = |step: &CaptureStep| {
        step.effects
            .iter()
            .find_map(|effect| match effect {
                CaptureEffect::AwaitBrowserContext { observation, .. } => Some(*observation),
                _ => None,
            })
            .expect("correlation key")
    };
    let first_id = observation(&first);
    let second_id = observation(&second);
    assert_ne!(first_id, second_id);
    let context = FieldSnapshot {
        domain: Some(crate::domain_name::DomainName::parse("normal.example").expect("domain")),
        ..FieldSnapshot::default()
    };
    let ready = fixture.send(
        at(30),
        CaptureEvent::PrivacyContext {
            observation: second_id,
            browser: BrowserContext::Ready(context.clone()),
        },
    );
    assert!(format!("{ready:?}").contains("allowed current page"));
    assert!(!format!("{ready:?}").contains("secret older page"));
    let late = fixture.send(
        at(40),
        CaptureEvent::PrivacyContext {
            observation: first_id,
            browser: BrowserContext::Ready(context),
        },
    );
    assert!(late.writes.is_empty());
    fixture.finish(at(50));
}

#[test]
fn invalid_capture_inputs_leave_every_core_component_unchanged() {
    let mut fixture = Fixture::active();
    let _ = fixture.hook(at(10), 1, window(1, "known state"));
    let cases = [
        CaptureInput {
            at: at(9),
            rules: fixture.rules.snapshots().load(),
            event: CaptureEvent::Wake,
        },
        CaptureInput {
            at: at(11),
            rules: fixture.rules.snapshots().load(),
            event: CaptureEvent::Committed(ids::IdAllocator::test_batchid(u64::MAX)),
        },
        CaptureInput {
            at: Timestamp {
                utc: UtcInstant::new(jiff::Timestamp::MAX),
                mono: mono(11),
            },
            rules: fixture.rules.snapshots().load(),
            event: CaptureEvent::Wake,
        },
    ];
    for (index, input) in cases.into_iter().enumerate() {
        let before = format!("{:?}", fixture.core);
        let error = fixture
            .core
            .step(input)
            .expect_err("invalid work unit must be rejected");
        match index {
            0 => assert!(matches!(error, StepCaptureError::BackwardMonotonicTime)),
            1 => assert!(matches!(error, StepCaptureError::InvalidAcknowledgement)),
            2 => assert!(matches!(error, StepCaptureError::Time { .. })),
            _ => unreachable!("three explicit cases"),
        }
        assert_eq!(
            format!("{:?}", fixture.core),
            before,
            "rejected input {index} changed the core"
        );
    }
    fixture.finish(at(20));
}
