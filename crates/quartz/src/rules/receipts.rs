//! Volatile, connection-owned confirmations for exact privacy mutations.

use super::digest::ReceiptDigest;
use super::registry::RunRegistryCommandError;
use crate::{
    query::{input::QueryRange, preview::PreviewCalendar, versions::VersionState},
    store::Store,
};
use quartz_domain::rules::registry::{RuleSetWrite, RuleSnapshot};
use quartz_protocol::{
    ErrorKind, LocalDate, PreviewMismatchReason, ProtocolError, QueryVersions, Role,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// 04 §6.3.2 limits each connection to eight five-minute confirmations.
const RECEIPT_CAPACITY: usize = 8;
pub(crate) const RECEIPT_TOKEN_CHARS: usize = 64;
const RECEIPT_LIFETIME: Duration = Duration::from_secs(5 * 60);

#[derive(Debug)]
pub(crate) struct ReceiptOwner {
    context: ReceiptContext,
}
impl ReceiptOwner {
    pub(crate) fn new(role: Role) -> Self {
        Self {
            context: ReceiptContext {
                state: Arc::new(Mutex::new(ReceiptState {
                    role,
                    connection: ConnectionState::Connected,
                    entries: VecDeque::new(),
                })),
                clock: Arc::new(Instant::now),
            },
        }
    }
    #[cfg(test)]
    pub(crate) fn with_clock(role: Role, clock: Arc<dyn Fn() -> Instant + Send + Sync>) -> Self {
        let mut owner = Self::new(role);
        owner.context.clock = clock;
        owner
    }
    pub(crate) fn context(&self) -> ReceiptContext {
        self.context.clone()
    }
}
impl Drop for ReceiptOwner {
    fn drop(&mut self) {
        if let Ok(mut state) = self.context.state.lock() {
            state.connection = ConnectionState::Disconnected;
            state.entries.clear();
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
enum ConnectionState {
    Connected,
    Disconnected,
}
struct ReceiptState {
    role: Role,
    connection: ConnectionState,
    entries: VecDeque<Arc<Receipt>>,
}
struct Receipt {
    token: String,
    role: Role,
    deadline: Instant,
    binding: ReceiptBinding,
}
#[derive(Clone)]
pub(crate) struct ReceiptContext {
    state: Arc<Mutex<ReceiptState>>,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}
impl std::fmt::Debug for ReceiptContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReceiptContext([redacted])")
    }
}
pub(crate) struct IssuedReceipt {
    pub token: String,
    pub expires_utc_ms: i64,
    pub grant: ReceiptGrant,
}
impl std::fmt::Debug for IssuedReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedReceipt")
            .field("expires_utc_ms", &self.expires_utc_ms)
            .finish_non_exhaustive()
    }
}
#[derive(Debug)]
pub(crate) struct ReceiptBinding {
    mutation: String,
    revisions: String,
    inputs: QueryVersions,
    reference_inputs: QueryVersions,
    range: QueryRange,
    reference_range: QueryRange,
    start_date: LocalDate,
    end_date: LocalDate,
}
impl ReceiptBinding {
    pub(crate) fn new(
        mutation: String,
        snapshot: &RuleSnapshot,
        state: &VersionState,
        dates: ReceiptDates,
    ) -> Result<Self, crate::query::RunQueryError> {
        Ok(Self {
            mutation,
            revisions: ReceiptDigest::revisions(snapshot)?,
            inputs: state.digest(dates.range)?,
            reference_inputs: state.digest(dates.reference_range)?,
            range: dates.range,
            reference_range: dates.reference_range,
            start_date: dates.start_date,
            end_date: dates.end_date,
        })
    }
    fn verify(&self, state: &VersionState, snapshot: &RuleSnapshot) -> Result<(), ProtocolError> {
        let calendar = PreviewCalendar::new(
            self.start_date,
            self.end_date,
            state.settings.day_start_offset,
            state.time_zone.clone(),
        )
        .map_err(|error| match error {
            crate::query::RunQueryError::InvalidRangeRequest
            | crate::query::RunQueryError::Calendar { .. } => {
                Self::mismatch(PreviewMismatchReason::Range)
            }
            _ => ReceiptFailure::ResolveRange.into_protocol_error(),
        })?;
        if calendar.range() != self.range || calendar.reference_range() != self.reference_range {
            return Err(Self::mismatch(PreviewMismatchReason::Range));
        }
        if self.revisions != ReceiptDigest::revisions(snapshot)?
            || self.inputs
                != state
                    .digest(self.range)
                    .map_err(|_| ReceiptFailure::HashInputs.into_protocol_error())?
            || self.reference_inputs
                != state
                    .digest(self.reference_range)
                    .map_err(|_| ReceiptFailure::HashInputs.into_protocol_error())?
        {
            return Err(Self::mismatch(PreviewMismatchReason::Inputs));
        }
        Ok(())
    }
    fn mismatch(reason: PreviewMismatchReason) -> ProtocolError {
        ProtocolError::new(ErrorKind::PreviewMismatch { reason })
    }
}
#[derive(Debug)]
pub(crate) struct ReceiptDates {
    pub start_date: LocalDate,
    pub end_date: LocalDate,
    pub range: QueryRange,
    pub reference_range: QueryRange,
}
impl ReceiptContext {
    pub(crate) fn issue(&self, binding: ReceiptBinding) -> Result<IssuedReceipt, ProtocolError> {
        let token = Self::random_token()?;
        let now = (self.clock)();
        let lifetime_ms = i64::try_from(RECEIPT_LIFETIME.as_millis())
            .map_err(|_| ReceiptFailure::Deadline.into_protocol_error())?;
        let expires_utc_ms = jiff::Timestamp::now()
            .as_millisecond()
            .checked_add(lifetime_ms)
            .ok_or_else(|| ReceiptFailure::Deadline.into_protocol_error())?;
        let deadline = now
            .checked_add(RECEIPT_LIFETIME)
            .ok_or_else(|| ReceiptFailure::Deadline.into_protocol_error())?;
        let state = self
            .state
            .lock()
            .map_err(|_| ReceiptFailure::StatePoisoned.into_protocol_error())?;
        if state.connection == ConnectionState::Disconnected {
            return Err(ProtocolError::new(ErrorKind::Cancelled));
        }
        let receipt = Arc::new(Receipt {
            token: token.clone(),
            role: state.role,
            deadline,
            binding,
        });
        Ok(IssuedReceipt {
            token,
            expires_utc_ms,
            grant: ReceiptGrant {
                context: self.clone(),
                receipt,
            },
        })
    }
    fn random_token() -> Result<String, ProtocolError> {
        const TOKEN_BYTES: usize = RECEIPT_TOKEN_CHARS / 2;
        let mut bytes = [0; TOKEN_BYTES];
        #[cfg(windows)]
        {
            use windows::Win32::Security::Cryptography::{
                BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
            };
            // SAFETY: the output slice is live and writable; the system RNG needs no algorithm handle.
            unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }
                .ok()
                .map_err(|_| ReceiptFailure::Random.into_protocol_error())?;
        }
        #[cfg(not(windows))]
        {
            use std::io::Read;
            std::fs::File::open("/dev/urandom")
                .and_then(|mut file| file.read_exact(&mut bytes))
                .map_err(|_| ReceiptFailure::Random.into_protocol_error())?;
        }
        Ok(quartz_domain::text::encode_hex(&bytes))
    }
}
/// Owns a pending receipt; dropping it leaves the connection's delivered receipts unchanged.
pub(crate) struct ReceiptGrant {
    context: ReceiptContext,
    receipt: Arc<Receipt>,
}
impl std::fmt::Debug for ReceiptGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReceiptGrant([redacted])")
    }
}
impl ReceiptGrant {
    pub(crate) fn delivered(self) -> Result<(), ProtocolError> {
        if (self.context.clock)() >= self.receipt.deadline {
            return Err(ProtocolError::new(ErrorKind::PreviewExpired));
        }
        let mut state = self
            .context
            .state
            .lock()
            .map_err(|_| ReceiptFailure::StatePoisoned.into_protocol_error())?;
        if state.connection == ConnectionState::Disconnected {
            return Err(ProtocolError::new(ErrorKind::Cancelled));
        }
        let collision = state.entries.iter().fold(false, |found, entry| {
            entry.matches(&self.receipt.token) | found
        });
        if collision {
            drop(state);
            return Err(ReceiptFailure::Duplicate.into_protocol_error());
        }
        if state.entries.len() == RECEIPT_CAPACITY {
            state.entries.pop_front();
        }
        state.entries.push_back(self.receipt);
        Ok(())
    }
}
impl Receipt {
    fn matches(&self, token: &str) -> bool {
        if token.len() != RECEIPT_TOKEN_CHARS {
            return false;
        }
        let mut difference = 0_u8;
        for index in 0..RECEIPT_TOKEN_CHARS {
            difference |= self.token.as_bytes()[index] ^ token.as_bytes()[index];
        }
        std::hint::black_box(difference) == 0
    }
}
/// Contains only the proof supplied by the caller; verification stays in the writer.
pub(crate) struct ReceiptAuthorization {
    context: Option<ReceiptContext>,
    token: Option<String>,
    mutation: String,
}
impl ReceiptAuthorization {
    pub(super) fn new(
        context: Option<ReceiptContext>,
        token: Option<String>,
        mutation: String,
    ) -> Self {
        Self {
            context,
            token,
            mutation,
        }
    }
    pub(crate) fn commit(
        &self,
        store: &mut Store,
        snapshot: &RuleSnapshot,
        writes: &[RuleSetWrite],
        now: i64,
    ) -> Result<(), RunRegistryCommandError> {
        self.try_commit(store, snapshot, writes, now)
            .map_err(|error| match error {
                CommitReceiptError::Persist { source } => {
                    RunRegistryCommandError::Persist { source }
                }
                CommitReceiptError::Preview { error } => RunRegistryCommandError::Receipt { error },
                CommitReceiptError::Internal { failure } => RunRegistryCommandError::Receipt {
                    error: failure.into_protocol_error(),
                },
            })
    }
    fn try_commit(
        &self,
        store: &mut Store,
        snapshot: &RuleSnapshot,
        writes: &[RuleSetWrite],
        now: i64,
    ) -> Result<(), CommitReceiptError> {
        let context = self.context.as_ref().ok_or_else(|| {
            CommitReceiptError::preview(if self.token.is_none() {
                ErrorKind::PreviewRequired
            } else {
                ErrorKind::PreviewExpired
            })
        })?;
        let receipt = {
            let state = context
                .state
                .lock()
                .map_err(|_| CommitReceiptError::Internal {
                    failure: ReceiptFailure::StatePoisoned,
                })?;
            if state.role != Role::Ui {
                return Err(CommitReceiptError::preview(ErrorKind::Forbidden));
            }
            let token = self
                .token
                .as_deref()
                .ok_or_else(|| CommitReceiptError::preview(ErrorKind::PreviewRequired))?;
            let mut selected = None;
            for entry in &state.entries {
                if entry.matches(token) {
                    selected = Some(entry.clone());
                }
            }
            let receipt =
                selected.ok_or_else(|| CommitReceiptError::preview(ErrorKind::PreviewExpired))?;
            if state.connection == ConnectionState::Disconnected
                || (context.clock)() >= receipt.deadline
            {
                return Err(CommitReceiptError::preview(ErrorKind::PreviewExpired));
            }
            if receipt.role != state.role {
                return Err(CommitReceiptError::preview(ErrorKind::Forbidden));
            }
            if receipt.binding.mutation != self.mutation {
                return Err(CommitReceiptError::Preview {
                    error: ReceiptBinding::mismatch(PreviewMismatchReason::Mutation),
                });
            }
            receipt
        };
        store.rule_transaction(|transaction| {
            let stored = transaction
                .preview_versions(receipt.binding.reference_range)
                .map_err(|_| CommitReceiptError::Internal {
                    failure: ReceiptFailure::ReadInputs,
                })?;
            let versions =
                VersionState::from_stored(stored, snapshot.hashes().clone()).map_err(|_| {
                    CommitReceiptError::Internal {
                        failure: ReceiptFailure::ResolveVersions,
                    }
                })?;
            receipt
                .binding
                .verify(&versions, snapshot)
                .map_err(|error| CommitReceiptError::Preview { error })?;
            if (context.clock)() >= receipt.deadline {
                return Err(CommitReceiptError::preview(ErrorKind::PreviewExpired));
            }
            transaction
                .execute(writes, now)
                .map_err(|source| CommitReceiptError::Persist { source })
        })?;
        match context.state.lock() {
            Ok(mut state) => state.entries.retain(|entry| !Arc::ptr_eq(entry, &receipt)),
            Err(_) => {
                ReceiptFailure::StatePoisoned.into_protocol_error();
            }
        }
        Ok(())
    }
}
#[derive(Debug, snafu::Snafu)]
enum CommitReceiptError {
    #[snafu(display("could not read preview dependencies"))]
    Internal { failure: ReceiptFailure },
    #[snafu(display("preview dependencies changed"))]
    Preview { error: ProtocolError },
    #[snafu(display("could not persist confirmed mutation"))]
    Persist {
        source: crate::store::rules::ExecuteRuleWritesError,
    },
}
impl From<diesel::result::Error> for CommitReceiptError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Persist {
            source: source.into(),
        }
    }
}

#[derive(Debug)]
enum ReceiptFailure {
    StatePoisoned,
    ReadInputs,
    ResolveVersions,
    ResolveRange,
    HashInputs,
    Random,
    Deadline,
    Duplicate,
}
impl ReceiptFailure {
    fn into_protocol_error(self) -> ProtocolError {
        tracing::error!(failure = ?self, "privacy receipt operation failed");
        ProtocolError::new(ErrorKind::Internal)
    }
}
impl CommitReceiptError {
    fn preview(kind: ErrorKind) -> Self {
        Self::Preview {
            error: ProtocolError::new(kind),
        }
    }
}

#[cfg(test)]
mod tests;
