//! Storage error boundary for the query shell.
#[derive(Debug, snafu::Snafu)]
#[snafu(display("could not read SQLite query snapshot: {source}"))]
pub(crate) struct ReadQueryError {
    pub source: diesel::result::Error,
}
impl From<diesel::result::Error> for crate::query::RunQueryError {
    fn from(source: diesel::result::Error) -> Self {
        crate::query::RunQueryError::Read {
            source: ReadQueryError { source },
        }
    }
}
