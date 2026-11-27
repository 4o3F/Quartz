//! Local data storage inspection, backup, compaction, and date-range deletion (04 §6.3).

use crate::{LocalDate, QueryVersions};
use serde::{Deserialize, Serialize};

/// Origin of a recognized database backup file under the backups directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupKind {
    /// Automatic backup written before a schema migration (`pre-migrate-…`).
    Migration,
    /// User-triggered backup written by `data.backup` (`manual-…`).
    Manual,
}

/// Metadata for a single backup database file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupEntry {
    /// File name within the backups directory.
    pub file_name: String,
    /// File size in bytes.
    pub bytes: u64,
    /// Backup creation timestamp in UTC milliseconds.
    pub created_utc_ms: i64,
    /// Whether this backup was created automatically before migration or manually.
    pub kind: BackupKind,
}

/// Empty `data.info` request body.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataInfoRequest {}

/// Storage locations, database size, and known backups sorted newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataInfoResponse {
    /// Absolute path to the main SQLite database file.
    pub database_path: String,
    /// Combined size in bytes of the main database file and its `-wal` file.
    pub database_bytes: u64,
    /// Absolute path to the logs directory.
    pub logs_path: String,
    /// Absolute path to the backups directory.
    pub backups_path: String,
    /// Known backup files sorted newest first.
    pub backups: Vec<BackupEntry>,
}

/// Empty `data.backup` request body.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataBackupRequest {}

/// Empty `data.compact` request body.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataCompactRequest {}

/// Database size before and after compaction (`VACUUM` + WAL truncation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataCompactResponse {
    /// Combined main + `-wal` size in bytes before compaction.
    pub before_bytes: u64,
    /// Combined main + `-wal` size in bytes after compaction.
    pub after_bytes: u64,
}

/// Half-open logical date range `[start_date, end_date)` of finished days to delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataDeleteRangeRequest {
    /// First logical day to delete (inclusive).
    pub start_date: LocalDate,
    /// End boundary logical day (exclusive; must be at most the current logical day).
    pub end_date: LocalDate,
}

/// Summary returned after committing a date-range deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataDeleteRangeResponse {
    /// Active foreground window milliseconds removed by the deletion.
    pub deleted_ms: i64,
    /// Updated version digest for the deleted UTC range.
    pub versions: QueryVersions,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoreRequest, Op};

    fn snapshot<T: serde::de::DeserializeOwned + Serialize>(value: &serde_json::Value) {
        let decoded: T = serde_json::from_value(value.clone()).expect("typed data wire value");
        assert_eq!(serde_json::to_value(decoded).expect("data JSON"), *value);
    }

    #[test]
    fn data_wire_snapshots_cover_every_request_and_response() {
        let wire: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/data.json"))
                .expect("data wire snapshots");
        snapshot::<DataInfoRequest>(&wire["info_request"]);
        snapshot::<DataInfoResponse>(&wire["info_response"]);
        snapshot::<DataBackupRequest>(&wire["backup_request"]);
        snapshot::<BackupEntry>(&wire["backup_response"]);
        snapshot::<DataCompactRequest>(&wire["compact_request"]);
        snapshot::<DataCompactResponse>(&wire["compact_response"]);
        snapshot::<DataDeleteRangeRequest>(&wire["delete_range_request"]);
        snapshot::<DataDeleteRangeResponse>(&wire["delete_range_response"]);

        assert_eq!(DataInfoRequest::OP, Op::DataInfo);
        assert_eq!(DataBackupRequest::OP, Op::DataBackup);
        assert_eq!(DataCompactRequest::OP, Op::DataCompact);
        assert_eq!(DataDeleteRangeRequest::OP, Op::DataDeleteRange);

        assert!(
            serde_json::from_value::<DataInfoRequest>(serde_json::json!({"extra": true})).is_err()
        );
        assert!(
            serde_json::from_value::<DataBackupRequest>(serde_json::json!({"extra": true}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<DataCompactRequest>(serde_json::json!({"extra": true}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<DataDeleteRangeRequest>(serde_json::json!({
                "start_date": "2026-11-20",
                "end_date": "2026-11-22",
                "extra": true
            }))
            .is_err()
        );
    }
}
