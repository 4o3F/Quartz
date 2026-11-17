//! Localized startup failure text and recovery commands for the error window.

use quartz_protocol::{CoreError, CoreErrorCode};

#[derive(Debug)]
pub(crate) struct CoreErrorPresentation {
    pub title: String,
    pub message: String,
    pub recovery: Vec<String>,
}
impl CoreErrorPresentation {
    pub(crate) fn new(error: &CoreError, locale: &str) -> Self {
        let (title, message) = match error.code {
            CoreErrorCode::NewerDatabase => (
                "ui.core_error.newer_database.title",
                "ui.core_error.newer_database.message",
            ),
            CoreErrorCode::MigrationFailed => (
                "ui.core_error.migration_failed.title",
                "ui.core_error.migration_failed.message",
            ),
            CoreErrorCode::OtherSession => (
                "ui.core_error.other_session.title",
                "ui.core_error.other_session.message",
            ),
            _ => (
                "ui.core_error.start_failed.title",
                "ui.core_error.start_failed.message",
            ),
        };
        let mut recovery = Vec::new();
        if !error.backup_paths.is_empty() {
            recovery.push(
                quartz_i18n::translate_in_locale("ui.core_error.backups", locale).into_owned(),
            );
            for backup in &error.backup_paths {
                recovery.push(backup.display().to_string());
                recovery.push(format!(
                    "quartz restore-backup --yes \"{}\"",
                    backup.display()
                ));
            }
            recovery.push(
                quartz_i18n::translate_in_locale("ui.core_error.restore_warning", locale)
                    .into_owned(),
            );
        }
        Self {
            title: quartz_i18n::translate_in_locale(title, locale).into_owned(),
            message: quartz_i18n::translate_in_locale(message, locale).into_owned(),
            recovery,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn localized_failure_includes_exact_path_and_restore_command() {
        let error = CoreError::new(
            CoreErrorCode::MigrationFailed,
            vec!["C:/数据/backup with spaces.db".into()],
        );
        for (locale, title) in [
            ("zh-CN", "数据库升级失败"),
            ("en", "Database upgrade failed"),
        ] {
            let presentation = CoreErrorPresentation::new(&error, locale);
            assert_eq!(presentation.title, title);
            assert!(
                presentation
                    .recovery
                    .iter()
                    .any(|text| text == "C:/数据/backup with spaces.db")
            );
            assert!(presentation.recovery.iter().any(
                |text| text == "quartz restore-backup --yes \"C:/数据/backup with spaces.db\""
            ));
            assert!(!presentation.message.is_empty());
        }
    }
    #[test]
    fn codes_choose_distinct_messages_without_inspecting_error_text() {
        let mut titles = std::collections::BTreeSet::new();
        for code in [
            CoreErrorCode::NewerDatabase,
            CoreErrorCode::MigrationFailed,
            CoreErrorCode::OtherSession,
            CoreErrorCode::CoreStartFailed,
        ] {
            titles.insert(CoreErrorPresentation::new(&CoreError::new(code, vec![]), "en").title);
        }
        assert_eq!(titles.len(), 4);
    }
}
