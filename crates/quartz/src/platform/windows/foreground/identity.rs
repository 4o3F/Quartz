//! Slow identity and privacy queries, called exclusively by the capture worker.

use super::frame_aumid::{AUMID_BUFFER_UNITS, Aumid, DecodeAumidError, HostedIdentity};
use super::{Window, version};
use crate::core::{
    app::ObservedEvent,
    foreground::{self, BoundedCache, CacheRetention, Desktop, ProcessKey, Snapshot},
};
use quartz_domain::capture::{
    AppIdentity, BrowserContext, CaptureEvent, Foreground, MetadataSource, NoForegroundReason,
    ObservationSource, PrivacyContext, ProcessId, SystemSignal, UtcInstant, WindowFacts,
};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows::{
    Win32::{
        Foundation::{
            self, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, FILETIME, HANDLE, STILL_ACTIVE,
        },
        Storage::Packaging::Appx,
        System::Threading::{self, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION},
    },
    core::PWSTR,
};

/// Maximum extended Windows path buffer including terminator, capture 02 §2.1.
const IMAGE_PATH_UNITS: usize = 32_768;
/// Host executable whose content app is identified through a child CoreWindow (02 §2.1).
const APPLICATION_FRAME_HOST_EXE: &str = "applicationframehost.exe";
/// FILETIME units and Unix epoch offset, capture 02 §2.1 creation-time identity.
const TICKS_PER_MILLISECOND: u64 = 10_000;
const UNIX_EPOCH_TICKS: i128 = 116_444_736_000_000_000;

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
enum ResolveProcessError {
    #[snafu(display("could not open process for limited metadata query"))]
    Open { source: windows::core::Error },
    #[snafu(display("could not read process creation time"))]
    Times { source: windows::core::Error },
    #[snafu(display("could not read process image path"))]
    Image { source: windows::core::Error },
    #[snafu(display("process metadata contains invalid UTF-16"))]
    DecodeText { source: std::string::FromUtf16Error },
    #[snafu(display("could not query application user model identity ({code})"))]
    Aumid { code: u32 },
    #[snafu(display("could not decode process application user model identity"))]
    DecodeAumid { source: DecodeAumidError },
    #[snafu(display("process creation time exceeds the supported calendar"))]
    CreationTime,
}
#[derive(Debug)]
enum ResolveProcessErrorKind {
    Open,
    Times,
    Image,
    DecodeText,
    Aumid,
    DecodeAumid,
    CreationTime,
}
impl ResolveProcessError {
    fn kind(&self) -> ResolveProcessErrorKind {
        match self {
            Self::Open { .. } => ResolveProcessErrorKind::Open,
            Self::Times { .. } => ResolveProcessErrorKind::Times,
            Self::Image { .. } => ResolveProcessErrorKind::Image,
            Self::DecodeText { .. } => ResolveProcessErrorKind::DecodeText,
            Self::Aumid { .. } => ResolveProcessErrorKind::Aumid,
            Self::DecodeAumid { .. } => ResolveProcessErrorKind::DecodeAumid,
            Self::CreationTime => ResolveProcessErrorKind::CreationTime,
        }
    }
}
#[derive(Clone)]
struct Metadata {
    identity: AppIdentity,
    exe_name: String,
    display_name: String,
    created: UtcInstant,
}
struct ApplicationMetadata {
    identity: AppIdentity,
    source: MetadataSource,
    exe_name: Option<String>,
    display_name: String,
    browser: BrowserContext,
}
#[derive(Debug)]
enum FrameFallbackReason {
    ContentWindowUnavailable,
    ContentProcessUnavailable,
}
impl Metadata {
    fn into_application(self) -> ApplicationMetadata {
        let browser = match self.exe_name.as_str() {
            "msedge.exe" | "chrome.exe" | "firefox.exe" | "brave.exe" | "vivaldi.exe"
            | "opera.exe" => BrowserContext::Offline,
            _ => BrowserContext::NotBrowser,
        };
        ApplicationMetadata {
            identity: self.identity,
            source: MetadataSource::Process,
            exe_name: Some(self.exe_name),
            display_name: self.display_name,
            browser,
        }
    }
}
impl ApplicationMetadata {
    fn from_frame(aumid: Aumid, display_name: String) -> Self {
        Self {
            identity: AppIdentity::Aumid(aumid.into_string()),
            source: MetadataSource::Frame,
            exe_name: None,
            display_name,
            browser: BrowserContext::NotBrowser,
        }
    }
    fn unresolved(class: &str) -> Self {
        Self {
            identity: AppIdentity::Unresolved(class.to_owned()),
            source: MetadataSource::Unavailable,
            exe_name: None,
            display_name: class.to_owned(),
            browser: PrivacyContext::default().browser,
        }
    }
}
struct ProcessMetadata {
    handle: ProcessHandle,
    metadata: Metadata,
}
#[derive(Debug, snafu::Snafu)]
#[snafu(display("could not query cached process state"))]
struct ReadProcessStateError {
    source: windows::core::Error,
}
#[derive(Debug, Clone, Copy)]
enum ProcessState {
    Running,
    Exited,
}
impl ProcessMetadata {
    fn state(&self) -> Result<ProcessState, ReadProcessStateError> {
        let mut code = 0;
        // SAFETY: A cached limited-query handle stays valid until the entry is evicted.
        unsafe { Threading::GetExitCodeProcess(self.handle.raw(), &mut code) }
            .map_err(|source| ReadProcessStateError { source })?;
        if code == STILL_ACTIVE.0.cast_unsigned() {
            Ok(ProcessState::Running)
        } else {
            Ok(ProcessState::Exited)
        }
    }
    fn retention(&self) -> CacheRetention {
        match self.state() {
            Ok(ProcessState::Running) => CacheRetention::Keep,
            Ok(ProcessState::Exited) => CacheRetention::Evict,
            Err(error) => {
                tracing::debug!(
                    code = error.source.code().0,
                    "cached process state unavailable; evicting metadata"
                );
                CacheRetention::Evict
            }
        }
    }
}
#[derive(Clone)]
enum DisplayName {
    Frame(String),
    Process(String),
}
impl DisplayName {
    fn text(&self) -> &str {
        match self {
            Self::Frame(name) | Self::Process(name) => name,
        }
    }
}
pub(crate) struct Resolver {
    processes: BoundedCache<ProcessKey, ProcessMetadata>,
    names: BoundedCache<String, DisplayName>,
}
pub(crate) use crate::core::foreground::Resolution;
impl Resolver {
    pub(crate) fn new() -> Self {
        Self {
            processes: BoundedCache::new(foreground::PROCESS_CACHE_CAPACITY),
            names: BoundedCache::new(foreground::DISPLAY_CACHE_CAPACITY),
        }
    }
    pub(crate) fn resolve(&mut self, snapshot: Snapshot) -> Resolution {
        // Native event time remains a wrapping OS tick; the sampled clock supplies core ordering.
        let Snapshot {
            at,
            read_at: _read_at,
            event_time_ms: _event_time_ms,
            source,
            kind: _kind,
            window,
            title,
            desktop,
        } = snapshot;
        if source == ObservationSource::Review {
            self.processes.retain(|_, entry| entry.retention());
        }
        let foreground = if matches!(desktop, Desktop::Unknown) {
            Foreground::None(NoForegroundReason::UnknownDesktop)
        } else if window.get() == 0 {
            Foreground::None(NoForegroundReason::Empty)
        } else {
            let native = Window::from_handle(window);
            let owner = native.process();
            let class = match native.class() {
                Ok(class) => class,
                Err(error) => {
                    if error.log_level() == tracing::Level::DEBUG {
                        tracing::debug!(kind = ?error.kind(), "window class unavailable; requesting foreground review");
                    } else {
                        tracing::warn!(kind = ?error.kind(), "window class unavailable; requesting foreground review");
                    }

                    self.processes.evict_process(owner);
                    return Resolution::Review;
                }
            };
            let root = self.process(owner);
            let metadata = match root.as_ref() {
                Some(root) if root.exe_name == APPLICATION_FRAME_HOST_EXE => {
                    let children = match native.children() {
                        Ok(children) => children,
                        Err(error) => {
                            if error.log_level() == tracing::Level::DEBUG {
                                tracing::debug!(kind = ?error.kind(), "child window class unavailable; requesting foreground review");
                            } else {
                                tracing::warn!(kind = ?error.kind(), "child window class unavailable; requesting foreground review");
                            }

                            self.processes.evict_process(owner);
                            return Resolution::Review;
                        }
                    };
                    let content = foreground::ChildWindow::content(&children, owner);
                    let content_metadata = match content {
                        Some(child) => {
                            let metadata = self.process(child.process);
                            if Window::from_handle(child.window).process() != child.process {
                                self.processes.evict_process(child.process);
                                return Resolution::Review;
                            }
                            metadata
                        }
                        None => {
                            tracing::debug!(
                                child_count = children.len(),
                                "application frame content window unavailable"
                            );
                            None
                        }
                    };
                    let frame = if content_metadata.is_some() {
                        Ok(None)
                    } else {
                        let reason = match content {
                            Some(_) => FrameFallbackReason::ContentProcessUnavailable,
                            None => FrameFallbackReason::ContentWindowUnavailable,
                        };
                        tracing::debug!(?reason, "application frame identity fallback");
                        native.frame_aumid()
                    };
                    match HostedIdentity::from_evidence(content_metadata, frame) {
                        HostedIdentity::Content(metadata) => metadata.into_application(),
                        HostedIdentity::Frame(aumid) => self.frame_application(aumid),
                        HostedIdentity::Unresolved(reason) => {
                            tracing::debug!(?reason, "application frame AUMID unavailable");
                            ApplicationMetadata::unresolved(&class)
                        }
                    }
                }
                Some(_) | None => root
                    .clone()
                    .map(Metadata::into_application)
                    .unwrap_or_else(|| ApplicationMetadata::unresolved(&class)),
            };
            if native.process() != owner || owner.get() == 0 {
                self.processes.evict_process(owner);
                return Resolution::Review;
            }
            let created = root.as_ref().map(|root| root.created);
            let privacy = PrivacyContext {
                capture_excluded: native.display_affinity().signal(),
                private_window: SystemSignal::Unknown,
                browser: metadata.browser,
                ..PrivacyContext::default()
            };
            Foreground::Window {
                facts: Box::new(WindowFacts {
                    handle: window,
                    process: owner,
                    process_created_utc: created,
                    class,
                    app: metadata.identity,
                    metadata_source: metadata.source,
                    exe_name: metadata.exe_name,
                    display_name: metadata.display_name,
                    title,
                }),
                privacy,
            }
        };
        Resolution::Event(ObservedEvent {
            at,
            event: CaptureEvent::Foreground { source, foreground },
        })
    }
    fn frame_application(&mut self, aumid: Aumid) -> ApplicationMetadata {
        self.frame_application_with(aumid, Aumid::display_name)
    }
    fn frame_application_with(
        &mut self,
        aumid: Aumid,
        shell: impl FnOnce(
            &Aumid,
        ) -> Result<
            super::frame_aumid::AppDisplayName,
            super::frame_aumid::ReadAppDisplayNameError,
        >,
    ) -> ApplicationMetadata {
        let key = AppIdentity::Aumid(aumid.as_str().to_owned()).key();
        use super::frame_aumid::{FrameDisplayName, FrameDisplayNameSource};
        let selected =
            FrameDisplayName::select(self.names.find(&key).map(DisplayName::text), || {
                shell(&aumid)
            });
        let source = selected.source();
        tracing::debug!(name_source = ?source, "application identity resolved from frame");
        let display_name = selected.into_name(&aumid);
        if source == FrameDisplayNameSource::AppsFolder {
            self.names
                .insert(key, DisplayName::Frame(display_name.clone()));
        }
        ApplicationMetadata::from_frame(aumid, display_name)
    }
    fn process_display_name(&mut self, key: String, read: impl FnOnce() -> String) -> String {
        if let Some(DisplayName::Process(name)) = self.names.find(&key) {
            return name.clone();
        }
        let name = read();
        self.names.insert(key, DisplayName::Process(name.clone()));
        name
    }
    fn process(&mut self, process: ProcessId) -> Option<Metadata> {
        match self.resolve_process(process) {
            Ok(metadata) => Some(metadata),
            Err(error) => {
                tracing::debug!(kind = ?error.kind(), "process identity unresolved");

                self.processes.evict_process(process);
                None
            }
        }
    }
    fn resolve_process(&mut self, process: ProcessId) -> Result<Metadata, ResolveProcessError> {
        let handle = ProcessHandle::open(process)?;
        let key = ProcessKey {
            process,
            created: handle.creation_ticks()?,
        };
        self.processes.retain_lifetime(key);
        if let Some(cached) = self.processes.find(&key) {
            match cached.retention() {
                CacheRetention::Keep => return Ok(cached.metadata.clone()),
                CacheRetention::Evict => self.processes.evict_process(process),
            }
        }
        let mut path = vec![0; IMAGE_PATH_UNITS];
        let mut length = u32::try_from(path.len()).expect("path cap fits u32");
        // SAFETY: The output buffer and its input/output length remain live for this call.
        unsafe {
            Threading::QueryFullProcessImageNameW(
                handle.raw(),
                PROCESS_NAME_WIN32,
                PWSTR(path.as_mut_ptr()),
                &mut length,
            )
        }
        .map_err(|source| ResolveProcessError::Image { source })?;
        let image_path =
            String::from_utf16(&path[..usize::try_from(length).expect("path length fits usize")])
                .map_err(|source| ResolveProcessError::DecodeText { source })?;
        let path = image_path.replace('/', "\\").to_lowercase();
        let identity =
            AppIdentity::from_process(&path, handle.application_user_model_id()?.as_deref());
        let exe_name = path
            .rsplit('\\')
            .next()
            .ok_or(ResolveProcessError::CreationTime)?
            .to_owned();
        let app_key = identity.key();
        let display_name = self.process_display_name(app_key, || {
            match version::FileDescription::read(&image_path) {
                Ok(Some(description)) => description.into_string(),
                Ok(None) => exe_name.clone(),
                Err(error) => {
                    tracing::warn!(kind = ?error.kind(), "application description unavailable; using executable filename");
                    exe_name.clone()
                }
            }
        });
        let millis = (i128::from(key.created) - UNIX_EPOCH_TICKS)
            .div_euclid(i128::from(TICKS_PER_MILLISECOND));
        let created = UtcInstant::try_from(
            i64::try_from(millis).map_err(|_| ResolveProcessError::CreationTime)?,
        )
        .map_err(|_| ResolveProcessError::CreationTime)?;
        let metadata = Metadata {
            identity,
            exe_name,
            display_name,
            created,
        };
        self.processes.insert(
            key,
            ProcessMetadata {
                handle,
                metadata: metadata.clone(),
            },
        );
        Ok(metadata)
    }
}
struct ProcessHandle(OwnedHandle);
impl ProcessHandle {
    fn open(process: ProcessId) -> Result<Self, ResolveProcessError> {
        // SAFETY: Only limited metadata access is requested; ownership is transferred immediately.
        let handle = unsafe {
            Threading::OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process.get())
        }
        .map_err(|source| ResolveProcessError::Open { source })?;
        // SAFETY: OpenProcess returned this uniquely owned, CloseHandle-compatible kernel handle.
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(handle.0) }))
    }
    fn raw(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }
    fn creation_ticks(&self) -> Result<u64, ResolveProcessError> {
        let mut created = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: All four outputs are writable and the process handle is live.
        unsafe {
            Threading::GetProcessTimes(self.raw(), &mut created, &mut exit, &mut kernel, &mut user)
        }
        .map_err(|source| ResolveProcessError::Times { source })?;
        Ok((u64::from(created.dwHighDateTime) << u32::BITS) | u64::from(created.dwLowDateTime))
    }
    fn application_user_model_id(&self) -> Result<Option<String>, ResolveProcessError> {
        let mut length = 0;
        // SAFETY: Null buffer queries the required AUMID length.
        let code = unsafe { Appx::GetApplicationUserModelId(self.raw(), &mut length, None) };
        if code == Foundation::APPMODEL_ERROR_NO_APPLICATION {
            return Ok(None);
        }
        if code != ERROR_INSUFFICIENT_BUFFER
            || length == 0
            || usize::try_from(length).expect("native AUMID length fits usize") > AUMID_BUFFER_UNITS
        {
            return Err(ResolveProcessError::Aumid { code: code.0 });
        }
        let mut buffer = vec![0; usize::try_from(length).expect("AUMID length fits usize")];
        // SAFETY: The buffer holds the queried length including its terminator.
        let code = unsafe {
            Appx::GetApplicationUserModelId(
                self.raw(),
                &mut length,
                Some(PWSTR(buffer.as_mut_ptr())),
            )
        };
        if code != ERROR_SUCCESS {
            return Err(ResolveProcessError::Aumid { code: code.0 });
        }
        Aumid::from_terminated(&buffer)
            .map(|aumid| Some(aumid.into_string()))
            .map_err(|source| ResolveProcessError::DecodeAumid { source })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::foreground::EventKind;
    #[test]
    fn successful_shell_names_are_cached_and_process_names_supersede_them() {
        let aumid =
            Aumid::from_utf16(&"Package!App".encode_utf16().collect::<Vec<_>>()).expect("AUMID");
        let mut resolver = Resolver::new();
        let first = resolver.frame_application_with(aumid.clone(), |_| {
            super::super::frame_aumid::AppDisplayName::new("Shell name".into())
        });
        assert_eq!(first.display_name, "Shell name");
        let cached = resolver
            .frame_application_with(aumid.clone(), |_| panic!("Shell lookup must be cached"));
        assert_eq!(cached.display_name, "Shell name");
        let key = AppIdentity::Aumid(aumid.as_str().to_owned()).key();
        assert_eq!(
            resolver.process_display_name(key, || "Process name".into()),
            "Process name"
        );
        let upgraded = resolver.frame_application_with(aumid, |_| panic!("process name is cached"));
        assert_eq!(upgraded.display_name, "Process name");
    }

    #[test]
    fn frame_identity_reuses_cached_display_name_without_process_metadata() {
        let units: Vec<_> = "Package!App".encode_utf16().collect();
        let aumid = Aumid::from_utf16(&units).expect("valid test AUMID");
        let identity = AppIdentity::Aumid(aumid.as_str().to_owned());
        let mut resolver = Resolver::new();
        resolver
            .names
            .insert(identity.key(), DisplayName::Process("Settings".into()));
        let cached = resolver.frame_application(aumid);
        assert_eq!(cached.identity, identity);
        assert_eq!(cached.source, MetadataSource::Frame);
        assert!(cached.exe_name.is_none());
        assert_eq!(cached.display_name, "Settings");
    }
    #[test]
    fn failed_class_query_requests_review_without_rule_evaluation() {
        use quartz_domain::capture::{MonoInstant, Timestamp, Title, WindowHandle};
        let at = Timestamp {
            utc: UtcInstant::try_from(0).expect("test UTC"),
            mono: MonoInstant::from_millis(0),
        };
        let snapshot = Snapshot {
            at,
            read_at: at,
            event_time_ms: None,
            source: ObservationSource::Review,
            kind: EventKind::Foreground,
            window: WindowHandle::new(u64::try_from(usize::MAX).expect("native HWND fits u64")),
            title: Title::unreadable(),
            desktop: Desktop::Observable,
        };
        assert!(matches!(
            Resolver::new().resolve(snapshot),
            Resolution::Review
        ));
    }
    #[test]
    fn resolves_own_process_without_windows_or_shell_objects() {
        let mut resolver = Resolver::new();
        let process = ProcessId::new(std::process::id());
        let metadata = resolver
            .resolve_process(process)
            .expect("current process metadata");
        let expected = std::env::current_exe()
            .expect("test executable")
            .to_string_lossy()
            .replace('/', "\\")
            .to_lowercase();
        assert_eq!(metadata.identity, AppIdentity::Exe(expected.clone()));
        assert!(metadata.created.get().as_millisecond() > 0);
        assert_eq!(
            metadata.display_name,
            version::FileDescription::read(&expected)
                .expect("valid own-process version resource query")
                .map(version::FileDescription::into_string)
                .unwrap_or(metadata.exe_name)
        );
        assert!(!metadata.display_name.is_empty());
    }
}
