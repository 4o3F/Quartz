//! Write diagnostics contain typed failure classes and no stored values or database messages.

use crate::{
    core::autostart::AccessAutostartError,
    store::{settings::AccessSettingError, versions::BumpVersionError},
};
use quartz_protocol::{ErrorKind, ProtocolError};

#[derive(Debug)]
pub(crate) enum WriteFailure {
    Database(DatabaseFailure),
    Version(VersionFailure),
    Setting(SettingFailure),
    Autostart(AutostartFailure),
    Identity,
    StoredCategory,
    StoredAnnotation,
    Rule(RuleFailure),
}
#[derive(Debug)]
pub(crate) enum AutostartFailure {
    ResolveExecutable,
    EncodeExecutablePath,
    ReadRegistry,
    WriteRegistry,
    DeleteRegistry,
    ConvertRegistryLength,
    DecodeRegistryValue,
}
#[derive(Debug)]
pub(crate) enum RuleFailure {
    MissingId,
    PreconditionMismatch,
    CommitEpoch,
    CommitSnapshot,
    SnapshotCompile,
    SnapshotEffective,
    SnapshotTree,
    SnapshotIdentity,
}
#[derive(Debug)]
pub(crate) enum DatabaseFailure {
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
pub(crate) enum SettingFailure {
    Json,
    Deadline,
    DayStartOffset,
    StatsPolicyVersion,
    MetadataVersion,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VersionFailure {
    DayExhausted,
    MetadataExhausted,
    InvalidRange,
    AnnotationWork,
}
impl WriteFailure {
    pub(crate) fn rule_persist(error: &crate::store::rules::ExecuteRuleWritesError) -> Self {
        use crate::store::rules::ExecuteRuleWritesError;
        match error {
            ExecuteRuleWritesError::Database { source } => Self::database(source),
            ExecuteRuleWritesError::MissingId => Self::Rule(RuleFailure::MissingId),
            ExecuteRuleWritesError::PreconditionMismatch { .. } => {
                Self::Rule(RuleFailure::PreconditionMismatch)
            }
        }
    }
    pub(crate) fn rule_commit(
        error: &quartz_domain::rules::registry::CommitRuleDecisionError,
    ) -> Self {
        use quartz_domain::rules::registry::CommitRuleDecisionError;
        Self::Rule(match error {
            CommitRuleDecisionError::StaleEpoch { .. } => RuleFailure::CommitEpoch,
            CommitRuleDecisionError::Snapshot { .. } => RuleFailure::CommitSnapshot,
        })
    }
    pub(crate) fn rule_snapshot(
        error: &quartz_domain::rules::registry::BuildRuleSnapshotError,
    ) -> Self {
        use quartz_domain::rules::registry::BuildRuleSnapshotError;
        Self::Rule(match error {
            BuildRuleSnapshotError::Compile { .. } => RuleFailure::SnapshotCompile,
            BuildRuleSnapshotError::Effective { .. } => RuleFailure::SnapshotEffective,
            BuildRuleSnapshotError::Tree { .. } => RuleFailure::SnapshotTree,
            BuildRuleSnapshotError::Identity { .. } => RuleFailure::SnapshotIdentity,
        })
    }

    pub(crate) fn database(error: &diesel::result::Error) -> Self {
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
    pub(crate) fn version(error: &BumpVersionError) -> Self {
        match error {
            BumpVersionError::Database { source } => Self::database(source),
            BumpVersionError::Setting { source } => Self::setting(source),
            BumpVersionError::DayVersion => Self::Version(VersionFailure::DayExhausted),
            BumpVersionError::MetadataVersion => Self::Version(VersionFailure::MetadataExhausted),
            BumpVersionError::InvalidRange => Self::Version(VersionFailure::InvalidRange),
            BumpVersionError::AnnotationWork => Self::Version(VersionFailure::AnnotationWork),
        }
    }
    pub(crate) fn setting(error: &AccessSettingError) -> Self {
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
    pub(crate) fn autostart(error: &AccessAutostartError) -> Self {
        Self::Autostart(match error {
            AccessAutostartError::ResolveExecutable { .. } => AutostartFailure::ResolveExecutable,
            AccessAutostartError::EncodeExecutablePath => AutostartFailure::EncodeExecutablePath,
            AccessAutostartError::ReadRegistry { .. } => AutostartFailure::ReadRegistry,
            AccessAutostartError::WriteRegistry { .. } => AutostartFailure::WriteRegistry,
            AccessAutostartError::DeleteRegistry { .. } => AutostartFailure::DeleteRegistry,
            AccessAutostartError::ConvertRegistryLength { .. } => {
                AutostartFailure::ConvertRegistryLength
            }
            AccessAutostartError::DecodeRegistryValue { .. } => {
                AutostartFailure::DecodeRegistryValue
            }
        })
    }
    pub(crate) fn into_protocol_error(self) -> ProtocolError {
        let kind = match &self {
            Self::Version(VersionFailure::DayExhausted | VersionFailure::MetadataExhausted) => {
                ErrorKind::VersionExhausted
            }
            Self::Setting(setting) => {
                tracing::error!(failure = ?setting, "database write failed");
                return ProtocolError::new(ErrorKind::Internal);
            }
            Self::Autostart(autostart) => {
                tracing::error!(failure = ?autostart, "autostart registry access failed");
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
            Self::Rule(rule) => {
                tracing::error!(failure = ?rule, "rule write failed");
                return ProtocolError::new(ErrorKind::Internal);
            }
        };
        tracing::error!(failure = ?self, "database write failed");
        ProtocolError::new(kind)
    }
}
