//! Validates offline simulation paths against the production data directory.

use crate::data_directory::DataDirectory;
use snafu::{ResultExt, Snafu};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Snafu)]
pub(crate) enum ValidateSimulationError {
    #[snafu(display("could not resolve production data directory: {source}"))]
    Directory {
        source: crate::data_directory::ResolveDataDirectoryError,
    },
    #[snafu(display("could not resolve simulation path: {source}"))]
    Resolve { source: std::io::Error },
    #[snafu(display("simulation database must be outside the production data directory"))]
    Production,
    #[snafu(display("simulation database already exists"))]
    Exists,
}

#[derive(Debug)]
pub(crate) struct SimulationPath(PathBuf);
impl SimulationPath {
    pub(crate) fn validate(
        path: &Path,
        production: &DataDirectory,
    ) -> Result<Self, ValidateSimulationError> {
        let path = Self::resolved(path).context(ResolveSnafu)?;
        let root = Self::resolved(production.as_path()).context(ResolveSnafu)?;
        // Windows paths compare without case, including canonicalized UNC prefixes.
        #[cfg(windows)]
        let inside = path.to_string_lossy().to_lowercase().starts_with(&format!(
            "{}\\",
            root.to_string_lossy().trim_end_matches('\\').to_lowercase()
        )) || path
            .to_string_lossy()
            .eq_ignore_ascii_case(&root.to_string_lossy());
        #[cfg(not(windows))]
        let inside = path.starts_with(&root);
        snafu::ensure!(!inside, ProductionSnafu);
        Ok(Self(path))
    }
    pub(crate) fn offline(path: &Path) -> Result<Self, ValidateSimulationError> {
        let directory = DataDirectory::resolve().context(DirectorySnafu)?;
        Self::validate(path, &directory)
    }
    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }
    pub(crate) fn reserve(&self) -> Result<(), ValidateSimulationError> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.0)
            .map(|_| ())
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::AlreadyExists {
                    ValidateSimulationError::Exists
                } else {
                    ValidateSimulationError::Resolve { source }
                }
            })
    }
    fn resolved(path: &Path) -> std::io::Result<PathBuf> {
        let absolute = std::path::absolute(path)?;
        let mut normalized = PathBuf::new();
        for component in absolute.components() {
            match component {
                Component::ParentDir => {
                    normalized.pop();
                }
                Component::CurDir => {}
                component => normalized.push(component.as_os_str()),
            }
        }
        // Resolve every existing ancestor, so directory junctions and symlinks are checked.
        if normalized.try_exists()? {
            let canonical = fs::canonicalize(normalized)?;
            #[cfg(windows)]
            if let Some(text) = canonical.to_str() {
                if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
                    return Ok(PathBuf::from(format!(r"\\{unc}")));
                }
                if let Some(disk) = text.strip_prefix(r"\\?\") {
                    return Ok(PathBuf::from(disk));
                }
            }
            return Ok(canonical);
        }
        let parent = normalized
            .parent()
            .ok_or_else(|| std::io::Error::other("path has no parent"))?;
        Ok(Self::resolved(parent)?.join(
            normalized
                .file_name()
                .ok_or_else(|| std::io::Error::other("path has no filename"))?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_existing_files_and_production_descendants() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let production = DataDirectory::new(temporary.path().join("production"));
        for path in [
            production.database(),
            production.as_path().join("nested/simulation.db"),
        ] {
            assert!(matches!(
                SimulationPath::validate(&path, &production),
                Err(ValidateSimulationError::Production)
            ));
        }
        let path = temporary.path().join("simulation.db");
        let simulation = SimulationPath::validate(&path, &production).expect("independent path");
        simulation.reserve().expect("new file");
        assert!(matches!(
            simulation.reserve(),
            Err(ValidateSimulationError::Exists)
        ));
    }
}
