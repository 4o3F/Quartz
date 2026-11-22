//! Storage error boundary for snapshot reads.
#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum ReadQueryError {
    #[snafu(display("{source}"))]
    Facts {
        source: crate::store::facts::ReadFactsError,
    },
    #[snafu(display("query reader is unavailable"))]
    ReaderUnavailable,
    #[snafu(display("could not read SQLite query snapshot: {source}"))]
    Read { source: diesel::result::Error },
    #[snafu(display("invalid persisted query settings"))]
    InvalidSettings,
    #[snafu(display("invalid persisted capture facts"))]
    InvalidFacts,
    #[snafu(display("invalid persisted category metadata"))]
    InvalidCategories,
}
impl From<diesel::result::Error> for ReadQueryError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Read { source }
    }
}

impl From<super::settings::AccessSettingError> for ReadQueryError {
    fn from(error: super::settings::AccessSettingError) -> Self {
        match error {
            super::settings::AccessSettingError::Database { source } => Self::Read { source },
            super::settings::AccessSettingError::Version { .. }
            | super::settings::AccessSettingError::Json { .. }
            | super::settings::AccessSettingError::Deadline { .. }
            | super::settings::AccessSettingError::DayStartOffset
            | super::settings::AccessSettingError::MetadataVersion => Self::InvalidSettings,
        }
    }
}

impl From<crate::store::facts::ReadFactsError> for ReadQueryError {
    fn from(source: crate::store::facts::ReadFactsError) -> Self {
        Self::Facts { source }
    }
}
