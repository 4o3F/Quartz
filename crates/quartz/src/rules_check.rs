//! Offline file validation against a read-only snapshot of installed rule sets.

use crate::{data_directory::DataDirectory, store};
use quartz_domain::rules::{
    self, Diagnostic, Enablement, InstalledSet, ParseContext, ReferenceCatalog, RuleKind,
    RuleOrigin, RuleSet,
};
use snafu::{ResultExt, Snafu};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Snafu)]
pub(crate) enum CheckRulesError {
    #[snafu(display("could not inspect installed rules: {source}"))]
    Database { source: store::ReadRulesError },
    #[snafu(display("could not inspect rule database path"))]
    InspectDatabase { source: std::io::Error },
    #[snafu(display("rule validation failed with {count} error(s)"))]
    Invalid { count: usize },
    #[snafu(display("rule sibling order exceeds the supported range"))]
    SiblingOrderOverflow { source: std::num::TryFromIntError },
    #[snafu(display("could not resolve rule database directory"))]
    Directory {
        source: crate::data_directory::ResolveDataDirectoryError,
    },
}
#[derive(Debug, Snafu)]
#[snafu(display("could not read rule file {}: {source}", file.display()))]
struct ReadRuleFileError {
    file: PathBuf,
    source: std::io::Error,
}

pub(crate) struct CheckRulesCommand<'files> {
    files: &'files [PathBuf],
}
impl<'files> CheckRulesCommand<'files> {
    pub(crate) fn new(files: &'files [PathBuf]) -> Self {
        Self { files }
    }
    pub(crate) fn run(&self) -> Result<(), CheckRulesError> {
        #[cfg(windows)]
        let database = Some(DataDirectory::resolve().context(DirectorySnafu)?.database());
        #[cfg(not(windows))]
        let database = Self::optional_database(std::env::var_os("LOCALAPPDATA"))?;
        self.check(database.as_deref())
    }
    #[cfg(any(not(windows), test))]
    fn optional_database(
        local: Option<std::ffi::OsString>,
    ) -> Result<Option<PathBuf>, CheckRulesError> {
        local
            .map(|path| {
                DataDirectory::from_local_app_data(Some(path))
                    .map(|directory| directory.database())
                    .context(DirectorySnafu)
            })
            .transpose()
    }
    fn check(&self, database: Option<&Path>) -> Result<(), CheckRulesError> {
        let database = match database {
            Some(path) if path.try_exists().context(InspectDatabaseSnafu)? => Some(path),
            _ => None,
        };
        let (mut installed, references) = if let Some(path) = database {
            let loaded = store::ReadOnlyStore::open(path)
                .context(DatabaseSnafu)?
                .rule_sets()
                .load()
                .map_err(store::ReadRulesError::from)
                .context(DatabaseSnafu)?;
            (loaded.installed_sets(), loaded.references)
        } else {
            (Vec::new(), ReferenceCatalog::default())
        };
        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let mut reads = Vec::new();
        let mut input_sets = Vec::new();
        for file in self.files {
            let source = match fs::read_to_string(file).context(ReadRuleFileSnafu { file }) {
                Ok(source) => source,
                Err(error) => {
                    reads.push(error);
                    continue;
                }
            };
            match RuleSet::parse(
                &file.to_string_lossy(),
                &source,
                ParseContext {
                    origin: RuleOrigin::User,
                    references: &references,
                },
            ) {
                Ok(set) => {
                    let existing = installed
                        .iter()
                        .position(|entry| entry.set.header.key == set.header.key)
                        .map(|index| installed.remove(index));
                    input_sets.push(InstalledSet {
                        set,
                        version: existing
                            .as_ref()
                            .map_or(1, |old| old.version.saturating_add(1)),
                        enabled: existing
                            .as_ref()
                            .map_or(Enablement::Enabled, |old| old.enabled),
                        sibling_order: match &existing {
                            Some(old) => old.sibling_order,
                            None => i64::try_from(input_sets.len())
                                .context(SiblingOrderOverflowSnafu)?,
                        },
                    });
                }
                Err(error) => diagnostics.extend(error.into_diagnostics()),
            }
        }
        installed.extend(input_sets);
        // Tree validation covers all kinds in one pass; family only selects the provisional list.
        diagnostics.extend(rules::Resolution::resolve(&installed, RuleKind::Privacy).diagnostics);
        for diagnostic in &diagnostics {
            eprintln!("{diagnostic}");
        }
        for error in &reads {
            eprintln!("{error}");
        }
        let count = diagnostics.len() + reads.len();
        snafu::ensure!(count == 0, InvalidSnafu { count });
        println!("Checked {} rule file(s).", self.files.len());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn optional_rule_database_rejects_empty_and_relative_environment_roots() {
        for root in ["", "relative"] {
            assert!(matches!(
                super::CheckRulesCommand::optional_database(Some(root.into())),
                Err(super::CheckRulesError::Directory { .. })
            ));
        }
        assert!(
            super::CheckRulesCommand::optional_database(None)
                .expect("absent root")
                .is_none()
        );
        let root = std::env::temp_dir();
        assert_eq!(
            super::CheckRulesCommand::optional_database(Some(root.clone().into_os_string()))
                .expect("absolute root"),
            Some(root.join("Quartz").join("quartz.db"))
        );
    }
    use super::*;
    #[test]
    fn checks_offline_without_creating_a_database() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let file = directory.path().join("privacy.toml");
        fs::write(&file, "format = 'quartz-rules/1'\n[set]\nkey = 'user.bank'\nkind = 'privacy'\nname = 'Bank'\n[[rule]]\nid = 'bank'\nlabel = 'Hide bank'\nwhen = [{ field = 'web.domain', op = 'domain_suffix', value = 'bank.example' }]\npreset = 'title_hidden'\n").expect("write rule");
        let db = directory.path().join("quartz.db");
        CheckRulesCommand::new(&[file])
            .check(Some(&db))
            .expect("offline check");
        assert!(!db.exists());
    }
    #[test]
    fn unreadable_files_fail_without_starting_core() {
        assert!(matches!(
            CheckRulesCommand::new(&[PathBuf::from("/missing/quartz-rule.toml")]).check(None),
            Err(CheckRulesError::Invalid { count: 1 })
        ));
    }
}
