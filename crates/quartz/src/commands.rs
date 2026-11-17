//! Offline backup restoration with confirmation and instance exclusion.

use crate::{
    data_directory::{self, DataDirectory},
    platform, store,
};
use snafu::{ResultExt, Snafu};

#[derive(Debug, Snafu)]
pub(crate) enum RestoreBackupCommandError {
    #[snafu(display("could not resolve restore data directory: {source}"))]
    ResolveDirectory {
        source: data_directory::ResolveDataDirectoryError,
    },
    #[snafu(display("{source}"))]
    CreateDirectory {
        source: data_directory::CreateDataDirectoryError,
    },
    #[snafu(display("could not acquire data-directory lock"))]
    Lock {
        source: platform::AcquireInstanceError,
    },
    #[snafu(display("stop the Quartz core before restoring a backup"))]
    CoreRunning,
    #[snafu(display("restore requires --yes"))]
    ConfirmationRequired,
    #[snafu(display("could not restore backup"))]
    Restore { source: store::RestoreBackupError },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestoreConfirmation {
    Required,
    Confirmed,
}

pub(crate) struct RestoreBackupCommand<'file> {
    file: &'file std::path::Path,
    confirmation: RestoreConfirmation,
}
impl<'file> RestoreBackupCommand<'file> {
    pub(crate) fn new(file: &'file std::path::Path, confirmation: RestoreConfirmation) -> Self {
        Self { file, confirmation }
    }
    pub(crate) fn run(&self) -> Result<(), RestoreBackupCommandError> {
        let directory = DataDirectory::resolve().context(ResolveDirectorySnafu)?;
        eprintln!(
            "Restoring backup {} will replace {}. All records written after the backup will be lost.",
            self.file.display(),
            directory.database().display()
        );
        snafu::ensure!(
            self.confirmation == RestoreConfirmation::Confirmed,
            ConfirmationRequiredSnafu
        );
        directory.create().context(CreateDirectorySnafu)?;
        let _instance = match directory.acquire_instance().context(LockSnafu)? {
            platform::AcquireInstance::Acquired(instance) => instance,
            platform::AcquireInstance::SameSession | platform::AcquireInstance::OtherSession => {
                return CoreRunningSnafu.fail();
            }
        };
        directory.restore_backup(self.file).context(RestoreSnafu)?;
        println!("Database backup restored.");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn lock_failure_preserves_platform_and_io_sources() {
        let source = platform::AcquireInstanceError::OpenLockFile {
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        let error = Err::<(), _>(source)
            .context(LockSnafu)
            .expect_err("lock failure reaches command");
        let platform_error = error
            .source()
            .and_then(|source| source.downcast_ref::<platform::AcquireInstanceError>())
            .expect("original platform error");
        assert_eq!(
            platform_error
                .source()
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .expect("original I/O cause")
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(error.to_string(), "could not acquire data-directory lock");
    }

    #[test]
    fn confirmation_and_running_core_diagnostics_remain_english() {
        assert_eq!(
            RestoreBackupCommandError::ConfirmationRequired.to_string(),
            "restore requires --yes"
        );
        assert_eq!(
            RestoreBackupCommandError::CoreRunning.to_string(),
            "stop the Quartz core before restoring a backup"
        );
    }
}
