//! Storage inspection, manual backups, compaction, and atomic date-range deletion (03 §6).

use super::{
    BackupName, Store, WriteTx,
    annotations::clipping::{DeleteSplitIdentity, Remainders},
    enums::{
        AvailabilityEndReason, AvailabilityReason, CaptureLevel, DataSource, IntervalEndReason,
        IntervalKind, TitleStatus,
    },
    raw::ConnectionSql,
    rows::{
        AnnotationsRow, AvailabilityIntervalsRow, CaptureGapsRow, IntervalsRow, WebIntervalsRow,
    },
    schema::{annotations, availability_intervals, capture_gaps, intervals, web_intervals},
    versions::{BumpVersionError, UtcDay, UtcDays},
};
use crate::data_directory::DataDirectory;
use diesel::{
    BoolExpressionMethods, ExpressionMethods, NullableExpressionMethods, QueryDsl, RunQueryDsl,
    SelectableHelper,
};
use quartz_domain::{
    capture::{UtcInstant, limits::UTC_DAY_MS},
    facts::QueryRange,
};
use quartz_protocol::{
    AnnotationId, BackupEntry, BackupKind, DataCompactResponse, DataInfoResponse,
};
use snafu::{OptionExt, ResultExt, Snafu};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// `web_intervals.end_reason` code for `policy_changed` (schema v1 `enum_labels`).
const WEB_END_REASON_POLICY_CHANGED: i32 = 5;
/// Exact ASCII length of `YYYYMMDDTHHMMSSZ` in manual backup file names.
const MANUAL_TIMESTAMP_LEN: usize = 16;
/// Maximum UTC days processed in one title-retention write transaction so large purges do not
/// hold SQLite's write lock across an entire year of rows.
const TITLE_PURGE_CHUNK_DAYS: i64 = 30;
/// Millisecond width of one title-retention purge chunk.
const TITLE_PURGE_CHUNK_MS: i64 = TITLE_PURGE_CHUNK_DAYS * UTC_DAY_MS;

#[derive(Debug, Snafu)]
#[snafu(module(read_data_info_error))]
pub(crate) enum ReadDataInfoError {
    #[snafu(display("storage path is not valid Unicode"))]
    InvalidPath,
    #[snafu(display("could not inspect database file {}: {source}", path.display()))]
    ReadDatabaseMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not inspect WAL file {}: {source}", path.display()))]
    ReadWalMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not list backups directory: {source}"))]
    ReadBackupsDirectory { source: std::io::Error },
    #[snafu(display("could not read backup directory entry: {source}"))]
    ReadBackupEntry { source: std::io::Error },
    #[snafu(display("could not inspect backup entry {}: {source}", path.display()))]
    InspectBackupEntry {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Snafu)]
#[snafu(module(create_manual_backup_error))]
pub(crate) enum CreateManualBackupError {
    #[snafu(display("backup timestamp cannot be represented as a UTC calendar instant"))]
    InvalidTimestamp,
    #[snafu(display("backup path is not valid Unicode"))]
    InvalidPath,
    #[snafu(display("could not create backups directory: {source}"))]
    CreateBackupDirectory { source: std::io::Error },
    #[snafu(display("could not inspect backup target {}: {source}", path.display()))]
    CheckBackupTarget {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("manual backup already exists: {}", path.display()))]
    AlreadyExists { path: PathBuf },
    #[snafu(display("could not write manual backup {}: {source}", path.display()))]
    ExecuteBackup {
        path: PathBuf,
        source: diesel::result::Error,
    },
    #[snafu(display("could not inspect written backup {}: {source}", path.display()))]
    ReadBackupMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Snafu)]
#[snafu(module(compact_database_error))]
pub(crate) enum CompactDatabaseError {
    #[snafu(display("could not inspect database file {}: {source}", path.display()))]
    ReadDatabaseMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not inspect WAL file {}: {source}", path.display()))]
    ReadWalMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not vacuum database: {source}"))]
    Vacuum { source: diesel::result::Error },
    #[snafu(display("could not truncate WAL after vacuum: {source}"))]
    Checkpoint { source: diesel::result::Error },
}

#[derive(Debug, Snafu)]
#[snafu(module(delete_range_error))]
pub(crate) enum DeleteRangeError {
    #[snafu(display("delete range must be non-empty"))]
    EmptyRange,
    #[snafu(display("stored row in {table} violated time or identity invariants"))]
    InvalidStoredRow { table: &'static str },
    #[snafu(display("stored annotation UUID is invalid: {source}"))]
    InvalidAnnotationUuid {
        source: quartz_protocol::ParseUuidError,
    },
    #[snafu(display("could not delete range from database: {source}"))]
    Database { source: diesel::result::Error },
    #[snafu(display("could not bump data day versions: {source}"))]
    Version { source: BumpVersionError },
    #[snafu(display("could not truncate WAL after range deletion: {source}"))]
    Checkpoint { source: diesel::result::Error },
}

impl From<diesel::result::Error> for DeleteRangeError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}

impl From<BumpVersionError> for DeleteRangeError {
    fn from(source: BumpVersionError) -> Self {
        Self::Version { source }
    }
}

#[derive(Debug, Snafu)]
#[snafu(module(purge_titles_error))]
pub(crate) enum PurgeTitlesError {
    #[snafu(display("stored row in {table} violated time invariants during title purge"))]
    InvalidPurgeRow { table: &'static str },
    #[snafu(display("could not purge expired titles from database: {source}"))]
    PurgeDatabase { source: diesel::result::Error },
    #[snafu(display("could not bump data day versions after title purge: {source}"))]
    PurgeVersion { source: BumpVersionError },
    #[snafu(display("could not truncate WAL after title purge: {source}"))]
    PurgeCheckpoint { source: diesel::result::Error },
}

impl From<diesel::result::Error> for PurgeTitlesError {
    fn from(source: diesel::result::Error) -> Self {
        Self::PurgeDatabase { source }
    }
}

impl From<BumpVersionError> for PurgeTitlesError {
    fn from(source: BumpVersionError) -> Self {
        Self::PurgeVersion { source }
    }
}

/// Counts of rows and dictionary entries cleared by `Store::purge_expired_titles`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PurgeTitlesSummary {
    /// Closed `intervals` rows whose `title_id` was cleared and marked `purged`.
    pub intervals_purged: usize,
    /// Closed `web_intervals` rows whose `tab_title_id` was cleared and marked `purged`.
    pub web_intervals_purged: usize,
    /// Unreferenced rows deleted from `titles`.
    pub titles_deleted: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PurgeChunkPosition {
    Intermediate,
    Final,
}

/// Minimal summary returned by `Store::delete_range`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeleteRangeSummary {
    /// Active foreground window milliseconds removed from `intervals`.
    pub deleted_ms: i64,
}

#[derive(Debug, PartialEq, Eq)]
struct ManualBackupName {
    file_name: String,
    created_utc_ms: i64,
}

impl ManualBackupName {
    fn from_instant(now: UtcInstant) -> Result<Self, CreateManualBackupError> {
        let utc = now.get().to_zoned_datetime(jiff::tz::TimeZone::UTC);
        if !(0..=9999).contains(&utc.year()) {
            return Err(CreateManualBackupError::InvalidTimestamp);
        }
        let file_name = format!(
            "manual-{:04}{:02}{:02}T{:02}{:02}{:02}Z.db",
            utc.year(),
            utc.month(),
            utc.day(),
            utc.hour(),
            utc.minute(),
            utc.second()
        );
        let parsed = Self::parse(&file_name).ok_or(CreateManualBackupError::InvalidTimestamp)?;
        Ok(parsed)
    }

    fn parse(name: &str) -> Option<Self> {
        let body = name.strip_prefix("manual-")?.strip_suffix(".db")?;
        if body.len() != MANUAL_TIMESTAMP_LEN || !body.is_ascii() {
            return None;
        }
        let bytes = body.as_bytes();
        if bytes[8] != b'T' || bytes[15] != b'Z' {
            return None;
        }
        if !bytes[..8].iter().all(u8::is_ascii_digit)
            || !bytes[9..15].iter().all(u8::is_ascii_digit)
        {
            return None;
        }
        let iso = format!(
            "{}-{}-{}T{}:{}:{}Z",
            &body[0..4],
            &body[4..6],
            &body[6..8],
            &body[9..11],
            &body[11..13],
            &body[13..15]
        );
        let timestamp: jiff::Timestamp = iso.parse().ok()?;
        Some(Self {
            file_name: name.to_owned(),
            created_utc_ms: timestamp.as_millisecond(),
        })
    }
}

fn classify_backup(file_name: &str) -> Option<(BackupKind, i64)> {
    if let Some(migration) = BackupName::parse(file_name) {
        return Some((BackupKind::Migration, migration.timestamp));
    }
    if let Some(manual) = ManualBackupName::parse(file_name) {
        return Some((BackupKind::Manual, manual.created_utc_ms));
    }
    None
}

fn path_to_string(path: &Path) -> Option<String> {
    path.to_str().map(ToOwned::to_owned)
}

fn measure_database_bytes(
    directory: &DataDirectory,
) -> Result<u64, (PathBuf, std::io::Error, FileKind)> {
    let database = directory.database();
    let main_bytes = fs::metadata(&database)
        .map_err(|source| (database, source, FileKind::Main))?
        .len();
    let wal = directory.database_wal();
    let wal_bytes = match fs::metadata(&wal) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(source) => return Err((wal, source, FileKind::Wal)),
    };
    Ok(main_bytes.saturating_add(wal_bytes))
}

#[derive(Debug, Clone, Copy)]
enum FileKind {
    Main,
    Wal,
}

fn map_info_size_error(
    (path, source, kind): (PathBuf, std::io::Error, FileKind),
) -> ReadDataInfoError {
    match kind {
        FileKind::Main => ReadDataInfoError::ReadDatabaseMetadata { path, source },
        FileKind::Wal => ReadDataInfoError::ReadWalMetadata { path, source },
    }
}

fn map_compact_size_error(
    (path, source, kind): (PathBuf, std::io::Error, FileKind),
) -> CompactDatabaseError {
    match kind {
        FileKind::Main => CompactDatabaseError::ReadDatabaseMetadata { path, source },
        FileKind::Wal => CompactDatabaseError::ReadWalMetadata { path, source },
    }
}

impl Store {
    /// Returns storage paths, main + `-wal` size, and recognized backups sorted newest first.
    pub(crate) fn data_info(&self) -> Result<DataInfoResponse, ReadDataInfoError> {
        let database = self.directory.database();
        let logs = self.directory.logs();
        let backups_dir = self.directory.backups();
        let database_path =
            path_to_string(&database).context(read_data_info_error::InvalidPathSnafu)?;
        let logs_path = path_to_string(&logs).context(read_data_info_error::InvalidPathSnafu)?;
        let backups_path =
            path_to_string(&backups_dir).context(read_data_info_error::InvalidPathSnafu)?;
        let database_bytes =
            measure_database_bytes(&self.directory).map_err(map_info_size_error)?;
        let mut backups = Vec::new();
        match fs::read_dir(&backups_dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.context(read_data_info_error::ReadBackupEntrySnafu)?;
                    let path = entry.path();
                    if !entry
                        .file_type()
                        .context(read_data_info_error::InspectBackupEntrySnafu { path: &path })?
                        .is_file()
                    {
                        continue;
                    }
                    let Some(file_name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                        continue;
                    };
                    let Some((kind, created_utc_ms)) = classify_backup(&file_name) else {
                        continue;
                    };
                    let bytes = entry
                        .metadata()
                        .context(read_data_info_error::InspectBackupEntrySnafu { path: &path })?
                        .len();
                    backups.push(BackupEntry {
                        file_name,
                        bytes,
                        created_utc_ms,
                        kind,
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(ReadDataInfoError::ReadBackupsDirectory { source }),
        }
        backups.sort_by(|left, right| {
            right
                .created_utc_ms
                .cmp(&left.created_utc_ms)
                .then_with(|| left.file_name.cmp(&right.file_name))
        });
        Ok(DataInfoResponse {
            database_path,
            database_bytes,
            logs_path,
            backups_path,
            backups,
        })
    }

    /// Writes `backups/manual-<UTC yyyymmddThhmmssZ>.db` via `VACUUM INTO` without overwriting.
    pub(crate) fn create_manual_backup(
        &mut self,
        now: UtcInstant,
    ) -> Result<BackupEntry, CreateManualBackupError> {
        let name = ManualBackupName::from_instant(now)?;
        let backups_dir = self.directory.backups();
        fs::create_dir_all(&backups_dir)
            .context(create_manual_backup_error::CreateBackupDirectorySnafu)?;
        let target = backups_dir.join(&name.file_name);
        if target
            .try_exists()
            .context(create_manual_backup_error::CheckBackupTargetSnafu { path: &target })?
        {
            return Err(CreateManualBackupError::AlreadyExists { path: target });
        }
        let target_str = target
            .to_str()
            .context(create_manual_backup_error::InvalidPathSnafu)?;
        ConnectionSql::new(&mut self.writer)
            .backup(target_str)
            .context(create_manual_backup_error::ExecuteBackupSnafu { path: &target })?;
        let bytes = fs::metadata(&target)
            .context(create_manual_backup_error::ReadBackupMetadataSnafu { path: &target })?
            .len();
        tracing::debug!(backup = %target.display(), bytes, "created manual database backup");
        Ok(BackupEntry {
            file_name: name.file_name,
            bytes,
            created_utc_ms: name.created_utc_ms,
            kind: BackupKind::Manual,
        })
    }

    /// Runs `VACUUM` then `PRAGMA wal_checkpoint(TRUNCATE)` on the write connection.
    pub(crate) fn compact(&mut self) -> Result<DataCompactResponse, CompactDatabaseError> {
        let before_bytes =
            measure_database_bytes(&self.directory).map_err(map_compact_size_error)?;
        ConnectionSql::new(&mut self.writer)
            .vacuum()
            .context(compact_database_error::VacuumSnafu)?;
        let truncated = ConnectionSql::new(&mut self.writer)
            .checkpoint()
            .context(compact_database_error::CheckpointSnafu)?;
        if !truncated {
            tracing::warn!("WAL truncation after compact was blocked by an active reader");
        }
        let after_bytes =
            measure_database_bytes(&self.directory).map_err(map_compact_size_error)?;
        tracing::debug!(before_bytes, after_bytes, "compacted database");
        Ok(DataCompactResponse {
            before_bytes,
            after_bytes,
        })
    }

    /// Atomically deletes and clips all fact tables across `[range.start, range.end)` UTC,
    /// removes unreferenced titles/domains, bumps affected day versions, and truncates the WAL.
    pub(crate) fn delete_range(
        &mut self,
        range: QueryRange,
        now: UtcInstant,
    ) -> Result<DeleteRangeSummary, DeleteRangeError> {
        if range.start >= range.end {
            return Err(DeleteRangeError::EmptyRange);
        }
        let summary = self.write(|tx| delete_range_in_tx(tx, range, now))?;
        let truncated = ConnectionSql::new(&mut self.writer)
            .checkpoint()
            .context(delete_range_error::CheckpointSnafu)?;
        if !truncated {
            tracing::warn!("WAL truncation after delete_range was blocked by an active reader");
        }
        Ok(summary)
    }

    /// Clears `title_id` / `tab_title_id` and marks `purged` on closed `intervals` and
    /// `web_intervals` ending at or before `cutoff_utc_ms` (whole-row granularity: rows
    /// straddling `cutoff_utc_ms` and open rows are left untouched), deletes unreferenced
    /// `titles`, bumps affected UTC-day versions, and truncates the WAL after commit.
    ///
    /// Large historical backlogs are committed in chunks of [`TITLE_PURGE_CHUNK_DAYS`] UTC days
    /// so SQLite's write lock stays bounded; small or incremental purges complete in one
    /// write transaction.
    pub(crate) fn purge_expired_titles(
        &mut self,
        cutoff_utc_ms: i64,
    ) -> Result<PurgeTitlesSummary, PurgeTitlesError> {
        let mut cursor = next_eligible_title_start(&mut self.writer, cutoff_utc_ms, None)?;
        if cursor.is_none() {
            return Ok(PurgeTitlesSummary::default());
        }
        let mut summary = PurgeTitlesSummary::default();
        while let Some(chunk_start) = cursor {
            let chunk_end = chunk_start
                .checked_add(TITLE_PURGE_CHUNK_MS - 1)
                .map_or(cutoff_utc_ms, |end| end.min(cutoff_utc_ms));
            let next_cursor = if chunk_end >= cutoff_utc_ms {
                None
            } else {
                next_eligible_title_start(&mut self.writer, cutoff_utc_ms, Some(chunk_end))?
            };
            let position = if next_cursor.is_none() {
                PurgeChunkPosition::Final
            } else {
                PurgeChunkPosition::Intermediate
            };
            let chunk_summary = self.write(|tx| {
                purge_titles_chunk_in_tx(tx, chunk_start, chunk_end, cutoff_utc_ms, position)
            })?;
            summary.intervals_purged += chunk_summary.intervals_purged;
            summary.web_intervals_purged += chunk_summary.web_intervals_purged;
            summary.titles_deleted += chunk_summary.titles_deleted;
            cursor = next_cursor;
        }
        if summary.intervals_purged > 0 || summary.web_intervals_purged > 0 {
            let truncated = ConnectionSql::new(&mut self.writer)
                .checkpoint()
                .context(purge_titles_error::PurgeCheckpointSnafu)?;
            if !truncated {
                tracing::warn!("WAL truncation after title purge was blocked by an active reader");
            }
            tracing::debug!(
                cutoff_utc_ms,
                intervals_purged = summary.intervals_purged,
                web_intervals_purged = summary.web_intervals_purged,
                titles_deleted = summary.titles_deleted,
                "purged expired titles"
            );
        }
        Ok(summary)
    }
}

fn next_eligible_title_start(
    connection: &mut diesel::SqliteConnection,
    cutoff_utc_ms: i64,
    after_start_utc_ms: Option<i64>,
) -> Result<Option<i64>, diesel::result::Error> {
    let interval_start = match after_start_utc_ms {
        Some(after) => intervals::table
            .filter(intervals::start_utc_ms.gt(after))
            .filter(intervals::start_utc_ms.le(cutoff_utc_ms))
            .filter(intervals::end_utc_ms.is_not_null())
            .filter(intervals::end_utc_ms.le(cutoff_utc_ms))
            .filter(intervals::title_id.is_not_null())
            .select(diesel::dsl::min(intervals::start_utc_ms))
            .first::<Option<i64>>(connection)?,
        None => intervals::table
            .filter(intervals::start_utc_ms.le(cutoff_utc_ms))
            .filter(intervals::end_utc_ms.is_not_null())
            .filter(intervals::end_utc_ms.le(cutoff_utc_ms))
            .filter(intervals::title_id.is_not_null())
            .select(diesel::dsl::min(intervals::start_utc_ms))
            .first::<Option<i64>>(connection)?,
    };
    let web_start = match after_start_utc_ms {
        Some(after) => web_intervals::table
            .filter(web_intervals::start_utc_ms.gt(after))
            .filter(web_intervals::start_utc_ms.le(cutoff_utc_ms))
            .filter(web_intervals::end_utc_ms.is_not_null())
            .filter(web_intervals::end_utc_ms.le(cutoff_utc_ms))
            .filter(web_intervals::tab_title_id.is_not_null())
            .select(diesel::dsl::min(web_intervals::start_utc_ms))
            .first::<Option<i64>>(connection)?,
        None => web_intervals::table
            .filter(web_intervals::start_utc_ms.le(cutoff_utc_ms))
            .filter(web_intervals::end_utc_ms.is_not_null())
            .filter(web_intervals::end_utc_ms.le(cutoff_utc_ms))
            .filter(web_intervals::tab_title_id.is_not_null())
            .select(diesel::dsl::min(web_intervals::start_utc_ms))
            .first::<Option<i64>>(connection)?,
    };
    Ok(match (interval_start, web_start) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(start), None) | (None, Some(start)) => Some(start),
        (None, None) => None,
    })
}

fn purge_titles_chunk_in_tx(
    tx: &mut WriteTx<'_>,
    chunk_start: i64,
    chunk_end: i64,
    cutoff_utc_ms: i64,
    position: PurgeChunkPosition,
) -> Result<PurgeTitlesSummary, PurgeTitlesError> {
    let mut affected: BTreeMap<DataSource, BTreeSet<UtcDay>> = BTreeMap::new();
    let interval_spans = intervals::table
        .filter(intervals::start_utc_ms.ge(chunk_start))
        .filter(intervals::start_utc_ms.le(chunk_end))
        .filter(intervals::end_utc_ms.is_not_null())
        .filter(intervals::end_utc_ms.le(cutoff_utc_ms))
        .filter(intervals::title_id.is_not_null())
        .select((intervals::start_utc_ms, intervals::end_utc_ms))
        .load::<(i64, Option<i64>)>(tx.connection)?;
    let intervals_purged = if interval_spans.is_empty() {
        0
    } else {
        for (start, end) in interval_spans {
            let end = end.ok_or(PurgeTitlesError::InvalidPurgeRow { table: "intervals" })?;
            if end < start {
                return Err(PurgeTitlesError::InvalidPurgeRow { table: "intervals" });
            }
            record_affected(
                &mut affected,
                DataSource::Intervals,
                QueryRange { start, end },
            );
        }
        diesel::update(
            intervals::table
                .filter(intervals::start_utc_ms.ge(chunk_start))
                .filter(intervals::start_utc_ms.le(chunk_end))
                .filter(intervals::end_utc_ms.is_not_null())
                .filter(intervals::end_utc_ms.le(cutoff_utc_ms))
                .filter(intervals::title_id.is_not_null()),
        )
        .set((
            intervals::title_id.eq(None::<i64>),
            intervals::title_status.eq(TitleStatus::Purged),
        ))
        .execute(tx.connection)?
    };

    let web_spans = web_intervals::table
        .filter(web_intervals::start_utc_ms.ge(chunk_start))
        .filter(web_intervals::start_utc_ms.le(chunk_end))
        .filter(web_intervals::end_utc_ms.is_not_null())
        .filter(web_intervals::end_utc_ms.le(cutoff_utc_ms))
        .filter(web_intervals::tab_title_id.is_not_null())
        .select((web_intervals::start_utc_ms, web_intervals::end_utc_ms))
        .load::<(i64, Option<i64>)>(tx.connection)?;
    let web_intervals_purged = if web_spans.is_empty() {
        0
    } else {
        for (start, end) in web_spans {
            let end = end.ok_or(PurgeTitlesError::InvalidPurgeRow {
                table: "web_intervals",
            })?;
            if end < start {
                return Err(PurgeTitlesError::InvalidPurgeRow {
                    table: "web_intervals",
                });
            }
            record_affected(&mut affected, DataSource::Web, QueryRange { start, end });
        }
        diesel::update(
            web_intervals::table
                .filter(web_intervals::start_utc_ms.ge(chunk_start))
                .filter(web_intervals::start_utc_ms.le(chunk_end))
                .filter(web_intervals::end_utc_ms.is_not_null())
                .filter(web_intervals::end_utc_ms.le(cutoff_utc_ms))
                .filter(web_intervals::tab_title_id.is_not_null()),
        )
        .set((
            web_intervals::tab_title_id.eq(None::<i64>),
            web_intervals::tab_title_status.eq(TitleStatus::Purged),
        ))
        .execute(tx.connection)?
    };

    let titles_deleted = match position {
        PurgeChunkPosition::Intermediate => 0,
        PurgeChunkPosition::Final => {
            ConnectionSql::new(tx.connection).delete_unreferenced_titles()?
        }
    };

    for (source, days) in affected {
        for day in days {
            tx.effects.bump_day(tx.connection, source, day)?;
        }
    }

    Ok(PurgeTitlesSummary {
        intervals_purged,
        web_intervals_purged,
        titles_deleted,
    })
}

fn delete_range_in_tx(
    tx: &mut WriteTx<'_>,
    range: QueryRange,
    now: UtcInstant,
) -> Result<DeleteRangeSummary, DeleteRangeError> {
    let mut affected: BTreeMap<DataSource, BTreeSet<UtcDay>> = BTreeMap::new();
    // Clip web_intervals before intervals because web_intervals.interval_id references intervals(id).
    delete_web_intervals(tx, range, &mut affected)?;
    let deleted_ms = delete_intervals(tx, range, &mut affected)?;
    delete_availability_intervals(tx, range, &mut affected)?;
    delete_capture_gaps(tx, range, &mut affected)?;
    delete_annotations(tx, range, now, &mut affected)?;
    let removed_titles = ConnectionSql::new(tx.connection).delete_unreferenced_titles()?;
    let removed_domains = ConnectionSql::new(tx.connection).delete_unreferenced_domains()?;
    for (source, days) in affected {
        for day in days {
            tx.effects.bump_day(tx.connection, source, day)?;
        }
    }
    tracing::debug!(
        start_utc_ms = range.start,
        end_utc_ms = range.end,
        deleted_ms,
        removed_titles,
        removed_domains,
        "deleted date range"
    );
    Ok(DeleteRangeSummary { deleted_ms })
}

fn record_affected(
    affected: &mut BTreeMap<DataSource, BTreeSet<UtcDay>>,
    source: DataSource,
    row_range: QueryRange,
) {
    let ordered = QueryRange {
        start: row_range.start,
        end: row_range.end.max(row_range.start),
    };
    let days = UtcDays::covering(ordered);
    let entry = affected.entry(source).or_default();
    for day in days.start().0..=days.end().0 {
        entry.insert(UtcDay(day));
    }
}

/// Linearly interpolates a monotonic timestamp within `[start_mono, end_mono]` using `i128` math.
fn interpolate_mono(
    start_utc: i64,
    end_utc: i64,
    start_mono: i64,
    end_mono: i64,
    cut_utc: i64,
) -> i64 {
    if cut_utc <= start_utc || end_utc <= start_utc || end_mono <= start_mono {
        return start_mono;
    }
    if cut_utc >= end_utc {
        return end_mono;
    }
    let utc_span = i128::from(end_utc) - i128::from(start_utc);
    let mono_span = i128::from(end_mono) - i128::from(start_mono);
    let offset = i128::from(cut_utc) - i128::from(start_utc);
    let interpolated = i128::from(start_mono) + (mono_span * offset) / utc_span;
    i64::try_from(interpolated).expect("interpolated monotonic timestamp fits within i64 endpoints")
}

/// Splits a half-open UTC span along UTC-midnight boundaries (`k * UTC_DAY_MS`).
fn utc_day_chunks(span: QueryRange) -> Vec<QueryRange> {
    let mut chunks = Vec::new();
    let mut cursor = span.start;
    while cursor < span.end {
        let next_midnight = (cursor.div_euclid(UTC_DAY_MS) + 1)
            .checked_mul(UTC_DAY_MS)
            .unwrap_or(span.end);
        let chunk_end = next_midnight.min(span.end);
        chunks.push(QueryRange {
            start: cursor,
            end: chunk_end,
        });
        cursor = chunk_end;
    }
    chunks
}

fn delete_web_intervals(
    tx: &mut WriteTx<'_>,
    range: QueryRange,
    affected: &mut BTreeMap<DataSource, BTreeSet<UtcDay>>,
) -> Result<(), DeleteRangeError> {
    let closed_rows = web_intervals::table
        .filter(web_intervals::end_utc_ms.is_not_null())
        .filter(web_intervals::start_utc_ms.lt(range.end))
        .filter(
            web_intervals::end_utc_ms
                .gt(range.start)
                .or(web_intervals::end_utc_ms
                    .eq(web_intervals::start_utc_ms.nullable())
                    .and(web_intervals::start_utc_ms.ge(range.start))),
        )
        .order(web_intervals::id)
        .select(WebIntervalsRow::as_select())
        .load::<WebIntervalsRow>(tx.connection)?;
    for row in closed_rows {
        let row_id = row.id.ok_or(DeleteRangeError::InvalidStoredRow {
            table: "web_intervals",
        })?;
        let old_end = row.end_utc_ms.ok_or(DeleteRangeError::InvalidStoredRow {
            table: "web_intervals",
        })?;
        if old_end < row.start_utc_ms {
            return Err(DeleteRangeError::InvalidStoredRow {
                table: "web_intervals",
            });
        }
        let old_range = QueryRange {
            start: row.start_utc_ms,
            end: old_end,
        };
        record_affected(affected, DataSource::Web, old_range);
        let remainders = Remainders::subtract(old_range, range);
        match (remainders.left, remainders.right) {
            (None, None) => {
                diesel::delete(web_intervals::table.filter(web_intervals::id.eq(row_id)))
                    .execute(tx.connection)?;
            }
            (Some(left), None) => {
                diesel::update(web_intervals::table.filter(web_intervals::id.eq(row_id)))
                    .set(web_intervals::end_utc_ms.eq(Some(left.end)))
                    .execute(tx.connection)?;
            }
            (None, Some(right)) => {
                diesel::update(web_intervals::table.filter(web_intervals::id.eq(row_id)))
                    .set(web_intervals::start_utc_ms.eq(right.start))
                    .execute(tx.connection)?;
            }
            (Some(left), Some(right)) => {
                diesel::update(web_intervals::table.filter(web_intervals::id.eq(row_id)))
                    .set(web_intervals::end_utc_ms.eq(Some(left.end)))
                    .execute(tx.connection)?;
                let mut right_row = row;
                right_row.id = None;
                right_row.start_utc_ms = right.start;
                right_row.end_utc_ms = Some(right.end);
                right_row.confirmed_utc_ms = None;
                diesel::insert_into(web_intervals::table)
                    .values(&right_row)
                    .execute(tx.connection)?;
            }
        }
    }

    let open_rows = web_intervals::table
        .filter(web_intervals::end_utc_ms.is_null())
        .filter(web_intervals::start_utc_ms.lt(range.end))
        .filter(
            web_intervals::confirmed_utc_ms
                .gt(range.start)
                .or(web_intervals::confirmed_utc_ms
                    .eq(web_intervals::start_utc_ms.nullable())
                    .and(web_intervals::start_utc_ms.ge(range.start))),
        )
        .order(web_intervals::id)
        .select(WebIntervalsRow::as_select())
        .load::<WebIntervalsRow>(tx.connection)?;
    for row in open_rows {
        let row_id = row.id.ok_or(DeleteRangeError::InvalidStoredRow {
            table: "web_intervals",
        })?;
        let old_conf = row
            .confirmed_utc_ms
            .ok_or(DeleteRangeError::InvalidStoredRow {
                table: "web_intervals",
            })?;
        record_affected(
            affected,
            DataSource::Web,
            QueryRange {
                start: row.start_utc_ms,
                end: old_conf.max(range.end),
            },
        );
        if row.start_utc_ms < range.start {
            let mut left_row = row.clone();
            left_row.id = None;
            left_row.start_utc_ms = row.start_utc_ms;
            left_row.end_utc_ms = Some(range.start);
            left_row.confirmed_utc_ms = None;
            left_row.end_reason = Some(WEB_END_REASON_POLICY_CHANGED);
            diesel::insert_into(web_intervals::table)
                .values(&left_row)
                .execute(tx.connection)?;
        }
        diesel::update(web_intervals::table.filter(web_intervals::id.eq(row_id)))
            .set((
                web_intervals::start_utc_ms.eq(range.end),
                web_intervals::confirmed_utc_ms.eq(Some(old_conf.max(range.end))),
            ))
            .execute(tx.connection)?;
    }
    Ok(())
}

fn removed_foreground_ms(
    row: &IntervalsRow,
    old_end_utc: i64,
    old_end_mono: i64,
    range: QueryRange,
) -> i64 {
    if row.kind != IntervalKind::Window || row.capture_level == CaptureLevel::Excluded {
        return 0;
    }
    let cut_start_utc = row.start_utc_ms.max(range.start);
    let cut_end_utc = old_end_utc.min(range.end);
    if cut_end_utc <= cut_start_utc {
        return 0;
    }
    let cut_start_mono = if cut_start_utc == row.start_utc_ms {
        row.start_mono_ms
    } else {
        interpolate_mono(
            row.start_utc_ms,
            old_end_utc,
            row.start_mono_ms,
            old_end_mono,
            cut_start_utc,
        )
    };
    let cut_end_mono = if cut_end_utc == old_end_utc {
        old_end_mono
    } else {
        interpolate_mono(
            row.start_utc_ms,
            old_end_utc,
            row.start_mono_ms,
            old_end_mono,
            cut_end_utc,
        )
    };
    cut_end_mono.saturating_sub(cut_start_mono).max(0)
}

fn delete_intervals(
    tx: &mut WriteTx<'_>,
    range: QueryRange,
    affected: &mut BTreeMap<DataSource, BTreeSet<UtcDay>>,
) -> Result<i64, DeleteRangeError> {
    let mut deleted_ms: i64 = 0;

    let closed_rows = intervals::table
        .filter(intervals::end_utc_ms.is_not_null())
        .filter(intervals::start_utc_ms.lt(range.end))
        .filter(
            intervals::end_utc_ms
                .gt(range.start)
                .or(intervals::end_utc_ms
                    .eq(intervals::start_utc_ms.nullable())
                    .and(intervals::start_utc_ms.ge(range.start))),
        )
        .order(intervals::id)
        .select(IntervalsRow::as_select())
        .load::<IntervalsRow>(tx.connection)?;
    for row in closed_rows {
        let row_id = row
            .id
            .ok_or(DeleteRangeError::InvalidStoredRow { table: "intervals" })?;
        let old_end_utc = row
            .end_utc_ms
            .ok_or(DeleteRangeError::InvalidStoredRow { table: "intervals" })?;
        let old_end_mono = row
            .end_mono_ms
            .ok_or(DeleteRangeError::InvalidStoredRow { table: "intervals" })?;
        if old_end_utc < row.start_utc_ms || old_end_mono < row.start_mono_ms {
            return Err(DeleteRangeError::InvalidStoredRow { table: "intervals" });
        }
        let old_range = QueryRange {
            start: row.start_utc_ms,
            end: old_end_utc,
        };
        record_affected(affected, DataSource::Intervals, old_range);
        deleted_ms = deleted_ms.saturating_add(removed_foreground_ms(
            &row,
            old_end_utc,
            old_end_mono,
            range,
        ));
        let remainders = Remainders::subtract(old_range, range);
        match (remainders.left, remainders.right) {
            (None, None) => {
                // Defensively remove any residual web_intervals rows still referencing this interval.
                let residual_web = web_intervals::table
                    .filter(web_intervals::interval_id.eq(row_id))
                    .select(WebIntervalsRow::as_select())
                    .load::<WebIntervalsRow>(tx.connection)?;
                for web in residual_web {
                    let web_end = web
                        .end_utc_ms
                        .or(web.confirmed_utc_ms)
                        .unwrap_or(web.start_utc_ms);
                    record_affected(
                        affected,
                        DataSource::Web,
                        QueryRange {
                            start: web.start_utc_ms,
                            end: web_end,
                        },
                    );
                }
                diesel::delete(web_intervals::table.filter(web_intervals::interval_id.eq(row_id)))
                    .execute(tx.connection)?;
                diesel::delete(intervals::table.filter(intervals::id.eq(row_id)))
                    .execute(tx.connection)?;
            }
            (Some(left), None) => {
                let left_end_mono = interpolate_mono(
                    row.start_utc_ms,
                    old_end_utc,
                    row.start_mono_ms,
                    old_end_mono,
                    left.end,
                );
                let duration = left_end_mono - row.start_mono_ms;
                diesel::update(intervals::table.filter(intervals::id.eq(row_id)))
                    .set((
                        intervals::end_utc_ms.eq(Some(left.end)),
                        intervals::end_mono_ms.eq(Some(left_end_mono)),
                        intervals::duration_ms.eq(Some(duration)),
                    ))
                    .execute(tx.connection)?;
            }
            (None, Some(right)) => {
                let right_start_mono = interpolate_mono(
                    row.start_utc_ms,
                    old_end_utc,
                    row.start_mono_ms,
                    old_end_mono,
                    right.start,
                );
                let duration = old_end_mono - right_start_mono;
                diesel::update(intervals::table.filter(intervals::id.eq(row_id)))
                    .set((
                        intervals::start_utc_ms.eq(right.start),
                        intervals::start_mono_ms.eq(right_start_mono),
                        intervals::duration_ms.eq(Some(duration)),
                    ))
                    .execute(tx.connection)?;
            }
            (Some(left), Some(right)) => {
                let left_end_mono = interpolate_mono(
                    row.start_utc_ms,
                    old_end_utc,
                    row.start_mono_ms,
                    old_end_mono,
                    left.end,
                );
                let right_start_mono = interpolate_mono(
                    row.start_utc_ms,
                    old_end_utc,
                    row.start_mono_ms,
                    old_end_mono,
                    right.start,
                );
                diesel::update(intervals::table.filter(intervals::id.eq(row_id)))
                    .set((
                        intervals::end_utc_ms.eq(Some(left.end)),
                        intervals::end_mono_ms.eq(Some(left_end_mono)),
                        intervals::duration_ms.eq(Some(left_end_mono - row.start_mono_ms)),
                    ))
                    .execute(tx.connection)?;
                let mut right_row = row;
                right_row.id = None;
                right_row.start_utc_ms = right.start;
                right_row.start_mono_ms = right_start_mono;
                right_row.end_utc_ms = Some(right.end);
                right_row.end_mono_ms = Some(old_end_mono);
                right_row.confirmed_utc_ms = None;
                right_row.confirmed_mono_ms = None;
                right_row.duration_ms = Some(old_end_mono - right_start_mono);
                let right_id = diesel::insert_into(intervals::table)
                    .values(&right_row)
                    .returning(intervals::id.assume_not_null())
                    .get_result::<i64>(tx.connection)?;
                diesel::update(
                    web_intervals::table
                        .filter(web_intervals::interval_id.eq(row_id))
                        .filter(web_intervals::start_utc_ms.ge(right.start)),
                )
                .set(web_intervals::interval_id.eq(right_id))
                .execute(tx.connection)?;
            }
        }
    }

    let open_rows = intervals::table
        .filter(intervals::end_utc_ms.is_null())
        .filter(intervals::start_utc_ms.lt(range.end))
        .filter(
            intervals::confirmed_utc_ms
                .gt(range.start)
                .or(intervals::confirmed_utc_ms
                    .eq(intervals::start_utc_ms.nullable())
                    .and(intervals::start_utc_ms.ge(range.start))),
        )
        .order(intervals::id)
        .select(IntervalsRow::as_select())
        .load::<IntervalsRow>(tx.connection)?;
    for row in open_rows {
        let row_id = row
            .id
            .ok_or(DeleteRangeError::InvalidStoredRow { table: "intervals" })?;
        let old_conf_utc = row
            .confirmed_utc_ms
            .ok_or(DeleteRangeError::InvalidStoredRow { table: "intervals" })?;
        let old_conf_mono = row
            .confirmed_mono_ms
            .ok_or(DeleteRangeError::InvalidStoredRow { table: "intervals" })?;
        if old_conf_utc < row.start_utc_ms || old_conf_mono < row.start_mono_ms {
            return Err(DeleteRangeError::InvalidStoredRow { table: "intervals" });
        }
        record_affected(
            affected,
            DataSource::Intervals,
            QueryRange {
                start: row.start_utc_ms,
                end: old_conf_utc.max(range.end),
            },
        );
        deleted_ms = deleted_ms.saturating_add(removed_foreground_ms(
            &row,
            old_conf_utc,
            old_conf_mono,
            range,
        ));
        if row.start_utc_ms < range.start {
            let chunks = utc_day_chunks(QueryRange {
                start: row.start_utc_ms,
                end: range.start,
            });
            let last_index = chunks.len().saturating_sub(1);
            for (index, chunk) in chunks.into_iter().enumerate() {
                let chunk_start_mono = if chunk.start == row.start_utc_ms {
                    row.start_mono_ms
                } else {
                    interpolate_mono(
                        row.start_utc_ms,
                        old_conf_utc,
                        row.start_mono_ms,
                        old_conf_mono,
                        chunk.start,
                    )
                };
                let chunk_end_mono = interpolate_mono(
                    row.start_utc_ms,
                    old_conf_utc,
                    row.start_mono_ms,
                    old_conf_mono,
                    chunk.end,
                );
                let end_reason = if index == last_index {
                    IntervalEndReason::PolicyChanged
                } else {
                    IntervalEndReason::MidnightSplit
                };
                let mut left_row = row.clone();
                left_row.id = None;
                left_row.start_utc_ms = chunk.start;
                left_row.start_mono_ms = chunk_start_mono;
                left_row.end_utc_ms = Some(chunk.end);
                left_row.end_mono_ms = Some(chunk_end_mono);
                left_row.confirmed_utc_ms = None;
                left_row.confirmed_mono_ms = None;
                left_row.duration_ms = Some(chunk_end_mono - chunk_start_mono);
                left_row.end_reason = Some(end_reason);
                let left_id = diesel::insert_into(intervals::table)
                    .values(&left_row)
                    .returning(intervals::id.assume_not_null())
                    .get_result::<i64>(tx.connection)?;
                if index == 0 {
                    diesel::update(
                        web_intervals::table
                            .filter(web_intervals::interval_id.eq(row_id))
                            .filter(web_intervals::start_utc_ms.lt(chunk.end)),
                    )
                    .set(web_intervals::interval_id.eq(left_id))
                    .execute(tx.connection)?;
                } else {
                    diesel::update(
                        web_intervals::table
                            .filter(web_intervals::interval_id.eq(row_id))
                            .filter(web_intervals::start_utc_ms.ge(chunk.start))
                            .filter(web_intervals::start_utc_ms.lt(chunk.end)),
                    )
                    .set(web_intervals::interval_id.eq(left_id))
                    .execute(tx.connection)?;
                }
            }
        }
        // Keep the open row's ID and open status so active CaptureKeys stay valid.
        let (new_start_mono, new_conf_utc, new_conf_mono) = if old_conf_utc > range.end {
            let start_mono = interpolate_mono(
                row.start_utc_ms,
                old_conf_utc,
                row.start_mono_ms,
                old_conf_mono,
                range.end,
            );
            (start_mono, old_conf_utc, old_conf_mono)
        } else {
            (old_conf_mono, range.end, old_conf_mono)
        };
        diesel::update(intervals::table.filter(intervals::id.eq(row_id)))
            .set((
                intervals::start_utc_ms.eq(range.end),
                intervals::start_mono_ms.eq(new_start_mono),
                intervals::confirmed_utc_ms.eq(Some(new_conf_utc)),
                intervals::confirmed_mono_ms.eq(Some(new_conf_mono)),
            ))
            .execute(tx.connection)?;
    }

    Ok(deleted_ms)
}

fn delete_availability_intervals(
    tx: &mut WriteTx<'_>,
    range: QueryRange,
    affected: &mut BTreeMap<DataSource, BTreeSet<UtcDay>>,
) -> Result<(), DeleteRangeError> {
    let closed_rows = availability_intervals::table
        .filter(availability_intervals::end_utc_ms.is_not_null())
        .filter(availability_intervals::start_utc_ms.lt(range.end))
        .filter(
            availability_intervals::end_utc_ms.gt(range.start).or(
                availability_intervals::end_utc_ms
                    .eq(availability_intervals::start_utc_ms.nullable())
                    .and(availability_intervals::start_utc_ms.ge(range.start)),
            ),
        )
        .order(availability_intervals::id)
        .select(AvailabilityIntervalsRow::as_select())
        .load::<AvailabilityIntervalsRow>(tx.connection)?;
    for row in closed_rows {
        let row_id = row.id.ok_or(DeleteRangeError::InvalidStoredRow {
            table: "availability_intervals",
        })?;
        let old_end_utc = row.end_utc_ms.ok_or(DeleteRangeError::InvalidStoredRow {
            table: "availability_intervals",
        })?;
        let old_end_mono = row.end_mono_ms.ok_or(DeleteRangeError::InvalidStoredRow {
            table: "availability_intervals",
        })?;
        if old_end_utc < row.start_utc_ms || old_end_mono < row.start_mono_ms {
            return Err(DeleteRangeError::InvalidStoredRow {
                table: "availability_intervals",
            });
        }
        let old_range = QueryRange {
            start: row.start_utc_ms,
            end: old_end_utc,
        };
        record_affected(affected, DataSource::Availability, old_range);
        let remainders = Remainders::subtract(old_range, range);
        match (remainders.left, remainders.right) {
            (None, None) => {
                diesel::delete(
                    availability_intervals::table.filter(availability_intervals::id.eq(row_id)),
                )
                .execute(tx.connection)?;
            }
            (Some(left), None) => {
                let left_end_mono = interpolate_mono(
                    row.start_utc_ms,
                    old_end_utc,
                    row.start_mono_ms,
                    old_end_mono,
                    left.end,
                );
                diesel::update(
                    availability_intervals::table.filter(availability_intervals::id.eq(row_id)),
                )
                .set((
                    availability_intervals::end_utc_ms.eq(Some(left.end)),
                    availability_intervals::end_mono_ms.eq(Some(left_end_mono)),
                ))
                .execute(tx.connection)?;
            }
            (None, Some(right)) => {
                let right_start_mono = interpolate_mono(
                    row.start_utc_ms,
                    old_end_utc,
                    row.start_mono_ms,
                    old_end_mono,
                    right.start,
                );
                diesel::update(
                    availability_intervals::table.filter(availability_intervals::id.eq(row_id)),
                )
                .set((
                    availability_intervals::start_utc_ms.eq(right.start),
                    availability_intervals::start_mono_ms.eq(right_start_mono),
                ))
                .execute(tx.connection)?;
            }
            (Some(left), Some(right)) => {
                let left_end_mono = interpolate_mono(
                    row.start_utc_ms,
                    old_end_utc,
                    row.start_mono_ms,
                    old_end_mono,
                    left.end,
                );
                let right_start_mono = interpolate_mono(
                    row.start_utc_ms,
                    old_end_utc,
                    row.start_mono_ms,
                    old_end_mono,
                    right.start,
                );
                diesel::update(
                    availability_intervals::table.filter(availability_intervals::id.eq(row_id)),
                )
                .set((
                    availability_intervals::end_utc_ms.eq(Some(left.end)),
                    availability_intervals::end_mono_ms.eq(Some(left_end_mono)),
                ))
                .execute(tx.connection)?;
                let mut right_row = row;
                right_row.id = None;
                right_row.start_utc_ms = right.start;
                right_row.start_mono_ms = right_start_mono;
                right_row.end_utc_ms = Some(right.end);
                right_row.end_mono_ms = Some(old_end_mono);
                right_row.confirmed_utc_ms = None;
                right_row.confirmed_mono_ms = None;
                diesel::insert_into(availability_intervals::table)
                    .values(&right_row)
                    .execute(tx.connection)?;
            }
        }
    }

    let open_rows = availability_intervals::table
        .filter(availability_intervals::end_utc_ms.is_null())
        .filter(availability_intervals::start_utc_ms.lt(range.end))
        .filter(
            availability_intervals::confirmed_utc_ms.gt(range.start).or(
                availability_intervals::confirmed_utc_ms
                    .eq(availability_intervals::start_utc_ms.nullable())
                    .and(availability_intervals::start_utc_ms.ge(range.start)),
            ),
        )
        .order(availability_intervals::id)
        .select(AvailabilityIntervalsRow::as_select())
        .load::<AvailabilityIntervalsRow>(tx.connection)?;
    for row in open_rows {
        let row_id = row.id.ok_or(DeleteRangeError::InvalidStoredRow {
            table: "availability_intervals",
        })?;
        let old_conf_utc = row
            .confirmed_utc_ms
            .ok_or(DeleteRangeError::InvalidStoredRow {
                table: "availability_intervals",
            })?;
        let old_conf_mono = row
            .confirmed_mono_ms
            .ok_or(DeleteRangeError::InvalidStoredRow {
                table: "availability_intervals",
            })?;
        if old_conf_utc < row.start_utc_ms || old_conf_mono < row.start_mono_ms {
            return Err(DeleteRangeError::InvalidStoredRow {
                table: "availability_intervals",
            });
        }
        record_affected(
            affected,
            DataSource::Availability,
            QueryRange {
                start: row.start_utc_ms,
                end: old_conf_utc.max(range.end),
            },
        );
        if row.start_utc_ms < range.start {
            let chunks = utc_day_chunks(QueryRange {
                start: row.start_utc_ms,
                end: range.start,
            });
            let last_index = chunks.len().saturating_sub(1);
            for (index, chunk) in chunks.into_iter().enumerate() {
                let chunk_start_mono = if chunk.start == row.start_utc_ms {
                    row.start_mono_ms
                } else {
                    interpolate_mono(
                        row.start_utc_ms,
                        old_conf_utc,
                        row.start_mono_ms,
                        old_conf_mono,
                        chunk.start,
                    )
                };
                let chunk_end_mono = interpolate_mono(
                    row.start_utc_ms,
                    old_conf_utc,
                    row.start_mono_ms,
                    old_conf_mono,
                    chunk.end,
                );
                let end_reason = if index == last_index {
                    AvailabilityEndReason::PolicyChanged
                } else {
                    AvailabilityEndReason::MidnightSplit
                };
                let mut left_row = row.clone();
                left_row.id = None;
                left_row.start_utc_ms = chunk.start;
                left_row.start_mono_ms = chunk_start_mono;
                left_row.end_utc_ms = Some(chunk.end);
                left_row.end_mono_ms = Some(chunk_end_mono);
                left_row.confirmed_utc_ms = None;
                left_row.confirmed_mono_ms = None;
                left_row.end_reason = Some(end_reason);
                left_row.exception = None;
                diesel::insert_into(availability_intervals::table)
                    .values(&left_row)
                    .execute(tx.connection)?;
            }
        }
        let (new_start_mono, new_conf_utc, new_conf_mono) = if old_conf_utc > range.end {
            let start_mono = interpolate_mono(
                row.start_utc_ms,
                old_conf_utc,
                row.start_mono_ms,
                old_conf_mono,
                range.end,
            );
            (start_mono, old_conf_utc, old_conf_mono)
        } else {
            (old_conf_mono, range.end, old_conf_mono)
        };
        if row.reason == AvailabilityReason::Idle {
            // Preserve crash-recovery invariants on open Idle rows:
            // last_input <= start (with start - last_input <= start_mono) and detected >= start.
            let new_detected = row.detected_utc_ms.map(|detected| detected.max(range.end));
            let new_last_input = row.last_input_utc_ms.map(|last_input| {
                range
                    .end
                    .min(last_input.max(range.end.saturating_sub(new_start_mono)))
            });
            diesel::update(
                availability_intervals::table.filter(availability_intervals::id.eq(row_id)),
            )
            .set((
                availability_intervals::start_utc_ms.eq(range.end),
                availability_intervals::start_mono_ms.eq(new_start_mono),
                availability_intervals::confirmed_utc_ms.eq(Some(new_conf_utc)),
                availability_intervals::confirmed_mono_ms.eq(Some(new_conf_mono)),
                availability_intervals::last_input_utc_ms.eq(new_last_input),
                availability_intervals::detected_utc_ms.eq(new_detected),
            ))
            .execute(tx.connection)?;
        } else {
            diesel::update(
                availability_intervals::table.filter(availability_intervals::id.eq(row_id)),
            )
            .set((
                availability_intervals::start_utc_ms.eq(range.end),
                availability_intervals::start_mono_ms.eq(new_start_mono),
                availability_intervals::confirmed_utc_ms.eq(Some(new_conf_utc)),
                availability_intervals::confirmed_mono_ms.eq(Some(new_conf_mono)),
            ))
            .execute(tx.connection)?;
        }
    }
    Ok(())
}

fn delete_capture_gaps(
    tx: &mut WriteTx<'_>,
    range: QueryRange,
    affected: &mut BTreeMap<DataSource, BTreeSet<UtcDay>>,
) -> Result<(), DeleteRangeError> {
    let rows = capture_gaps::table
        .filter(capture_gaps::start_utc_ms.lt(range.end))
        .filter(
            capture_gaps::end_utc_ms
                .gt(range.start)
                .or(capture_gaps::end_utc_ms
                    .eq(capture_gaps::start_utc_ms)
                    .and(capture_gaps::start_utc_ms.ge(range.start))),
        )
        .order(capture_gaps::id)
        .select(CaptureGapsRow::as_select())
        .load::<CaptureGapsRow>(tx.connection)?;
    for row in rows {
        let row_id = row.id.ok_or(DeleteRangeError::InvalidStoredRow {
            table: "capture_gaps",
        })?;
        if row.end_utc_ms < row.start_utc_ms || row.end_mono_ms < row.start_mono_ms {
            return Err(DeleteRangeError::InvalidStoredRow {
                table: "capture_gaps",
            });
        }
        let old_range = QueryRange {
            start: row.start_utc_ms,
            end: row.end_utc_ms,
        };
        record_affected(affected, DataSource::Gaps, old_range);
        let remainders = Remainders::subtract(old_range, range);
        match (remainders.left, remainders.right) {
            (None, None) => {
                diesel::delete(capture_gaps::table.filter(capture_gaps::id.eq(row_id)))
                    .execute(tx.connection)?;
            }
            (Some(left), None) => {
                let left_end_mono = interpolate_mono(
                    row.start_utc_ms,
                    row.end_utc_ms,
                    row.start_mono_ms,
                    row.end_mono_ms,
                    left.end,
                );
                diesel::update(capture_gaps::table.filter(capture_gaps::id.eq(row_id)))
                    .set((
                        capture_gaps::end_utc_ms.eq(left.end),
                        capture_gaps::end_mono_ms.eq(left_end_mono),
                    ))
                    .execute(tx.connection)?;
            }
            (None, Some(right)) => {
                let right_start_mono = interpolate_mono(
                    row.start_utc_ms,
                    row.end_utc_ms,
                    row.start_mono_ms,
                    row.end_mono_ms,
                    right.start,
                );
                diesel::update(capture_gaps::table.filter(capture_gaps::id.eq(row_id)))
                    .set((
                        capture_gaps::start_utc_ms.eq(right.start),
                        capture_gaps::start_mono_ms.eq(right_start_mono),
                    ))
                    .execute(tx.connection)?;
            }
            (Some(left), Some(right)) => {
                let left_end_mono = interpolate_mono(
                    row.start_utc_ms,
                    row.end_utc_ms,
                    row.start_mono_ms,
                    row.end_mono_ms,
                    left.end,
                );
                let right_start_mono = interpolate_mono(
                    row.start_utc_ms,
                    row.end_utc_ms,
                    row.start_mono_ms,
                    row.end_mono_ms,
                    right.start,
                );
                diesel::update(capture_gaps::table.filter(capture_gaps::id.eq(row_id)))
                    .set((
                        capture_gaps::end_utc_ms.eq(left.end),
                        capture_gaps::end_mono_ms.eq(left_end_mono),
                    ))
                    .execute(tx.connection)?;
                let mut right_row = row;
                right_row.id = None;
                right_row.start_utc_ms = right.start;
                right_row.start_mono_ms = right_start_mono;
                right_row.end_utc_ms = right.end;
                diesel::insert_into(capture_gaps::table)
                    .values(&right_row)
                    .execute(tx.connection)?;
            }
        }
    }
    Ok(())
}

fn delete_annotations(
    tx: &mut WriteTx<'_>,
    range: QueryRange,
    now: UtcInstant,
    affected: &mut BTreeMap<DataSource, BTreeSet<UtcDay>>,
) -> Result<(), DeleteRangeError> {
    let now_ms = now.get().as_millisecond();
    let rows = annotations::table
        .filter(annotations::start_utc_ms.lt(range.end))
        .filter(annotations::end_utc_ms.gt(range.start))
        .order(annotations::id)
        .select(AnnotationsRow::as_select())
        .load::<AnnotationsRow>(tx.connection)?;
    for row in rows {
        let row_id = row.id.ok_or(DeleteRangeError::InvalidStoredRow {
            table: "annotations",
        })?;
        if row.end_utc_ms <= row.start_utc_ms {
            return Err(DeleteRangeError::InvalidStoredRow {
                table: "annotations",
            });
        }
        let old_range = QueryRange {
            start: row.start_utc_ms,
            end: row.end_utc_ms,
        };
        record_affected(affected, DataSource::Annotations, old_range);
        let remainders = Remainders::subtract(old_range, range);
        match (remainders.left, remainders.right) {
            (None, None) => {
                diesel::delete(annotations::table.filter(annotations::id.eq(row_id)))
                    .execute(tx.connection)?;
            }
            (Some(left), None) => {
                diesel::update(annotations::table.filter(annotations::id.eq(row_id)))
                    .set((
                        annotations::end_utc_ms.eq(left.end),
                        annotations::updated_utc_ms.eq(now_ms),
                    ))
                    .execute(tx.connection)?;
            }
            (None, Some(right)) => {
                diesel::update(annotations::table.filter(annotations::id.eq(row_id)))
                    .set((
                        annotations::start_utc_ms.eq(right.start),
                        annotations::updated_utc_ms.eq(now_ms),
                    ))
                    .execute(tx.connection)?;
            }
            (Some(left), Some(right)) => {
                let old_uuid = AnnotationId(
                    row.uuid
                        .parse()
                        .context(delete_range_error::InvalidAnnotationUuidSnafu)?,
                );
                let right_uuid = DeleteSplitIdentity {
                    old: &old_uuid,
                    old_range,
                    deleted_range: range,
                }
                .derive();
                diesel::update(annotations::table.filter(annotations::id.eq(row_id)))
                    .set((
                        annotations::end_utc_ms.eq(left.end),
                        annotations::updated_utc_ms.eq(now_ms),
                    ))
                    .execute(tx.connection)?;
                let mut right_row = row;
                right_row.id = None;
                right_row.uuid = right_uuid.0.to_string();
                right_row.start_utc_ms = right.start;
                right_row.end_utc_ms = right.end;
                right_row.created_utc_ms = now_ms;
                right_row.updated_utc_ms = now_ms;
                diesel::insert_into(annotations::table)
                    .values(&right_row)
                    .execute(tx.connection)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
