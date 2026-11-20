//! Localized startup failure text and recovery commands for the error window.

use std::path::{Path, PathBuf};

use quartz_protocol::{CoreError, CoreErrorCode, cli};

use crate::format::{Arg, Formatter, Msg};

/// One line of recovery help below the failure message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum RecoveryLine {
    /// The heading of the backup list.
    BackupsHeading(String),
    /// A migration backup the user can restore.
    BackupPath(PathBuf),
    /// A command that restores the backup above it, meant to be copied.
    RestoreCommand(String),
    /// A caution about restoring, or how to restore a backup that has no command line.
    Warning(String),
}

/// The text of the startup error window.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CoreErrorPresentation {
    pub title: String,
    pub message: String,
    pub recovery: Vec<RecoveryLine>,
}

/// The title and message of one failure code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FailureText {
    title: Msg,
    message: Msg,
}

impl FailureText {
    fn of(code: CoreErrorCode) -> Self {
        let (title, message) = match code {
            CoreErrorCode::NewerDatabase => (
                Msg::CoreErrorNewerDatabaseTitle,
                Msg::CoreErrorNewerDatabaseMessage,
            ),
            CoreErrorCode::MigrationFailed => (
                Msg::CoreErrorMigrationFailedTitle,
                Msg::CoreErrorMigrationFailedMessage,
            ),
            CoreErrorCode::OtherSession => (
                Msg::CoreErrorOtherSessionTitle,
                Msg::CoreErrorOtherSessionMessage,
            ),
            // A code from a newer core than this UI knows also gets the generic text; the test
            // below fails when a code of this build has no text of its own.
            CoreErrorCode::CoreStartFailed | _ => (
                Msg::CoreErrorStartFailedTitle,
                Msg::CoreErrorStartFailedMessage,
            ),
        };
        Self { title, message }
    }
}

/// The command that restores `backup`; `None` when the path cannot be written inside double
/// quotes, because it is not Unicode or contains a quote.
fn restore_command(backup: &Path) -> Option<String> {
    let path = backup.to_str().filter(|path| !path.contains('"'))?;
    Some(format!(
        "{} {} {} \"{path}\"",
        cli::PROGRAM,
        cli::RESTORE_BACKUP,
        cli::CONFIRM
    ))
}

impl CoreErrorPresentation {
    pub(crate) fn new(error: &CoreError, formatter: Formatter) -> Self {
        let text = FailureText::of(error.code);
        let mut recovery = Vec::new();
        if !error.backup_paths.is_empty() {
            recovery.push(RecoveryLine::BackupsHeading(
                formatter.text(Msg::CoreErrorBackups),
            ));
            for backup in &error.backup_paths {
                recovery.push(RecoveryLine::BackupPath(backup.clone()));
                recovery.push(match restore_command(backup) {
                    Some(command) => RecoveryLine::RestoreCommand(command),
                    None => RecoveryLine::Warning(formatter.text_with(
                        Msg::CoreErrorRestoreManually,
                        &[Arg::Command.with(&format!("{} {}", cli::PROGRAM, cli::RESTORE_BACKUP))],
                    )),
                });
            }
            recovery.push(RecoveryLine::Warning(
                formatter.text(Msg::CoreErrorRestoreWarning),
            ));
        }
        Self {
            title: formatter.text(text.title),
            message: formatter.text(text.message),
            recovery,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use quartz_i18n::Language;

    use super::*;

    #[test]
    fn localized_failure_includes_exact_path_and_restore_command() {
        let backup = PathBuf::from("C:/数据/backup with spaces.db");
        let error = CoreError::new(CoreErrorCode::MigrationFailed, vec![backup.clone()]);
        for (language, title) in [
            (Language::SimplifiedChinese, "数据库升级失败"),
            (Language::English, "Database upgrade failed"),
        ] {
            let presentation = CoreErrorPresentation::new(&error, Formatter::new(language));
            assert_eq!(presentation.title, title);
            assert!(!presentation.message.is_empty());
            assert!(matches!(
                presentation.recovery.as_slice(),
                [
                    RecoveryLine::BackupsHeading(_),
                    RecoveryLine::BackupPath(path),
                    RecoveryLine::RestoreCommand(command),
                    RecoveryLine::Warning(_),
                ] if *path == backup
                    && command == "quartz restore-backup --yes \"C:/数据/backup with spaces.db\""
            ));
        }
    }

    #[test]
    fn a_path_with_a_quote_gets_instructions_instead_of_a_command() {
        let error = CoreError::new(
            CoreErrorCode::MigrationFailed,
            vec![PathBuf::from("C:/a\"b.db")],
        );
        let presentation = CoreErrorPresentation::new(&error, Formatter::new(Language::English));
        assert!(matches!(
            presentation.recovery.get(2),
            Some(RecoveryLine::Warning(text)) if text.contains("quartz restore-backup")
        ));
    }

    #[test]
    fn every_code_has_its_own_text_without_inspecting_error_text() {
        let titles: BTreeSet<String> = CoreErrorCode::ALL
            .into_iter()
            .map(|code| {
                CoreErrorPresentation::new(
                    &CoreError::new(code, vec![]),
                    Formatter::new(Language::English),
                )
                .title
            })
            .collect();
        assert_eq!(titles.len(), CoreErrorCode::ALL.len());
    }
}
