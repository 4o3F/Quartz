//! Context attached to rejected capture values and persisted rows.

use super::{Capture, DbRowId, LogicalKey};
use crate::store::identifiers::{ColumnName, SchemaColumn, TableName};
use quartz_domain::capture::{self as domain, RunId, Timestamp};
use snafu::Snafu;
use std::{num::TryFromIntError, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegerTarget {
    U32,
    U64,
}
impl std::fmt::Display for IntegerTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::U32 => std::any::type_name::<u32>(),
            Self::U64 => std::any::type_name::<u64>(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CaptureColumn {
    pub table: TableName,
    pub column: ColumnName,
    pub row_id: Option<i64>,
}
impl std::fmt::Display for CaptureColumn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.row_id {
            Some(id) => write!(f, "{}.{} (row {id})", self.table, self.column),
            None => write!(f, "{}.{}", self.table, self.column),
        }
    }
}

#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum DecodeCaptureRowError {
    #[snafu(display("stored timed policy exceeds its single-grant limit"))]
    ExcessiveTimedGrant,
    #[snafu(display("invalid application identity at {field}"))]
    ApplicationIdentity { field: CaptureColumn },
    #[snafu(display("missing value at {field}"))]
    MissingValue { field: CaptureColumn },
    #[snafu(display("invalid integer {value} at {field}; expected {target}"))]
    Integer {
        field: CaptureColumn,
        value: i64,
        target: IntegerTarget,
        source: TryFromIntError,
    },
    #[snafu(display("invalid time {value} at {field}"))]
    Time {
        field: CaptureColumn,
        value: i64,
        source: domain::AdvanceTimeError,
    },
    #[snafu(display("confirmation {confirmed} precedes start {start} at {field}"))]
    Confirmation {
        field: CaptureColumn,
        start: i128,
        confirmed: i128,
        source: domain::AdvanceTimeError,
    },
    #[snafu(display("identifier space exhausted at {field}: {value}"))]
    ExhaustedIdentifier { field: CaptureColumn, value: i64 },
    #[snafu(display("unsupported enum code {value} at {field}"))]
    Enum {
        field: CaptureColumn,
        value: i32,
        source: crate::store::enums::ConvertTitleStatusError,
    },
    #[snafu(display("invalid deadline {value} in settings[{key}]", key = Capture::deadline_key(*owner)))]
    Deadline {
        owner: domain::SchedulerKind,
        value: String,
        source: std::num::ParseIntError,
    },
}

#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum ExecuteCaptureBatchError {
    #[snafu(display("invalid metadata version in settings"))]
    MetadataVersion,
    #[snafu(display("could not execute capture writes"))]
    Database { source: diesel::result::Error },
    #[snafu(display("could not decode a persisted capture row before writing"))]
    Decode { source: DecodeCaptureRowError },
    #[snafu(display("invalid capture time {value} at {field}"))]
    Time {
        field: CaptureColumn,
        value: CaptureTimeValue,
        source: domain::AdvanceTimeError,
    },
    #[snafu(display("integer {value} exceeds the storage range at {field}"))]
    Integer {
        field: CaptureColumn,
        value: u128,
        source: TryFromIntError,
    },
    #[snafu(display("unknown logical key {key} in run {run}", run = run.get()))]
    MissingKey { run: RunId, key: LogicalKey },
    #[snafu(display("duplicate key {key} in run {run}: {existing} and {replacement}", run = run.get()))]
    DuplicateKey {
        run: RunId,
        key: LogicalKey,
        existing: i64,
        replacement: i64,
    },
    #[snafu(display("no logical key for {table} row {row_id} in run {run}", run = run.get()))]
    MissingMapping {
        run: RunId,
        table: TableName,
        row_id: i64,
    },
    #[snafu(display("expected one open {table} row {row_id}, affected {affected_rows}"))]
    MissingOpenRow {
        table: TableName,
        row_id: i64,
        affected_rows: usize,
    },
    #[snafu(display("invalid capture range at {field}: UTC {start_utc} to {end_utc}, monotonic {start_mono} to {end_mono}", start_utc = start.utc.get(), end_utc = end.utc.get(), start_mono = start.mono.get(), end_mono = end.mono.get()))]
    Range {
        field: CaptureColumn,
        start: Timestamp,
        end: Timestamp,
        source: domain::AdvanceTimeError,
    },
}
impl From<diesel::result::Error> for ExecuteCaptureBatchError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}
impl From<DecodeCaptureRowError> for ExecuteCaptureBatchError {
    fn from(source: DecodeCaptureRowError) -> Self {
        Self::Decode { source }
    }
}
#[derive(Debug)]
pub(crate) enum CaptureTimeValue {
    Utc(jiff::Timestamp),
    Monotonic(u64),
    Duration(Duration),
}
impl std::fmt::Display for CaptureTimeValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Utc(value) => write!(f, "{value}"),
            Self::Monotonic(value) => write!(f, "{value}ms"),
            Self::Duration(value) => write!(f, "{}ns", value.as_nanos()),
        }
    }
}
impl CaptureColumn {
    pub(super) fn new<C: SchemaColumn>(column: C) -> Self {
        Self {
            table: C::TABLE,
            column: ColumnName::of(column),
            row_id: None,
        }
    }
    pub(super) fn integer(self, value: u64) -> Result<i64, ExecuteCaptureBatchError> {
        i64::try_from(value).map_err(|source| ExecuteCaptureBatchError::Integer {
            field: self,
            value: u128::from(value),
            source,
        })
    }
    pub(super) fn duration(self, value: Duration) -> Result<i64, ExecuteCaptureBatchError> {
        domain::MonoInstant::from_millis(0)
            .checked_add(value)
            .and_then(i64::try_from)
            .map_err(|source| ExecuteCaptureBatchError::Time {
                field: self,
                value: CaptureTimeValue::Duration(value),
                source,
            })
    }
    pub(super) fn utc(self, value: domain::UtcInstant) -> Result<i64, ExecuteCaptureBatchError> {
        i64::try_from(value).map_err(|source| ExecuteCaptureBatchError::Time {
            field: self,
            value: CaptureTimeValue::Utc(value.get()),
            source,
        })
    }
    pub(super) fn mono(self, value: domain::MonoInstant) -> Result<i64, ExecuteCaptureBatchError> {
        i64::try_from(value).map_err(|source| ExecuteCaptureBatchError::Time {
            field: self,
            value: CaptureTimeValue::Monotonic(value.get()),
            source,
        })
    }
    pub(super) fn range(
        self,
        start: Timestamp,
        end: Timestamp,
    ) -> Result<Duration, ExecuteCaptureBatchError> {
        end.utc
            .duration_since(start.utc)
            .and_then(|_| end.mono.duration_since(start.mono))
            .map_err(|source| ExecuteCaptureBatchError::Range {
                field: self,
                start,
                end,
                source,
            })
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RowContext {
    id: Option<DbRowId>,
}
#[derive(Debug, Clone, Copy)]
pub(super) struct TimeColumns<U, M> {
    utc: U,
    mono: M,
}
impl<U: SchemaColumn, M: SchemaColumn<Table = U::Table>> TimeColumns<U, M> {
    pub(super) fn new(utc: U, mono: M) -> Self {
        Self { utc, mono }
    }
}
impl RowContext {
    pub(super) fn new(id: Option<i64>) -> Self {
        Self {
            id: id.map(DbRowId::new),
        }
    }
    pub(super) fn field<C: SchemaColumn>(self, column: C) -> CaptureColumn {
        CaptureColumn {
            table: C::TABLE,
            column: ColumnName::of(column),
            row_id: self.id.map(DbRowId::get),
        }
    }
    pub(super) fn required<T>(
        self,
        value: Option<T>,
        column: impl SchemaColumn,
    ) -> Result<T, DecodeCaptureRowError> {
        value.ok_or(DecodeCaptureRowError::MissingValue {
            field: self.field(column),
        })
    }
    pub(super) fn unsigned(
        self,
        value: i64,
        column: impl SchemaColumn,
    ) -> Result<u64, DecodeCaptureRowError> {
        u64::try_from(value).map_err(|source| DecodeCaptureRowError::Integer {
            field: self.field(column),
            value,
            target: IntegerTarget::U64,
            source,
        })
    }
    pub(super) fn policy_version(
        self,
        value: i64,
        column: impl SchemaColumn,
    ) -> Result<u32, DecodeCaptureRowError> {
        u32::try_from(value).map_err(|source| DecodeCaptureRowError::Integer {
            field: self.field(column),
            value,
            target: IntegerTarget::U32,
            source,
        })
    }
    pub(super) fn utc(
        self,
        value: i64,
        column: impl SchemaColumn,
    ) -> Result<domain::UtcInstant, DecodeCaptureRowError> {
        value
            .try_into()
            .map_err(|source| DecodeCaptureRowError::Time {
                field: self.field(column),
                value,
                source,
            })
    }
    pub(super) fn mono(
        self,
        value: i64,
        column: impl SchemaColumn,
    ) -> Result<domain::MonoInstant, DecodeCaptureRowError> {
        value
            .try_into()
            .map_err(|source| DecodeCaptureRowError::Time {
                field: self.field(column),
                value,
                source,
            })
    }
    pub(super) fn point<U: SchemaColumn, M: SchemaColumn<Table = U::Table>>(
        self,
        utc: i64,
        mono: i64,
        columns: TimeColumns<U, M>,
    ) -> Result<Timestamp, DecodeCaptureRowError> {
        Ok(Timestamp {
            utc: self.utc(utc, columns.utc)?,
            mono: self.mono(mono, columns.mono)?,
        })
    }
    pub(super) fn confirmation<U: SchemaColumn, M: SchemaColumn<Table = U::Table>>(
        self,
        start: Timestamp,
        confirmed: Timestamp,
        columns: TimeColumns<U, M>,
    ) -> Result<(), DecodeCaptureRowError> {
        confirmed.utc.duration_since(start.utc).map_err(|source| {
            DecodeCaptureRowError::Confirmation {
                field: self.field(columns.utc),
                start: i128::from(start.utc.get().as_millisecond()),
                confirmed: i128::from(confirmed.utc.get().as_millisecond()),
                source,
            }
        })?;
        confirmed
            .mono
            .duration_since(start.mono)
            .map_err(|source| DecodeCaptureRowError::Confirmation {
                field: self.field(columns.mono),
                start: i128::from(start.mono.get()),
                confirmed: i128::from(confirmed.mono.get()),
                source,
            })?;
        Ok(())
    }
    pub(super) fn next_identifier(
        self,
        value: i64,
        column: impl SchemaColumn,
    ) -> Result<i64, DecodeCaptureRowError> {
        value
            .checked_add(1)
            .ok_or(DecodeCaptureRowError::ExhaustedIdentifier {
                field: self.field(column),
                value,
            })
    }
}

impl RowContext {
    pub(super) fn confirmed<U: SchemaColumn, M: SchemaColumn<Table = U::Table>>(
        self,
        start: Timestamp,
        utc: Option<i64>,
        mono: Option<i64>,
        columns: TimeColumns<U, M>,
    ) -> Result<Timestamp, DecodeCaptureRowError> {
        let point = self.point(
            self.required(utc, columns.utc)?,
            self.required(mono, columns.mono)?,
            columns,
        )?;
        self.confirmation(start, point, columns)?;
        Ok(point)
    }
    pub(super) fn prior_point(
        self,
        start: Timestamp,
        value: i64,
        column: impl SchemaColumn,
    ) -> Result<Timestamp, DecodeCaptureRowError> {
        let utc = self.utc(value, column)?;
        let mono = start
            .utc
            .duration_since(utc)
            .and_then(|duration| start.mono.checked_sub(duration))
            .map_err(|source| DecodeCaptureRowError::Time {
                field: self.field(column),
                value,
                source,
            })?;
        Ok(Timestamp { utc, mono })
    }
    pub(super) fn later_point(
        self,
        start: Timestamp,
        value: i64,
        column: impl SchemaColumn,
    ) -> Result<Timestamp, DecodeCaptureRowError> {
        let utc = self.utc(value, column)?;
        let mono = utc
            .duration_since(start.utc)
            .and_then(|duration| start.mono.checked_add(duration))
            .map_err(|source| DecodeCaptureRowError::Time {
                field: self.field(column),
                value,
                source,
            })?;
        Ok(Timestamp { utc, mono })
    }
}
