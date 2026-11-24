//! Write diagnostics contain typed failure classes and no stored values or database messages.

use crate::store::{settings::AccessSettingError, versions::BumpVersionError};
use quartz_protocol::{ErrorKind, ProtocolError};

#[derive(Debug)]
pub(super) enum WriteFailure {
    Database(DatabaseFailure),
    Version(VersionFailure),
    Setting(SettingFailure),
    Identity,
    StoredCategory,
    StoredAnnotation,
}
#[derive(Debug)]
pub(super) enum DatabaseFailure {
    Constraint(diesel::result::DatabaseErrorKind),
    MissingRow,
    QueryBuilder,
    Decode,
    Encode,
    Rollback,
    TransactionState,
    Integer,
    CString,
    CloseHandle,
    Unrecognized,
}
#[derive(Debug)]
pub(super) enum SettingFailure {
    Json,
    Deadline,
    DayStartOffset,
    StatsPolicyVersion,
    MetadataVersion,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum VersionFailure {
    DayExhausted,
    MetadataExhausted,
    InvalidRange,
    AnnotationWork,
}
impl WriteFailure {
    pub(super) fn database(error: &diesel::result::Error) -> Self {
        use diesel::result::Error;
        let kind = match error {
            Error::DatabaseError(kind, _) => DatabaseFailure::Constraint(*kind),
            Error::NotFound => DatabaseFailure::MissingRow,
            Error::QueryBuilderError(_) => DatabaseFailure::QueryBuilder,
            Error::DeserializationError(_) => DatabaseFailure::Decode,
            Error::SerializationError(_) => DatabaseFailure::Encode,
            Error::RollbackErrorOnCommit { .. } | Error::RollbackTransaction => {
                DatabaseFailure::Rollback
            }
            Error::AlreadyInTransaction
            | Error::NotInTransaction
            | Error::BrokenTransactionManager => DatabaseFailure::TransactionState,
            Error::IntegerConversion(_) => DatabaseFailure::Integer,
            Error::InvalidCString(_) => DatabaseFailure::CString,
            Error::ClosingHandle(_) => DatabaseFailure::CloseHandle,
            _ => DatabaseFailure::Unrecognized,
        };
        Self::Database(kind)
    }
    pub(super) fn version(error: &BumpVersionError) -> Self {
        match error {
            BumpVersionError::Database { source } => Self::database(source),
            BumpVersionError::Setting { source } => Self::setting(source),
            BumpVersionError::DayVersion => Self::Version(VersionFailure::DayExhausted),
            BumpVersionError::MetadataVersion => Self::Version(VersionFailure::MetadataExhausted),
            BumpVersionError::InvalidRange => Self::Version(VersionFailure::InvalidRange),
            BumpVersionError::AnnotationWork => Self::Version(VersionFailure::AnnotationWork),
        }
    }
    pub(super) fn setting(error: &AccessSettingError) -> Self {
        match error {
            AccessSettingError::Database { source } => Self::database(source),
            AccessSettingError::Version { source } => Self::version(source),
            AccessSettingError::Json { .. } => Self::Setting(SettingFailure::Json),
            AccessSettingError::Deadline { .. } => Self::Setting(SettingFailure::Deadline),
            AccessSettingError::DayStartOffset => Self::Setting(SettingFailure::DayStartOffset),
            AccessSettingError::StatsPolicyVersion => {
                Self::Setting(SettingFailure::StatsPolicyVersion)
            }
            AccessSettingError::MetadataVersion => Self::Setting(SettingFailure::MetadataVersion),
        }
    }
    pub(super) fn into_protocol_error(self) -> ProtocolError {
        let kind = match &self {
            Self::Version(VersionFailure::DayExhausted | VersionFailure::MetadataExhausted) => {
                ErrorKind::VersionExhausted
            }
            Self::Setting(setting) => {
                tracing::error!(failure = ?setting, "database write failed");
                return ProtocolError::new(ErrorKind::Internal);
            }
            Self::Database(database) => {
                if let DatabaseFailure::Constraint(kind) = database {
                    tracing::error!(failure = ?self, database_kind = ?kind, "database write failed");
                } else {
                    tracing::error!(failure = ?self, "database write failed");
                }
                return ProtocolError::new(ErrorKind::Internal);
            }
            Self::Version(VersionFailure::InvalidRange | VersionFailure::AnnotationWork)
            | Self::Identity
            | Self::StoredCategory
            | Self::StoredAnnotation => ErrorKind::Internal,
        };
        tracing::error!(failure = ?self, "database write failed");
        ProtocolError::new(kind)
    }
}
