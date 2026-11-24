use super::*;
use crate::{
    query::calendar::DayCalendar,
    rules::{registry::RegistryShell, write::RuleMutation},
    store::query::test_support::{FIXTURE_DATE, QueryFixture},
};
use quartz_domain::capture::UtcInstant;
use quartz_protocol::RuleSetsEditRequest;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Fixture {
    facts: QueryFixture,
    registry: RegistryShell,
    owner: ReceiptOwner,
    mutation: RuleMutation,
}
impl Fixture {
    fn new(role: Role) -> Self {
        let mut facts = QueryFixture::new(4);
        let registry = RegistryShell::open(&mut facts.store, UtcInstant::try_from(0).expect("UTC"))
            .expect("registry");
        Self { facts, registry, owner: ReceiptOwner::new(role), mutation: RuleMutation::Edit(RuleSetsEditRequest { key: "user.receipt".into(), base_version: 0, text: "format='quartz-rules/1'\n[set]\nkey='user.receipt'\nkind='privacy'\nname='private-text-marker'\n".into(), preview_receipt: None }) }
    }
    fn state(&mut self) -> VersionState {
        VersionState::from_stored(
            self.facts.store.version_state().expect("stored"),
            self.registry.snapshots().load().hashes().clone(),
        )
        .expect("versions")
    }
    fn binding_in(
        &self,
        state: &VersionState,
        start: &str,
        end: &str,
        reference_start: &str,
    ) -> ReceiptBinding {
        let range = DayCalendar::range_in_zone(
            start.parse().expect("date"),
            state.settings.day_start_offset,
            state.time_zone.clone(),
        )
        .expect("range");
        let reference = DayCalendar::range_in_zone(
            reference_start.parse().expect("date"),
            state.settings.day_start_offset,
            state.time_zone.clone(),
        )
        .expect("reference");
        ReceiptBinding::new(
            self.mutation.digest().expect("digest"),
            &self.registry.snapshots().load(),
            state,
            ReceiptDates {
                start_date: start.parse().expect("date"),
                end_date: end.parse().expect("date"),
                range,
                reference_range: QueryRange {
                    start: reference.start,
                    end: range.end,
                },
            },
        )
        .expect("binding")
    }
    fn issue(&mut self) -> IssuedReceipt {
        let state = self.state();
        let binding = self.binding_in(&state, FIXTURE_DATE, "2025-01-03", "2024-12-27");
        self.owner.context.issue(binding).expect("issue")
    }
    fn deliver(&mut self) -> String {
        let issued = self.issue();
        issued.grant.delivered().expect("deliver");
        issued.token
    }
    fn save(
        &mut self,
        token: &str,
    ) -> Result<quartz_protocol::RuleSetsWriteResponse, ProtocolError> {
        let RuleMutation::Edit(mut request) = self.mutation.clone() else {
            panic!("edit")
        };
        request.preview_receipt = Some(token.into());
        RuleMutation::Edit(request).execute_with_receipts(
            &mut self.registry,
            &mut self.facts.store,
            UtcInstant::try_from(0).expect("UTC"),
            Some(self.owner.context()),
        )
    }
}
#[test]
fn privacy_commit_releases_receipt_lock_before_transaction() {
    let mut fixture = Fixture::new(Role::Ui);
    let available = Arc::new(AtomicBool::new(false));
    let observed = available.clone();
    let state = fixture.owner.context.state.clone();
    let calls = AtomicUsize::new(0);
    fixture.owner.context.clock = Arc::new(move || {
        const TRANSACTION_CLOCK_CALL: usize = 3;
        if calls.fetch_add(1, Ordering::SeqCst) == TRANSACTION_CLOCK_CALL {
            observed.store(state.try_lock().is_ok(), Ordering::SeqCst);
        }
        Instant::now()
    });
    let token = fixture.deliver();
    fixture.save(&token).expect("save");
    assert!(
        available.load(Ordering::SeqCst),
        "transaction must release receipt lock"
    );
}
#[test]
fn undelivered_ninth_preview_preserves_oldest_receipt() {
    let mut fixture = Fixture::new(Role::Ui);
    let oldest = fixture.deliver();
    for _ in 1..RECEIPT_CAPACITY {
        fixture.deliver();
    }
    let ninth = fixture.issue();
    drop(ninth.grant);
    fixture
        .save(&oldest)
        .expect("undelivered preview must not evict oldest");
}
#[test]
fn dropping_owner_after_poison_does_not_panic() {
    let fixture = Fixture::new(Role::Ui);
    let poison = fixture.owner.context.state.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = poison.lock().expect("test lock");
            panic!("test poison");
        })
        .join()
        .is_err()
    );
    let owner = fixture.owner;
    assert!(
        std::thread::spawn(move || drop(owner)).join().is_ok(),
        "destructor must not panic on poison"
    );
}
#[test]
fn dropping_undelivered_grant_after_poison_does_not_panic() {
    let mut fixture = Fixture::new(Role::Ui);
    let issued = fixture.issue();
    let grant = issued.grant;
    let poison = fixture.owner.context.state.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = poison.lock().expect("test lock");
            panic!("test poison");
        })
        .join()
        .is_err()
    );
    let result = std::thread::spawn(move || drop(grant)).join();
    drop(fixture.owner);
    assert!(
        result.is_ok(),
        "undelivered destructor must not lock poisoned state"
    );
}
#[test]
fn non_ui_role_is_rejected_before_receipt_lookup() {
    let mut fixture = Fixture::new(Role::Cli);
    assert_eq!(
        fixture
            .save(&"0".repeat(RECEIPT_TOKEN_CHARS))
            .expect_err("CLI cannot save")
            .kind,
        ErrorKind::Forbidden
    );
}
#[test]
fn maximum_preview_end_date_can_be_confirmed() {
    let mut fixture = Fixture::new(Role::Ui);
    let mut state = fixture.state();
    state.time_zone = jiff::tz::TimeZone::get("Etc/GMT-12").expect("positive offset");
    let binding = fixture.binding_in(&state, "9999-12-30", "9999-12-31", "9999-12-24");
    binding
        .verify(&state, &fixture.registry.snapshots().load())
        .expect("maximum end date is valid");
}
#[test]
fn changed_reference_boundaries_report_range_mismatch() {
    let mut fixture = Fixture::new(Role::Ui);
    let mut state = fixture.state();
    state.time_zone = jiff::tz::TimeZone::get("America/New_York").expect("zone");
    let binding = fixture.binding_in(&state, "2025-03-10", "2025-03-11", "2025-03-04");
    state.time_zone = jiff::tz::TimeZone::get("Etc/GMT+4").expect("fixed offset");
    assert_eq!(
        binding
            .verify(&state, &fixture.registry.snapshots().load())
            .expect_err("reference changed")
            .kind,
        ErrorKind::PreviewMismatch {
            reason: PreviewMismatchReason::Range
        }
    );
}
#[test]
fn failed_privacy_persistence_logs_typed_cause_without_private_values() {
    use diesel::{Connection, connection::SimpleConnection};
    use std::io::{Read, Seek};
    let mut fixture = Fixture::new(Role::Ui);
    let token = fixture.deliver();
    let path = fixture.facts.directory.path().join("quartz.db");
    let mut connection = diesel::sqlite::SqliteConnection::establish(path.to_str().expect("path"))
        .expect("isolated database");
    connection.batch_execute("CREATE TRIGGER reject_receipt BEFORE INSERT ON rule_set_versions BEGIN SELECT RAISE(ABORT, 'private-sqlite-marker'); END;").expect("reject write");
    let mut log = tempfile::tempfile().expect("log");
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(Mutex::new(log.try_clone().expect("writer")))
            .finish(),
    );
    assert_eq!(
        tracing::dispatcher::with_default(&dispatch, || fixture.save(&token))
            .expect_err("write rejected")
            .kind,
        ErrorKind::Internal
    );
    log.rewind().expect("rewind");
    let mut contents = String::new();
    log.read_to_string(&mut contents).expect("log text");
    assert!(
        contents.contains("Constraint"),
        "typed cause must be logged"
    );
    assert!(!contents.contains(&token));
    assert!(!contents.contains("private-text-marker"));
    assert!(!contents.contains("private-sqlite-marker"));
}

#[test]
fn pending_receipt_cannot_authorize_save_before_delivery() {
    let mut fixture = Fixture::new(Role::Ui);
    let issued = fixture.issue();
    assert_eq!(
        fixture.save(&issued.token).expect_err("not delivered").kind,
        ErrorKind::PreviewExpired
    );
    issued.grant.delivered().expect("deliver");
    fixture
        .save(&issued.token)
        .expect("delivered receipt authorizes save");
}
#[test]
fn disconnect_during_transaction_returns_committed_success_without_reinserting_receipt() {
    let mut fixture = Fixture::new(Role::Ui);
    let (entered, arrival) = std::sync::mpsc::sync_channel(1);
    let (resume, resumed) = std::sync::mpsc::sync_channel(1);
    let resumed = Mutex::new(resumed);
    let calls = AtomicUsize::new(0);
    fixture.owner.context.clock = Arc::new(move || {
        const TRANSACTION_CLOCK_CALL: usize = 3;
        if calls.fetch_add(1, Ordering::SeqCst) == TRANSACTION_CLOCK_CALL {
            entered.send(()).expect("transaction entered");
            resumed.lock().expect("resume lock").recv().expect("resume");
        }
        Instant::now()
    });
    let token = fixture.deliver();
    let context = fixture.owner.context();
    let owner = std::mem::replace(
        &mut fixture.owner,
        ReceiptOwner {
            context: context.clone(),
        },
    );
    struct Completed {
        result: Result<quartz_protocol::RuleSetsWriteResponse, ProtocolError>,
        fixture: Fixture,
    }
    let worker = std::thread::spawn(move || {
        let result = fixture.save(&token);
        Completed { result, fixture }
    });
    const TEST_TIMEOUT: Duration = Duration::from_secs(5);
    arrival
        .recv_timeout(TEST_TIMEOUT)
        .expect("transaction waiting");
    let (closed, closure) = std::sync::mpsc::sync_channel(1);
    let disconnect = std::thread::spawn(move || {
        drop(owner);
        closed.send(()).expect("disconnected");
    });
    let completed_before_commit = closure.recv_timeout(TEST_TIMEOUT).is_ok();
    resume.send(()).expect("resume transaction");
    let completed = worker.join().expect("writer");
    disconnect.join().expect("disconnect");
    assert!(
        completed_before_commit,
        "disconnect must not wait for SQLite work"
    );
    assert_eq!(
        completed.result.expect("committed success").sets[0].version,
        1
    );
    assert!(context.state.lock().expect("state").entries.is_empty());
    drop(completed.fixture);
}
#[test]
fn internal_receipt_read_failure_logs_only_typed_class() {
    use diesel::{Connection, connection::SimpleConnection};
    use std::io::{Read, Seek};
    let mut fixture = Fixture::new(Role::Ui);
    let token = fixture.deliver();
    let authorization = ReceiptAuthorization::new(
        Some(fixture.owner.context()),
        Some(token.clone()),
        fixture.mutation.digest().expect("digest"),
    );
    let path = fixture.facts.directory.path().join("quartz.db");
    let mut connection = diesel::sqlite::SqliteConnection::establish(path.to_str().expect("path"))
        .expect("isolated database");
    connection
        .batch_execute(
            "UPDATE settings SET value='private-settings-marker' WHERE key='metadata_version'",
        )
        .expect("corrupt fixture setting");
    let mut log = tempfile::tempfile().expect("log");
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(Mutex::new(log.try_clone().expect("writer")))
            .finish(),
    );
    let result = tracing::dispatcher::with_default(&dispatch, || {
        authorization.commit(
            &mut fixture.facts.store,
            &fixture.registry.snapshots().load(),
            &[],
            0,
        )
    });
    assert!(
        matches!(result, Err(RunRegistryCommandError::Receipt { error }) if error.kind == ErrorKind::Internal)
    );
    log.rewind().expect("rewind");
    let mut contents = String::new();
    log.read_to_string(&mut contents).expect("log text");
    assert!(contents.contains("ReadInputs"));
    assert!(!contents.contains("private-settings-marker"));
    assert!(!contents.contains(&token));
}

#[test]
fn unrepresentable_changed_date_boundaries_report_range_mismatch() {
    let mut fixture = Fixture::new(Role::Ui);
    let mut state = fixture.state();
    state.time_zone = jiff::tz::TimeZone::get("Etc/GMT-12").expect("positive offset");
    let binding = fixture.binding_in(&state, "9999-12-30", "9999-12-31", "9999-12-24");
    state.time_zone = jiff::tz::TimeZone::UTC;
    assert_eq!(
        binding
            .verify(&state, &fixture.registry.snapshots().load())
            .expect_err("new zone exceeds timestamp range")
            .kind,
        ErrorKind::PreviewMismatch {
            reason: PreviewMismatchReason::Range
        }
    );
}
#[test]
fn mutation_debug_omits_rule_text_and_receipt() {
    let fixture = Fixture::new(Role::Ui);
    let RuleMutation::Edit(mut request) = fixture.mutation else {
        panic!("edit")
    };
    request.preview_receipt = Some("private-receipt-marker".into());
    for mutation in [
        RuleMutation::Edit(request.clone()),
        RuleMutation::Import(request),
    ] {
        let debug = format!("{mutation:?}");
        assert!(!debug.contains("private-text-marker"));
        assert!(!debug.contains("private-receipt-marker"));
    }
}
#[test]
fn token_changes_at_every_position_are_rejected_without_consuming_receipt() {
    let mut fixture = Fixture::new(Role::Ui);
    let token = fixture.deliver();
    for index in 0..RECEIPT_TOKEN_CHARS {
        let mut changed = token.clone();
        let replacement = if changed.as_bytes()[index] == b'0' {
            "1"
        } else {
            "0"
        };
        changed.replace_range(index..index + 1, replacement);
        assert_eq!(
            fixture.save(&changed).expect_err("altered token").kind,
            ErrorKind::PreviewExpired
        );
    }
    fixture.save(&token).expect("original token remains valid");
}

#[test]
fn deadline_overflow_returns_internal_without_issuing_receipt() {
    let mut fixture = Fixture::new(Role::Ui);
    let mut latest = Instant::now();
    for shift in (0..u64::BITS).rev() {
        if let Some(next) = latest.checked_add(Duration::from_secs(1_u64 << shift)) {
            latest = next;
        }
    }
    assert!(
        latest.checked_add(RECEIPT_LIFETIME).is_none(),
        "test clock is near the representable limit"
    );
    fixture.owner.context.clock = Arc::new(move || latest);
    let state = fixture.state();
    let binding = fixture.binding_in(&state, FIXTURE_DATE, "2025-01-03", "2024-12-27");
    assert_eq!(
        fixture
            .owner
            .context
            .issue(binding)
            .expect_err("unrepresentable deadline")
            .kind,
        ErrorKind::Internal
    );
    assert!(
        fixture
            .owner
            .context
            .state
            .lock()
            .expect("state")
            .entries
            .is_empty()
    );
}
