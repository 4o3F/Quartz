//! Offline backup restoration with confirmation and instance exclusion.

use crate::{data_directory, platform, store};
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

pub(crate) fn restore_backup(
    file: &std::path::Path,
    confirmation: RestoreConfirmation,
) -> Result<(), RestoreBackupCommandError> {
    let directory = data_directory::resolve().context(ResolveDirectorySnafu)?;
    eprintln!(
        "Restoring backup {} will replace {}. All records written after the backup will be lost.",
        file.display(),
        directory.join("quartz.db").display()
    );
    snafu::ensure!(
        confirmation == RestoreConfirmation::Confirmed,
        ConfirmationRequiredSnafu
    );
    data_directory::create(&directory).context(CreateDirectorySnafu)?;
    let _instance = match platform::acquire_instance(&directory).context(LockSnafu)? {
        platform::AcquireInstance::Acquired(instance) => instance,
        platform::AcquireInstance::SameSession | platform::AcquireInstance::OtherSession => {
            return CoreRunningSnafu.fail();
        }
    };
    store::restore_backup(&directory, file).context(RestoreSnafu)?;
    println!("Database backup restored.");
    Ok(())
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
