//! 核心、界面与浏览器 host 共用的消息类型、分帧编解码、协议版本与结构化错误码；保持跨平台，不依赖 Windows API、SQLite 或 GPUI，消息只承载数据。

use serde::{Deserialize, Serialize};
use snafu::Snafu;
use std::{fmt, path::PathBuf, str::FromStr};

/// Stable startup failure codes shared by argv and the future pipe protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CoreErrorCode {
    /// The database contains migrations unknown to this program.
    NewerDatabase,
    /// A migration failed after a consistent backup was created.
    MigrationFailed,
    /// Another Windows session holds the data-directory lock.
    OtherSession,
    /// Core startup failed for another reason.
    CoreStartFailed,
}

impl CoreErrorCode {
    /// Returns the stable string used in command arguments and JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NewerDatabase => "newer_database",
            Self::MigrationFailed => "migration_failed",
            Self::OtherSession => "other_session",
            Self::CoreStartFailed => "core_start_failed",
        }
    }
}
impl fmt::Display for CoreErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An argv error code is not recognized by this program.
#[derive(Debug, Snafu)]
#[snafu(display("unknown core error code"))]
pub struct ParseCoreErrorCodeError;

impl FromStr for CoreErrorCode {
    type Err = ParseCoreErrorCodeError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "newer_database" => Ok(Self::NewerDatabase),
            "migration_failed" => Ok(Self::MigrationFailed),
            "other_session" => Ok(Self::OtherSession),
            "core_start_failed" => Ok(Self::CoreStartFailed),
            _ => Err(ParseCoreErrorCodeError),
        }
    }
}

/// A core failure and structured recovery paths, without presentation text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CoreError {
    /// Determines the UI's localized presentation.
    pub code: CoreErrorCode,
    /// Available migration backups; migration failure carries its exact backup here.
    #[serde(default)]
    pub backup_paths: Vec<PathBuf>,
}
impl CoreError {
    /// Creates a failure payload without translating or inspecting its paths.
    #[must_use]
    pub fn new(code: CoreErrorCode, backup_paths: Vec<PathBuf>) -> Self {
        Self { code, backup_paths }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_codes_round_trip_through_argv_and_json() {
        for (code, text) in [
            (CoreErrorCode::NewerDatabase, "newer_database"),
            (CoreErrorCode::MigrationFailed, "migration_failed"),
            (CoreErrorCode::OtherSession, "other_session"),
            (CoreErrorCode::CoreStartFailed, "core_start_failed"),
        ] {
            assert_eq!(code.to_string(), text);
            assert_eq!(
                text.parse::<CoreErrorCode>().expect("known argv code"),
                code
            );
            assert_eq!(
                serde_json::to_string(&code).expect("serialize code"),
                format!("\"{text}\"")
            );
            assert_eq!(
                serde_json::from_str::<CoreErrorCode>(&format!("\"{text}\""))
                    .expect("deserialize code"),
                code
            );
        }
        for unknown in ["", "newer-database", "unknown", "1"] {
            assert!(unknown.parse::<CoreErrorCode>().is_err());
        }
    }
    #[test]
    fn recovery_paths_are_separate_structured_fields() {
        let error = CoreError::new(
            CoreErrorCode::MigrationFailed,
            vec![PathBuf::from("C:/数据目录/backup with spaces.db")],
        );
        let json = serde_json::to_string(&error).expect("serialize failure");
        assert_eq!(
            serde_json::from_str::<CoreError>(&json).expect("deserialize failure"),
            error
        );
        assert!(
            serde_json::from_str::<CoreError>(r#"{"code":"other_session","future":true}"#)
                .expect("ignore future field")
                .backup_paths
                .is_empty()
        );
    }
}
