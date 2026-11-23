//! Checked UTC calendar and unbiased monotonic arithmetic.

use super::limits;
use jiff::tz::TimeZone;
use std::time::Duration;

/// Failure to construct or advance a capture time value.
#[derive(Debug, snafu::Snafu)]
pub enum AdvanceTimeError {
    /// UTC calendar or arithmetic exceeded jiff's supported range.
    #[snafu(display("could not advance UTC time"))]
    Utc {
        /// Original calendar or timestamp failure.
        source: jiff::Error,
    },
    /// A monotonic calculation overflowed or moved before its origin.
    #[snafu(display("monotonic time is outside its supported range"))]
    Monotonic,
    /// The requested conversion would discard sub-millisecond precision.
    #[snafu(display("capture time cannot discard sub-millisecond precision"))]
    Submillisecond,
}

/// UTC wall-clock position, with storage conversions performed explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcInstant(jiff::Timestamp);
impl UtcInstant {
    /// Wraps an already validated jiff UTC timestamp.
    #[must_use]
    pub fn new(value: jiff::Timestamp) -> Self {
        Self(value)
    }
    /// Borrows the timestamp value for calendar operations or store conversion.
    #[must_use]
    pub fn get(self) -> jiff::Timestamp {
        self.0
    }
    /// Advances UTC by a checked elapsed duration.
    /// # Errors
    /// Returns the original jiff range error.
    pub fn checked_add(self, duration: Duration) -> Result<Self, AdvanceTimeError> {
        self.0
            .checked_add(duration)
            .map(Self)
            .map_err(|source| AdvanceTimeError::Utc { source })
    }
    /// Subtracts a checked elapsed duration.
    /// # Errors
    /// Returns the original jiff range error.
    pub fn checked_sub(self, duration: Duration) -> Result<Self, AdvanceTimeError> {
        self.0
            .checked_sub(duration)
            .map(Self)
            .map_err(|source| AdvanceTimeError::Utc { source })
    }
    /// Returns elapsed wall-clock time between ordered UTC positions.
    /// # Errors
    /// Returns an error if the earlier value is after this instant.
    pub fn duration_since(self, earlier: Self) -> Result<Duration, AdvanceTimeError> {
        Duration::try_from(self.0.duration_since(earlier.0))
            .map_err(|_| AdvanceTimeError::Monotonic)
    }
    /// Returns the next civil midnight in UTC.
    /// # Errors
    /// Returns a jiff range error at the end of its supported calendar.
    pub fn next_midnight(self) -> Result<Self, AdvanceTimeError> {
        self.0
            .to_zoned_datetime(TimeZone::UTC)
            .date()
            .tomorrow()
            .and_then(|date| date.at(0, 0, 0, 0).to_zoned_datetime(TimeZone::UTC))
            .map(|date| Self(date.timestamp()))
            .map_err(|source| AdvanceTimeError::Utc { source })
    }
}
impl TryFrom<i64> for UtcInstant {
    type Error = AdvanceTimeError;
    fn try_from(value: i64) -> Result<Self, Self::Error> {
        jiff::Timestamp::from_millisecond(value)
            .map(Self)
            .map_err(|source| AdvanceTimeError::Utc { source })
    }
}

/// Unbiased interrupt position in milliseconds since the operating system's origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonoInstant(u64);
impl MonoInstant {
    /// Constructs a reading supplied by the platform shell.
    #[must_use]
    pub fn from_millis(value: u64) -> Self {
        Self(value)
    }
    /// Returns the platform reading for timer and storage adapters.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
    /// Adds a duration, checking conversion and overflow.
    /// # Errors
    /// Returns an error if the result exceeds the monotonic range.
    pub fn checked_add(self, duration: Duration) -> Result<Self, AdvanceTimeError> {
        if !duration.subsec_nanos().is_multiple_of(1_000_000) {
            return Err(AdvanceTimeError::Submillisecond);
        }
        let delta = u64::try_from(duration.as_millis()).map_err(|_| AdvanceTimeError::Monotonic)?;
        self.0
            .checked_add(delta)
            .map(Self)
            .ok_or(AdvanceTimeError::Monotonic)
    }
    /// Subtracts a duration, checking conversion and underflow.
    /// # Errors
    /// Returns an error if the result precedes the clock origin.
    pub fn checked_sub(self, duration: Duration) -> Result<Self, AdvanceTimeError> {
        if !duration.subsec_nanos().is_multiple_of(1_000_000) {
            return Err(AdvanceTimeError::Submillisecond);
        }
        let delta = u64::try_from(duration.as_millis()).map_err(|_| AdvanceTimeError::Monotonic)?;
        self.0
            .checked_sub(delta)
            .map(Self)
            .ok_or(AdvanceTimeError::Monotonic)
    }
    /// Measures an ordered pair of monotonic readings.
    /// # Errors
    /// Returns an error when the earlier reading is later than this reading.
    pub fn duration_since(self, earlier: Self) -> Result<Duration, AdvanceTimeError> {
        self.0
            .checked_sub(earlier.0)
            .map(Duration::from_millis)
            .ok_or(AdvanceTimeError::Monotonic)
    }
    pub(super) fn checked_add_rounded_up(
        self,
        duration: Duration,
    ) -> Result<Self, AdvanceTimeError> {
        let millis =
            duration.as_millis() + u128::from(!duration.subsec_nanos().is_multiple_of(1_000_000));
        let millis = u64::try_from(millis).map_err(|_| AdvanceTimeError::Monotonic)?;
        self.checked_add(Duration::from_millis(millis))
    }
    pub(super) fn advance(self, duration: Duration) -> Self {
        self.checked_add(duration)
            .expect("capture arithmetic was validated before the transition")
    }
    pub(super) fn retreat_to_origin(self, duration: Duration) -> Self {
        match self.checked_sub(duration) {
            Ok(at) => at,
            Err(AdvanceTimeError::Monotonic) => Self::from_millis(0),
            Err(AdvanceTimeError::Utc { .. } | AdvanceTimeError::Submillisecond) => {
                unreachable!("monotonic subtraction cannot return a UTC error")
            }
        }
    }
}

/// Paired wall-clock and unbiased interrupt readings supplied by the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    /// UTC positioning of this observation.
    pub utc: UtcInstant,
    /// Unbiased interrupt position, excluding suspended time.
    pub mono: MonoInstant,
}
impl Timestamp {
    /// Reserves room for midnight splitting, input history, and monotonic deadlines.
    pub(super) fn validate_capture(self) -> Result<(), AdvanceTimeError> {
        i64::try_from(self.utc)?;
        self.utc.checked_add(limits::CAPTURE_UTC_HEADROOM)?;
        self.utc
            .checked_sub(Duration::from_millis(u64::from(u32::MAX)))?;
        let utc_range =
            Duration::try_from(jiff::Timestamp::MAX.duration_since(jiff::Timestamp::MIN))
                .map_err(|_| AdvanceTimeError::Monotonic)?;
        self.mono.checked_add_rounded_up(utc_range)?;
        Ok(())
    }
    /// Projects a monotonic reading through this clock-segment anchor.
    /// # Errors
    /// Returns an error for backwards monotonic time or UTC overflow.
    pub fn checked_project(self, mono: MonoInstant) -> Result<Self, AdvanceTimeError> {
        Ok(Self {
            utc: self.utc.checked_add(mono.duration_since(self.mono)?)?,
            mono,
        })
    }
    /// Advances both clock positions by an elapsed duration.
    /// # Errors
    /// Returns an error if either clock exceeds its supported range.
    pub fn checked_advance(self, duration: Duration) -> Result<Self, AdvanceTimeError> {
        self.checked_project(self.mono.checked_add(duration)?)
    }
    /// Returns the next civil UTC midnight.
    /// # Errors
    /// Returns an error at the end of jiff's calendar.
    pub fn next_midnight(self) -> Result<UtcInstant, AdvanceTimeError> {
        self.utc.next_midnight()
    }
    /// Tests the strict clock-segment drift tolerance from capture 02 §5.
    #[must_use]
    pub fn drifted_from(self, anchor: Self) -> bool {
        let wall = self.utc.get().duration_since(anchor.utc.get()).as_millis();
        let mono = i128::from(self.mono.get()) - i128::from(anchor.mono.get());
        (wall - mono).unsigned_abs() > limits::CLOCK_DRIFT.as_millis()
    }
    pub(super) fn project(self, mono: MonoInstant) -> Self {
        self.checked_project(mono)
            .expect("clock projection was validated before the transition")
    }
}

/// Half-open range belonging to one clock segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    /// Inclusive start.
    pub start: Timestamp,
    /// Exclusive end.
    pub end: Timestamp,
}
impl TimeRange {
    /// Projects a UTC cut onto this row, clamping it to the row's elapsed range.
    /// A zero UTC span selects the row start.
    /// # Errors
    /// Returns an error for reversed endpoints or arithmetic outside the supported range.
    pub fn boundary_at(self, utc: UtcInstant) -> Result<Timestamp, AdvanceTimeError> {
        let wall = self.end.utc.duration_since(self.start.utc)?;
        let elapsed = self.end.mono.duration_since(self.start.mono)?;
        let utc = utc.clamp(self.start.utc, self.end.utc);
        if wall.is_zero() {
            return Ok(self.start);
        }
        let offset = utc.duration_since(self.start.utc)?;
        let millis = elapsed
            .as_millis()
            .checked_mul(offset.as_nanos())
            .ok_or(AdvanceTimeError::Monotonic)?
            / wall.as_nanos();
        let delta = u64::try_from(millis).map_err(|_| AdvanceTimeError::Monotonic)?;
        Ok(Timestamp {
            utc,
            mono: self
                .start
                .mono
                .checked_add(Duration::from_millis(delta))?
                .clamp(self.start.mono, self.end.mono),
        })
    }
    /// Splits at UTC civil midnights, preserving suspended ranges with equal monotonic readings.
    /// # Errors
    /// Returns an error for reversed endpoints or arithmetic outside the supported range.
    pub fn split_midnights(self) -> Result<Vec<Self>, AdvanceTimeError> {
        self.end.utc.duration_since(self.start.utc)?;
        self.end.mono.duration_since(self.start.mono)?;
        let mut pieces = Vec::new();
        let mut start = self.start;
        while start.next_midnight()? < self.end.utc {
            let end = self.boundary_at(start.next_midnight()?)?;
            pieces.push(Self { start, end });
            start = end;
        }
        pieces.push(Self {
            start,
            end: self.end,
        });
        Ok(pieces)
    }
}

impl std::fmt::Display for MonoInstant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<i64> for MonoInstant {
    type Error = AdvanceTimeError;
    fn try_from(value: i64) -> Result<Self, Self::Error> {
        u64::try_from(value)
            .map(Self::from_millis)
            .map_err(|_| AdvanceTimeError::Monotonic)
    }
}
impl TryFrom<MonoInstant> for i64 {
    type Error = AdvanceTimeError;
    fn try_from(value: MonoInstant) -> Result<Self, Self::Error> {
        i64::try_from(value.get()).map_err(|_| AdvanceTimeError::Monotonic)
    }
}
impl TryFrom<UtcInstant> for i64 {
    type Error = AdvanceTimeError;
    fn try_from(value: UtcInstant) -> Result<Self, Self::Error> {
        if value.get().as_nanosecond() % 1_000_000 != 0 {
            return Err(AdvanceTimeError::Submillisecond);
        }
        Ok(value.get().as_millisecond())
    }
}
