//! Storage error boundary for snapshot reads.
#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum ReadQueryError {
    #[snafu(display("could not read SQLite query snapshot: {source}"))]
    Read { source: diesel::result::Error },
    #[snafu(display("invalid persisted query settings"))]
    InvalidSettings,
    #[snafu(display("invalid persisted capture facts"))]
    InvalidFacts,
}
impl From<diesel::result::Error> for ReadQueryError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Read { source }
    }
}
