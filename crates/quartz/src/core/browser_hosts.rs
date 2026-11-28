//! Browser Native Messaging host manifest generation and registry management (05 §2.1).

use std::fmt;
#[cfg(any(windows, test))]
use std::fs::{self, OpenOptions};
#[cfg(any(windows, test))]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
#[cfg(any(windows, test))]
use std::sync::atomic::{AtomicU64, Ordering};

use quartz_protocol::{
    BrowserHostConnection, BrowserHostEntry, BrowserHostRegistration, BrowserHostsStatusResponse,
    BrowserKind, BrowserStream,
};
use serde::{Deserialize, Serialize};
use snafu::Snafu;

use crate::store::browser::{CHROMIUM_EXTENSION_ID, FIREFOX_EXTENSION_ID};

/// Native Messaging host name shared by Chrome, Edge, and Firefox (05 §2.1).
pub(crate) const NATIVE_HOST_NAME: &str = "dev.4o3f.quartz";

/// Human-readable description written into Native Messaging host manifests.
const NATIVE_HOST_DESCRIPTION: &str = "Quartz browser activity host";

/// Native Messaging transport type required by all supported browsers.
const NATIVE_HOST_TYPE: &str = "stdio";

/// Executable file name expected beside the running `quartz.exe`.
pub(crate) const HOST_EXECUTABLE_NAME: &str = "quartz-host.exe";

/// Shared manifest file name used by Chrome and Edge under `native-hosts/`.
pub(crate) const CHROMIUM_MANIFEST_FILE: &str = "dev.4o3f.quartz.chrome.json";

/// Manifest file name used by Firefox under `native-hosts/`.
pub(crate) const FIREFOX_MANIFEST_FILE: &str = "dev.4o3f.quartz.firefox.json";

/// Fixed order of browsers returned in `browser_hosts.status` responses.
pub(crate) const ALL_BROWSERS: [BrowserKind; 3] =
    [BrowserKind::Chrome, BrowserKind::Edge, BrowserKind::Firefox];

/// HKCU registry subkey for Google Chrome Native Messaging host registration.
pub(crate) const CHROME_REGISTRY_SUBKEY: &str =
    r"Software\Google\Chrome\NativeMessagingHosts\dev.4o3f.quartz";

/// HKCU registry subkey for Microsoft Edge Native Messaging host registration.
pub(crate) const EDGE_REGISTRY_SUBKEY: &str =
    r"Software\Microsoft\Edge\NativeMessagingHosts\dev.4o3f.quartz";

/// HKCU registry subkey for Mozilla Firefox Native Messaging host registration.
pub(crate) const FIREFOX_REGISTRY_SUBKEY: &str =
    r"Software\Mozilla\NativeMessagingHosts\dev.4o3f.quartz";

/// Maximum attempts to create a collision-free temporary file in `native-hosts/`.
#[cfg(any(windows, test))]
const MAX_TEMP_FILE_ATTEMPTS: usize = 16;

/// Returns the HKCU registry subkey for a browser's Native Messaging host entry.
#[must_use]
pub(crate) const fn registry_subkey(browser: BrowserKind) -> &'static str {
    match browser {
        BrowserKind::Chrome => CHROME_REGISTRY_SUBKEY,
        BrowserKind::Edge => EDGE_REGISTRY_SUBKEY,
        BrowserKind::Firefox => FIREFOX_REGISTRY_SUBKEY,
    }
}

/// Returns the manifest file name used by a browser family.
#[must_use]
pub(crate) const fn manifest_file_name(browser: BrowserKind) -> &'static str {
    match browser {
        BrowserKind::Chrome | BrowserKind::Edge => CHROMIUM_MANIFEST_FILE,
        BrowserKind::Firefox => FIREFOX_MANIFEST_FILE,
    }
}

/// Returns the Chromium `allowed_origins` entry for the fixed Quartz extension ID.
#[must_use]
pub(crate) fn chromium_allowed_origin() -> String {
    format!("chrome-extension://{CHROMIUM_EXTENSION_ID}/")
}

/// Typed failures when inspecting or updating Native Messaging host registrations.
#[derive(Debug, Snafu)]
pub(crate) enum ManageBrowserHostsError {
    #[snafu(display("could not resolve current executable path for browser host registration"))]
    ResolveCoreExecutable { source: std::io::Error },
    #[snafu(display("current executable path has no parent directory"))]
    ResolveCoreExecutableDir,
    #[snafu(display("browser host executable path is not valid UTF-8"))]
    EncodeHostPath,
    #[snafu(display("browser host manifest path is not valid UTF-8"))]
    EncodeManifestPath,
    #[snafu(display("could not serialize native messaging host manifest"))]
    SerializeManifest { source: serde_json::Error },
    #[snafu(display("could not create native host manifest directory {}", path.display()))]
    CreateManifestDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not write native host manifest {}", path.display()))]
    WriteManifestFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not read native host manifest {}", path.display()))]
    ReadManifestFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not read native host registry key for {browser:?}"))]
    ReadHostRegistry {
        browser: BrowserKind,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[snafu(display("could not write native host registry key for {browser:?}"))]
    WriteHostRegistry {
        browser: BrowserKind,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[snafu(display("native host registry value length for {browser:?} exceeds platform bounds"))]
    ConvertHostRegistryLength {
        browser: BrowserKind,
        source: std::num::TryFromIntError,
    },
    #[snafu(display("native host registry value for {browser:?} is not valid UTF-16"))]
    DecodeHostRegistryValue {
        browser: BrowserKind,
        source: std::string::FromUtf16Error,
    },
}

/// Raw state of a browser's Native Messaging host registry key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RegistryHostEntry {
    /// The `dev.4o3f.quartz` registry key does not exist.
    Absent,
    /// The registry key exists, but its default value is absent or not a valid `REG_SZ`.
    WithoutStringValue,
    /// The registry key exists with the given default `REG_SZ` string value.
    Value(String),
}

/// Whether a required file currently exists as a regular file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePresence {
    /// A regular file exists at the path.
    Present,
    /// No regular file exists at the path.
    Absent,
}

/// Low-level HKCU registry access for browser Native Messaging host keys.
pub(crate) trait BrowserHostRegistry: Send + Sync {
    /// Reads the default string value of `browser`'s Native Messaging host key.
    fn read_default_value(
        &self,
        browser: BrowserKind,
    ) -> Result<RegistryHostEntry, ManageBrowserHostsError>;
    /// Creates or opens `browser`'s Native Messaging host key and sets its default value.
    fn write_default_value(
        &self,
        browser: BrowserKind,
        manifest_path: &str,
    ) -> Result<(), ManageBrowserHostsError>;
}

/// Filesystem operations used for manifest files and host-executable presence checks.
pub(crate) trait BrowserHostFs: Send + Sync {
    /// Reports whether `path` exists as a regular file.
    fn file_presence(&self, path: &Path) -> FilePresence;
    /// Reads `path` as UTF-8 text, returning `Ok(None)` when the file does not exist.
    fn read_to_string(&self, path: &Path) -> Result<Option<String>, ManageBrowserHostsError>;
    /// Atomically writes `contents` to `path` via a temporary file in the same directory.
    fn write_atomic(&self, path: &Path, contents: &str) -> Result<(), ManageBrowserHostsError>;
}

/// Core-facing port for Native Messaging host startup registration and status inspection.
pub(crate) trait BrowserHostsPort: Send + Sync {
    /// Idempotently writes manifests and HKCU keys for all supported browsers on core startup.
    fn register_all(&self);
    /// Inspects registration state for all supported browsers and merges live connection state.
    fn status(
        &self,
        streams: &[BrowserStream],
    ) -> Result<BrowserHostsStatusResponse, ManageBrowserHostsError>;
}

/// Production filesystem backing using `std::fs` with atomic same-directory renames.
#[cfg(any(windows, test))]
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct StdBrowserHostFs;

#[cfg(any(windows, test))]
impl StdBrowserHostFs {
    fn next_temp_path(target: &Path) -> Result<PathBuf, ManageBrowserHostsError> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let parent = target
            .parent()
            .ok_or_else(|| ManageBrowserHostsError::WriteManifestFile {
                path: target.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "manifest path has no parent directory",
                ),
            })?;
        let file_name = target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("manifest.json");
        let pid = std::process::id();
        let seq = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Ok(parent.join(format!(".{file_name}.{pid}.{seq}.tmp")))
    }
}

#[cfg(any(windows, test))]
impl BrowserHostFs for StdBrowserHostFs {
    fn file_presence(&self, path: &Path) -> FilePresence {
        match fs::metadata(path) {
            Ok(meta) if meta.is_file() => FilePresence::Present,
            Ok(_) | Err(_) => FilePresence::Absent,
        }
    }

    fn read_to_string(&self, path: &Path) -> Result<Option<String>, ManageBrowserHostsError> {
        match fs::read_to_string(path) {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ManageBrowserHostsError::ReadManifestFile {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    fn write_atomic(&self, path: &Path, contents: &str) -> Result<(), ManageBrowserHostsError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| {
                ManageBrowserHostsError::CreateManifestDir {
                    path: parent.to_path_buf(),
                    source,
                }
            })?;
        }

        let mut last_error = None;
        for _ in 0..MAX_TEMP_FILE_ATTEMPTS {
            let temp_path = Self::next_temp_path(path)?;
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
            {
                Ok(mut file) => {
                    let write_result = file
                        .write_all(contents.as_bytes())
                        .and_then(|()| file.sync_all());
                    drop(file);
                    if let Err(source) = write_result {
                        let _ = fs::remove_file(&temp_path);
                        return Err(ManageBrowserHostsError::WriteManifestFile {
                            path: path.to_path_buf(),
                            source,
                        });
                    }
                    if let Err(source) = fs::rename(&temp_path, path) {
                        let _ = fs::remove_file(&temp_path);
                        return Err(ManageBrowserHostsError::WriteManifestFile {
                            path: path.to_path_buf(),
                            source,
                        });
                    }
                    return Ok(());
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    last_error = Some(error);
                }
                Err(source) => {
                    return Err(ManageBrowserHostsError::WriteManifestFile {
                        path: path.to_path_buf(),
                        source,
                    });
                }
            }
        }

        Err(ManageBrowserHostsError::WriteManifestFile {
            path: path.to_path_buf(),
            source: last_error.unwrap_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "exhausted temporary manifest names",
                )
            }),
        })
    }
}

/// Resolves `quartz-host.exe` next to the currently running `quartz.exe`.
#[cfg(windows)]
pub(crate) fn resolve_host_executable() -> Result<PathBuf, ManageBrowserHostsError> {
    let current = std::env::current_exe()
        .map_err(|source| ManageBrowserHostsError::ResolveCoreExecutable { source })?;
    let parent = current
        .parent()
        .ok_or(ManageBrowserHostsError::ResolveCoreExecutableDir)?;
    Ok(parent.join(HOST_EXECUTABLE_NAME))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ChromiumHostManifest {
    name: String,
    description: String,
    path: String,
    #[serde(rename = "type")]
    host_type: String,
    allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FirefoxHostManifest {
    name: String,
    description: String,
    path: String,
    #[serde(rename = "type")]
    host_type: String,
    allowed_extensions: Vec<String>,
}

fn serialize_manifest(
    browser: BrowserKind,
    host_exe: &str,
) -> Result<String, ManageBrowserHostsError> {
    let rendered = match browser {
        BrowserKind::Chrome | BrowserKind::Edge => {
            serde_json::to_string_pretty(&ChromiumHostManifest {
                name: NATIVE_HOST_NAME.to_owned(),
                description: NATIVE_HOST_DESCRIPTION.to_owned(),
                path: host_exe.to_owned(),
                host_type: NATIVE_HOST_TYPE.to_owned(),
                allowed_origins: vec![chromium_allowed_origin()],
            })
        }
        BrowserKind::Firefox => serde_json::to_string_pretty(&FirefoxHostManifest {
            name: NATIVE_HOST_NAME.to_owned(),
            description: NATIVE_HOST_DESCRIPTION.to_owned(),
            path: host_exe.to_owned(),
            host_type: NATIVE_HOST_TYPE.to_owned(),
            allowed_extensions: vec![FIREFOX_EXTENSION_ID.to_owned()],
        }),
    }
    .map_err(|source| ManageBrowserHostsError::SerializeManifest { source })?;
    Ok(format!("{rendered}\n"))
}

fn manifest_matches(browser: BrowserKind, raw_json: &str, expected_host_exe: &str) -> bool {
    match browser {
        BrowserKind::Chrome | BrowserKind::Edge => {
            let Ok(parsed) = serde_json::from_str::<ChromiumHostManifest>(raw_json) else {
                return false;
            };
            parsed.name == NATIVE_HOST_NAME
                && parsed.host_type == NATIVE_HOST_TYPE
                && parsed.path == expected_host_exe
                && parsed.allowed_origins.as_slice() == [chromium_allowed_origin()]
        }
        BrowserKind::Firefox => {
            let Ok(parsed) = serde_json::from_str::<FirefoxHostManifest>(raw_json) else {
                return false;
            };
            parsed.name == NATIVE_HOST_NAME
                && parsed.host_type == NATIVE_HOST_TYPE
                && parsed.path == expected_host_exe
                && parsed.allowed_extensions.as_slice() == [FIREFOX_EXTENSION_ID]
        }
    }
}

fn path_to_utf8(
    path: &Path,
    error: fn() -> ManageBrowserHostsError,
) -> Result<&str, ManageBrowserHostsError> {
    path.to_str().ok_or_else(error)
}

/// Manages Native Messaging manifests and registry entries for all supported browsers.
pub(crate) struct NativeHostManager<R, F> {
    registry: R,
    fs: F,
    manifests_dir: PathBuf,
    resolve_host_exe: Box<dyn Fn() -> Result<PathBuf, ManageBrowserHostsError> + Send + Sync>,
    serialize_ops: Mutex<()>,
}

impl<R, F> fmt::Debug for NativeHostManager<R, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeHostManager")
            .field("manifests_dir", &self.manifests_dir)
            .finish_non_exhaustive()
    }
}

impl<R, F> NativeHostManager<R, F> {
    /// Creates a Native Messaging host manager backed by `registry`, `fs`, and `manifests_dir`.
    pub(crate) fn new(
        registry: R,
        fs: F,
        manifests_dir: PathBuf,
        resolve_host_exe: impl Fn() -> Result<PathBuf, ManageBrowserHostsError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            registry,
            fs,
            manifests_dir,
            resolve_host_exe: Box::new(resolve_host_exe),
            serialize_ops: Mutex::new(()),
        }
    }

    fn manifest_path(&self, browser: BrowserKind) -> PathBuf {
        self.manifests_dir.join(manifest_file_name(browser))
    }
}

impl<R: BrowserHostRegistry, F: BrowserHostFs> NativeHostManager<R, F> {
    fn register_browser(&self, browser: BrowserKind, host_exe_str: &str) {
        let manifest_path = self.manifest_path(browser);
        let manifest_path_str = match path_to_utf8(&manifest_path, || {
            ManageBrowserHostsError::EncodeManifestPath
        }) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(?browser, %error, "browser host manifest path is not valid UTF-8");
                return;
            }
        };
        let expected_manifest = match serialize_manifest(browser, host_exe_str) {
            Ok(json) => json,
            Err(error) => {
                tracing::warn!(?browser, %error, "could not serialize native host manifest");
                return;
            }
        };

        let needs_manifest_write = match self.fs.read_to_string(&manifest_path) {
            Ok(Some(existing)) => existing != expected_manifest,
            Ok(None) => true,
            Err(error) => {
                tracing::warn!(
                    ?browser,
                    %error,
                    "could not read existing native host manifest; attempting rewrite"
                );
                true
            }
        };
        if needs_manifest_write
            && let Err(error) = self.fs.write_atomic(&manifest_path, &expected_manifest)
        {
            tracing::warn!(?browser, %error, "could not write native host manifest");
            return;
        }

        let needs_registry_write = match self.registry.read_default_value(browser) {
            Ok(RegistryHostEntry::Value(existing)) => existing != manifest_path_str,
            Ok(RegistryHostEntry::Absent | RegistryHostEntry::WithoutStringValue) => true,
            Err(error) => {
                tracing::warn!(?browser, %error, "could not read native host registry key");
                return;
            }
        };
        if needs_registry_write
            && let Err(error) = self
                .registry
                .write_default_value(browser, manifest_path_str)
        {
            tracing::warn!(?browser, %error, "could not write native host registry key");
        }
    }

    fn evaluate_registration(
        &self,
        browser: BrowserKind,
        expected_host_exe: &str,
    ) -> Result<BrowserHostRegistration, ManageBrowserHostsError> {
        let manifest_path = self.manifest_path(browser);
        let expected_manifest = path_to_utf8(&manifest_path, || {
            ManageBrowserHostsError::EncodeManifestPath
        })?;

        let entry = match self.registry.read_default_value(browser) {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(
                    ?browser,
                    %error,
                    "native host registry key could not be read; reporting needs_repair"
                );
                return Ok(BrowserHostRegistration::NeedsRepair);
            }
        };

        match entry {
            RegistryHostEntry::Absent | RegistryHostEntry::WithoutStringValue => {
                Ok(BrowserHostRegistration::NeedsRepair)
            }
            RegistryHostEntry::Value(registered_path) => {
                if registered_path != expected_manifest {
                    return Ok(BrowserHostRegistration::NeedsRepair);
                }
                let raw_manifest = match self.fs.read_to_string(&manifest_path) {
                    Ok(Some(contents)) => contents,
                    Ok(None) => return Ok(BrowserHostRegistration::NeedsRepair),
                    Err(error) => {
                        tracing::warn!(
                            ?browser,
                            %error,
                            "native host manifest could not be read; reporting needs_repair"
                        );
                        return Ok(BrowserHostRegistration::NeedsRepair);
                    }
                };
                if manifest_matches(browser, &raw_manifest, expected_host_exe) {
                    Ok(BrowserHostRegistration::Registered)
                } else {
                    Ok(BrowserHostRegistration::NeedsRepair)
                }
            }
        }
    }

    fn collect_status(
        &self,
        streams: &[BrowserStream],
    ) -> Result<BrowserHostsStatusResponse, ManageBrowserHostsError> {
        let host_exe = (self.resolve_host_exe)()?;
        let expected_host_exe =
            path_to_utf8(&host_exe, || ManageBrowserHostsError::EncodeHostPath)?;
        let host_presence = self.fs.file_presence(&host_exe);
        let mut hosts = Vec::with_capacity(ALL_BROWSERS.len());
        for browser in ALL_BROWSERS {
            let registration = match host_presence {
                FilePresence::Absent => BrowserHostRegistration::HostMissing,
                FilePresence::Present => self.evaluate_registration(browser, expected_host_exe)?,
            };
            let connected = if streams.iter().any(|stream| stream.browser == browser) {
                BrowserHostConnection::Connected
            } else {
                BrowserHostConnection::Disconnected
            };
            hosts.push(BrowserHostEntry {
                browser,
                registration,
                connected,
            });
        }
        Ok(BrowserHostsStatusResponse { hosts })
    }
}

impl<R: BrowserHostRegistry, F: BrowserHostFs> BrowserHostsPort for NativeHostManager<R, F> {
    fn register_all(&self) {
        let _guard = self
            .serialize_ops
            .lock()
            .expect("browser host manager lock");
        let host_exe = match (self.resolve_host_exe)() {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "could not resolve quartz-host.exe path; skipping browser host registration"
                );
                return;
            }
        };
        let host_exe_str = match path_to_utf8(&host_exe, || ManageBrowserHostsError::EncodeHostPath)
        {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "quartz-host.exe path is not valid UTF-8; skipping browser host registration"
                );
                return;
            }
        };
        if self.fs.file_presence(&host_exe) == FilePresence::Absent {
            tracing::warn!(
                path = %host_exe.display(),
                "quartz-host.exe not found beside quartz.exe; skipping automatic browser host registration"
            );
            return;
        }

        for browser in ALL_BROWSERS {
            self.register_browser(browser, host_exe_str);
        }
    }

    fn status(
        &self,
        streams: &[BrowserStream],
    ) -> Result<BrowserHostsStatusResponse, ManageBrowserHostsError> {
        let _guard = self
            .serialize_ops
            .lock()
            .expect("browser host manager lock");
        self.collect_status(streams)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    /// Fault mode for the in-memory browser host registry fake.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum FakeBrowserHostRegistryFault {
        #[default]
        None,
        FailRead,
        FailWrite,
    }

    #[derive(Debug, Default)]
    struct FakeRegistryState {
        entries: BTreeMap<BrowserKind, RegistryHostEntry>,
        faults: BTreeMap<BrowserKind, FakeBrowserHostRegistryFault>,
        write_count: usize,
    }

    /// In-memory `BrowserHostRegistry` fake so tests never touch HKCU.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct FakeBrowserHostRegistry {
        state: Arc<Mutex<FakeRegistryState>>,
    }

    impl FakeBrowserHostRegistry {
        pub(crate) fn set_entry(&self, browser: BrowserKind, entry: RegistryHostEntry) {
            let mut state = self.state.lock().expect("fake host registry lock");
            if entry == RegistryHostEntry::Absent {
                state.entries.remove(&browser);
            } else {
                state.entries.insert(browser, entry);
            }
        }

        pub(crate) fn entry(&self, browser: BrowserKind) -> RegistryHostEntry {
            self.state
                .lock()
                .expect("fake host registry lock")
                .entries
                .get(&browser)
                .cloned()
                .unwrap_or(RegistryHostEntry::Absent)
        }

        pub(crate) fn set_browser_fault(
            &self,
            browser: BrowserKind,
            fault: FakeBrowserHostRegistryFault,
        ) {
            let mut state = self.state.lock().expect("fake host registry lock");
            if fault == FakeBrowserHostRegistryFault::None {
                state.faults.remove(&browser);
            } else {
                state.faults.insert(browser, fault);
            }
        }

        pub(crate) fn write_count(&self) -> usize {
            self.state
                .lock()
                .expect("fake host registry lock")
                .write_count
        }
    }

    impl BrowserHostRegistry for FakeBrowserHostRegistry {
        fn read_default_value(
            &self,
            browser: BrowserKind,
        ) -> Result<RegistryHostEntry, ManageBrowserHostsError> {
            let state = self.state.lock().expect("fake host registry lock");
            if state.faults.get(&browser).copied() == Some(FakeBrowserHostRegistryFault::FailRead) {
                return Err(ManageBrowserHostsError::ReadHostRegistry {
                    browser,
                    source: Box::new(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "simulated registry read failure",
                    )),
                });
            }
            Ok(state
                .entries
                .get(&browser)
                .cloned()
                .unwrap_or(RegistryHostEntry::Absent))
        }

        fn write_default_value(
            &self,
            browser: BrowserKind,
            manifest_path: &str,
        ) -> Result<(), ManageBrowserHostsError> {
            let mut state = self.state.lock().expect("fake host registry lock");
            if state.faults.get(&browser).copied() == Some(FakeBrowserHostRegistryFault::FailWrite)
            {
                return Err(ManageBrowserHostsError::WriteHostRegistry {
                    browser,
                    source: Box::new(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "simulated registry write failure",
                    )),
                });
            }
            state.write_count += 1;
            state
                .entries
                .insert(browser, RegistryHostEntry::Value(manifest_path.to_owned()));
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FakeFsState {
        files: BTreeMap<PathBuf, String>,
        unreadable: BTreeSet<PathBuf>,
        write_count: usize,
    }

    /// In-memory `BrowserHostFs` fake so tests never touch `%LOCALAPPDATA%\Quartz`.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct FakeBrowserHostFs {
        state: Arc<Mutex<FakeFsState>>,
    }

    impl FakeBrowserHostFs {
        pub(crate) fn insert_file(&self, path: impl Into<PathBuf>, contents: impl Into<String>) {
            self.state
                .lock()
                .expect("fake host fs lock")
                .files
                .insert(path.into(), contents.into());
        }

        pub(crate) fn remove_file(&self, path: &Path) {
            self.state
                .lock()
                .expect("fake host fs lock")
                .files
                .remove(path);
        }

        pub(crate) fn mark_unreadable(&self, path: impl Into<PathBuf>) {
            self.state
                .lock()
                .expect("fake host fs lock")
                .unreadable
                .insert(path.into());
        }

        pub(crate) fn read_file(&self, path: &Path) -> Option<String> {
            self.state
                .lock()
                .expect("fake host fs lock")
                .files
                .get(path)
                .cloned()
        }

        pub(crate) fn exists(&self, path: &Path) -> bool {
            self.state
                .lock()
                .expect("fake host fs lock")
                .files
                .contains_key(path)
        }

        pub(crate) fn write_count(&self) -> usize {
            self.state.lock().expect("fake host fs lock").write_count
        }
    }

    impl BrowserHostFs for FakeBrowserHostFs {
        fn file_presence(&self, path: &Path) -> FilePresence {
            if self.exists(path) {
                FilePresence::Present
            } else {
                FilePresence::Absent
            }
        }

        fn read_to_string(&self, path: &Path) -> Result<Option<String>, ManageBrowserHostsError> {
            let state = self.state.lock().expect("fake host fs lock");
            if state.unreadable.contains(path) {
                return Err(ManageBrowserHostsError::ReadManifestFile {
                    path: path.to_path_buf(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "simulated unreadable manifest",
                    ),
                });
            }
            Ok(state.files.get(path).cloned())
        }

        fn write_atomic(&self, path: &Path, contents: &str) -> Result<(), ManageBrowserHostsError> {
            let mut state = self.state.lock().expect("fake host fs lock");
            state.write_count += 1;
            state.files.insert(path.to_path_buf(), contents.to_owned());
            Ok(())
        }
    }

    /// Test fixture holding an in-memory `BrowserHostsPort` and its inspectable fakes.
    #[derive(Clone)]
    pub(crate) struct FakeBrowserHosts {
        pub(crate) port: Arc<dyn BrowserHostsPort>,
        pub(crate) registry: FakeBrowserHostRegistry,
        pub(crate) fs: FakeBrowserHostFs,
        pub(crate) manifests_dir: PathBuf,
        host_exe: Arc<Mutex<PathBuf>>,
    }

    impl FakeBrowserHosts {
        pub(crate) fn new() -> Self {
            let manifests_dir = PathBuf::from(r"C:\Users\test\AppData\Local\Quartz\native-hosts");
            let initial_host_exe = PathBuf::from(r"C:\Program Files\Quartz\quartz-host.exe");
            let registry = FakeBrowserHostRegistry::default();
            let fs = FakeBrowserHostFs::default();
            fs.insert_file(&initial_host_exe, "host-binary-placeholder");
            let host_exe = Arc::new(Mutex::new(initial_host_exe));
            let host_exe_for_closure = Arc::clone(&host_exe);
            let port: Arc<dyn BrowserHostsPort> = Arc::new(NativeHostManager::new(
                registry.clone(),
                fs.clone(),
                manifests_dir.clone(),
                move || Ok(host_exe_for_closure.lock().expect("host exe lock").clone()),
            ));
            Self {
                port,
                registry,
                fs,
                manifests_dir,
                host_exe,
            }
        }

        pub(crate) fn set_host_exe(&self, path: impl Into<PathBuf>) {
            *self.host_exe.lock().expect("host exe lock") = path.into();
        }

        pub(crate) fn host_exe(&self) -> PathBuf {
            self.host_exe.lock().expect("host exe lock").clone()
        }
    }

    /// Builds a ready-to-use in-memory `BrowserHostsPort` for IPC and runtime tests.
    pub(crate) fn in_memory_browser_hosts() -> Arc<dyn BrowserHostsPort> {
        FakeBrowserHosts::new().port
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::*, *};
    use quartz_protocol::BrowserStreamId;

    fn entry(response: &BrowserHostsStatusResponse, browser: BrowserKind) -> BrowserHostEntry {
        response
            .hosts
            .iter()
            .copied()
            .find(|host| host.browser == browser)
            .expect("browser entry present")
    }

    #[test]
    fn auto_registration_writes_manifests_and_keys_for_all_three_browsers() {
        let fixture = FakeBrowserHosts::new();
        fixture.port.register_all();

        let chromium_manifest = fixture.manifests_dir.join(CHROMIUM_MANIFEST_FILE);
        let firefox_manifest = fixture.manifests_dir.join(FIREFOX_MANIFEST_FILE);
        assert_eq!(
            fixture.registry.entry(BrowserKind::Chrome),
            RegistryHostEntry::Value(chromium_manifest.to_str().expect("utf-8").to_owned())
        );
        assert_eq!(
            fixture.registry.entry(BrowserKind::Edge),
            RegistryHostEntry::Value(chromium_manifest.to_str().expect("utf-8").to_owned())
        );
        assert_eq!(
            fixture.registry.entry(BrowserKind::Firefox),
            RegistryHostEntry::Value(firefox_manifest.to_str().expect("utf-8").to_owned())
        );

        let chrome_raw = fixture
            .fs
            .read_file(&chromium_manifest)
            .expect("chrome manifest written");
        let chrome_json: serde_json::Value =
            serde_json::from_str(&chrome_raw).expect("valid chrome manifest json");
        assert_eq!(
            chrome_json,
            serde_json::json!({
                "name": "dev.4o3f.quartz",
                "description": "Quartz browser activity host",
                "path": r"C:\Program Files\Quartz\quartz-host.exe",
                "type": "stdio",
                "allowed_origins": [
                    format!("chrome-extension://{CHROMIUM_EXTENSION_ID}/")
                ]
            })
        );

        let firefox_raw = fixture
            .fs
            .read_file(&firefox_manifest)
            .expect("firefox manifest written");
        let firefox_json: serde_json::Value =
            serde_json::from_str(&firefox_raw).expect("valid firefox manifest json");
        assert_eq!(
            firefox_json,
            serde_json::json!({
                "name": "dev.4o3f.quartz",
                "description": "Quartz browser activity host",
                "path": r"C:\Program Files\Quartz\quartz-host.exe",
                "type": "stdio",
                "allowed_extensions": [FIREFOX_EXTENSION_ID]
            })
        );

        let status = fixture.port.status(&[]).expect("status after registration");
        assert_eq!(
            status
                .hosts
                .iter()
                .map(|host| (host.browser, host.registration))
                .collect::<Vec<_>>(),
            vec![
                (BrowserKind::Chrome, BrowserHostRegistration::Registered),
                (BrowserKind::Edge, BrowserHostRegistration::Registered),
                (BrowserKind::Firefox, BrowserHostRegistration::Registered),
            ]
        );
    }

    #[test]
    fn second_registration_run_with_identical_content_performs_no_writes() {
        let fixture = FakeBrowserHosts::new();
        fixture.port.register_all();
        // Chrome and Edge share one manifest file, so initial registration writes 2 files + 3 keys.
        assert_eq!(fixture.fs.write_count(), 2);
        assert_eq!(fixture.registry.write_count(), 3);

        fixture.port.register_all();
        assert_eq!(fixture.fs.write_count(), 2);
        assert_eq!(fixture.registry.write_count(), 3);
    }

    #[test]
    fn moved_host_path_rewrites_manifests_and_restores_registered_status() {
        let fixture = FakeBrowserHosts::new();
        fixture.port.register_all();
        let initial_fs_writes = fixture.fs.write_count();
        let initial_registry_writes = fixture.registry.write_count();

        // Executable moved -> manifest path mismatches current host path -> needs_repair.
        let moved_host = PathBuf::from(r"D:\Tools\Quartz\quartz-host.exe");
        fixture.fs.insert_file(&moved_host, "moved-host");
        fixture.set_host_exe(&moved_host);
        let moved = fixture.port.status(&[]).expect("moved status");
        for browser in ALL_BROWSERS {
            assert_eq!(
                entry(&moved, browser).registration,
                BrowserHostRegistration::NeedsRepair
            );
        }

        // Next startup rewrites the two manifests in place without rewriting unchanged HKCU keys.
        fixture.port.register_all();
        assert_eq!(fixture.fs.write_count(), initial_fs_writes + 2);
        assert_eq!(fixture.registry.write_count(), initial_registry_writes);
        let repaired = fixture.port.status(&[]).expect("repaired status");
        for browser in ALL_BROWSERS {
            assert_eq!(
                entry(&repaired, browser).registration,
                BrowserHostRegistration::Registered
            );
        }

        // A stale registry key path is also rewritten on startup.
        fixture.registry.set_entry(
            BrowserKind::Edge,
            RegistryHostEntry::Value(r"C:\Old\dev.4o3f.quartz.chrome.json".to_owned()),
        );
        fixture.port.register_all();
        assert_eq!(fixture.fs.write_count(), initial_fs_writes + 2);
        assert_eq!(fixture.registry.write_count(), initial_registry_writes + 1);
        let edge_repaired = fixture.port.status(&[]).expect("edge repaired status");
        assert_eq!(
            entry(&edge_repaired, BrowserKind::Edge).registration,
            BrowserHostRegistration::Registered
        );
    }

    #[test]
    fn missing_host_executable_skips_registration_without_writes_and_reports_host_missing() {
        let fixture = FakeBrowserHosts::new();
        let host_exe = fixture.host_exe();
        fixture.fs.remove_file(&host_exe);

        fixture.port.register_all();
        assert_eq!(fixture.fs.write_count(), 0);
        assert_eq!(fixture.registry.write_count(), 0);
        for browser in ALL_BROWSERS {
            assert_eq!(fixture.registry.entry(browser), RegistryHostEntry::Absent);
        }
        assert!(
            !fixture
                .fs
                .exists(&fixture.manifests_dir.join(CHROMIUM_MANIFEST_FILE))
        );
        assert!(
            !fixture
                .fs
                .exists(&fixture.manifests_dir.join(FIREFOX_MANIFEST_FILE))
        );

        let status = fixture.port.status(&[]).expect("status with missing host");
        for browser in ALL_BROWSERS {
            assert_eq!(
                entry(&status, browser).registration,
                BrowserHostRegistration::HostMissing
            );
        }
    }

    #[test]
    fn one_browsers_registry_failure_does_not_stop_other_browsers() {
        let fixture = FakeBrowserHosts::new();
        fixture
            .registry
            .set_browser_fault(BrowserKind::Edge, FakeBrowserHostRegistryFault::FailWrite);

        fixture.port.register_all();

        let status = fixture
            .port
            .status(&[])
            .expect("status after partial failure");
        assert_eq!(
            entry(&status, BrowserKind::Chrome).registration,
            BrowserHostRegistration::Registered
        );
        assert_eq!(
            entry(&status, BrowserKind::Edge).registration,
            BrowserHostRegistration::NeedsRepair
        );
        assert_eq!(
            entry(&status, BrowserKind::Firefox).registration,
            BrowserHostRegistration::Registered
        );

        // Read failure on one browser reports needs_repair for that browser without breaking others.
        fixture
            .registry
            .set_browser_fault(BrowserKind::Edge, FakeBrowserHostRegistryFault::FailRead);
        let read_fault_status = fixture
            .port
            .status(&[])
            .expect("status with one browser read failure");
        assert_eq!(
            entry(&read_fault_status, BrowserKind::Chrome).registration,
            BrowserHostRegistration::Registered
        );
        assert_eq!(
            entry(&read_fault_status, BrowserKind::Edge).registration,
            BrowserHostRegistration::NeedsRepair
        );
        assert_eq!(
            entry(&read_fault_status, BrowserKind::Firefox).registration,
            BrowserHostRegistration::Registered
        );
    }

    #[test]
    fn status_matrix_reports_needs_repair_registered_and_connected_states() {
        let fixture = FakeBrowserHosts::new();

        // Initially unregistered -> needs_repair in fixed Chrome, Edge, Firefox order.
        let initial = fixture.port.status(&[]).expect("initial status");
        assert_eq!(
            initial
                .hosts
                .iter()
                .map(|host| (host.browser, host.registration))
                .collect::<Vec<_>>(),
            vec![
                (BrowserKind::Chrome, BrowserHostRegistration::NeedsRepair),
                (BrowserKind::Edge, BrowserHostRegistration::NeedsRepair),
                (BrowserKind::Firefox, BrowserHostRegistration::NeedsRepair),
            ]
        );

        fixture.port.register_all();

        // Deleting manifest while key stays -> needs_repair for Chrome and Edge; Firefox stays registered.
        let chrome_manifest = fixture.manifests_dir.join(CHROMIUM_MANIFEST_FILE);
        fixture.fs.remove_file(&chrome_manifest);
        let missing_manifest = fixture.port.status(&[]).expect("missing manifest status");
        assert_eq!(
            entry(&missing_manifest, BrowserKind::Chrome).registration,
            BrowserHostRegistration::NeedsRepair
        );
        assert_eq!(
            entry(&missing_manifest, BrowserKind::Edge).registration,
            BrowserHostRegistration::NeedsRepair
        );
        assert_eq!(
            entry(&missing_manifest, BrowserKind::Firefox).registration,
            BrowserHostRegistration::Registered
        );

        // Unreadable manifest -> needs_repair.
        fixture.port.register_all();
        fixture.fs.mark_unreadable(&chrome_manifest);
        let unreadable = fixture
            .port
            .status(&[])
            .expect("unreadable manifest status");
        assert_eq!(
            entry(&unreadable, BrowserKind::Chrome).registration,
            BrowserHostRegistration::NeedsRepair
        );

        // Non-string default registry value -> needs_repair.
        fixture
            .registry
            .set_entry(BrowserKind::Firefox, RegistryHostEntry::WithoutStringValue);
        let streams = vec![BrowserStream {
            browser_stream: BrowserStreamId("a286737e-3855-43cb-98ee-c366344372d0".into()),
            browser: BrowserKind::Chrome,
        }];
        let mismatched = fixture.port.status(&streams).expect("status with streams");
        assert_eq!(
            entry(&mismatched, BrowserKind::Firefox).registration,
            BrowserHostRegistration::NeedsRepair
        );
        assert_eq!(
            entry(&mismatched, BrowserKind::Chrome).connected,
            BrowserHostConnection::Connected
        );
        assert_eq!(
            entry(&mismatched, BrowserKind::Edge).connected,
            BrowserHostConnection::Disconnected
        );
        assert_eq!(
            entry(&mismatched, BrowserKind::Firefox).connected,
            BrowserHostConnection::Disconnected
        );
    }

    #[test]
    fn std_filesystem_auto_registration_operates_idempotently_inside_temp_dir() {
        let temp = tempfile::tempdir().expect("isolated temp dir");
        let manifests_dir = temp.path().join("native-hosts");
        let host_exe = temp.path().join(HOST_EXECUTABLE_NAME);
        fs::write(&host_exe, b"stub").expect("create stub host exe");
        let registry = FakeBrowserHostRegistry::default();
        let host_for_closure = host_exe.clone();
        let manager = NativeHostManager::new(
            registry.clone(),
            StdBrowserHostFs,
            manifests_dir.clone(),
            move || Ok(host_for_closure.clone()),
        );

        manager.register_all();
        let status = manager.status(&[]).expect("status after temp dir register");
        for browser in ALL_BROWSERS {
            assert_eq!(
                entry(&status, browser).registration,
                BrowserHostRegistration::Registered
            );
        }
        assert!(manifests_dir.join(CHROMIUM_MANIFEST_FILE).is_file());
        assert!(manifests_dir.join(FIREFOX_MANIFEST_FILE).is_file());
        assert_eq!(registry.write_count(), 3);

        manager.register_all();
        assert_eq!(registry.write_count(), 3);
    }

    #[test]
    fn registry_subkeys_match_contract() {
        assert_eq!(
            registry_subkey(BrowserKind::Chrome),
            r"Software\Google\Chrome\NativeMessagingHosts\dev.4o3f.quartz"
        );
        assert_eq!(
            registry_subkey(BrowserKind::Edge),
            r"Software\Microsoft\Edge\NativeMessagingHosts\dev.4o3f.quartz"
        );
        assert_eq!(
            registry_subkey(BrowserKind::Firefox),
            r"Software\Mozilla\NativeMessagingHosts\dev.4o3f.quartz"
        );
    }
}
