//! Slow identity and privacy queries, called exclusively by the capture worker.

use super::{Window, version};
use crate::core::{
    app::ObservedEvent,
    foreground::{self, BoundedCache, CacheRetention, Desktop, EventKind, ProcessKey, Snapshot},
};
use quartz_domain::capture::{
    AppIdentity, BrowserContext, CaptureEvent, Foreground, NoForegroundReason, ObservationSource,
    PrivacyContext, ProcessId, SystemSignal, UtcInstant, WindowFacts,
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
    #[snafu(display("application user model identity is empty or missing a terminator"))]
    InvalidAumid,
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
    InvalidAumid,
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
            Self::InvalidAumid => ResolveProcessErrorKind::InvalidAumid,
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
pub(crate) struct Resolver {
    processes: BoundedCache<ProcessKey, ProcessMetadata>,
    names: BoundedCache<String, String>,
    failures: u64,
}
pub(crate) enum Resolution {
    Event(ObservedEvent),
    Review,
}
impl Resolver {
    pub(crate) fn new() -> Self {
        Self {
            processes: BoundedCache::new(foreground::PROCESS_CACHE_CAPACITY),
            names: BoundedCache::new(foreground::DISPLAY_CACHE_CAPACITY),
            failures: 0,
        }
    }
    pub(crate) fn resolve(&mut self, snapshot: Snapshot) -> Resolution {
        // Native event time remains a wrapping OS tick; the sampled clock supplies core ordering.
        let Snapshot {
            at,
            read_at: _read_at,
            event_time_ms: _event_time_ms,
            generation,
            source,
            kind,
            window,
            title,
            desktop,
        } = snapshot;
        if kind == EventKind::Review {
            return Resolution::Review;
        }
        if source == ObservationSource::Review {
            self.processes.retain(|_, entry| entry.retention());
        }
        if kind == EventKind::Destroy {
            return Resolution::Event(ObservedEvent {
                at,
                event: CaptureEvent::DestroyWindow {
                    generation,
                    handle: window,
                },
            });
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
                    tracing::warn!(kind = ?error.kind(), "window class unavailable; requesting foreground review");
                    self.failures = self.failures.saturating_add(1);
                    self.processes.evict_process(owner);
                    return Resolution::Review;
                }
            };
            let root = self.process(owner);
            let metadata = match root.as_ref() {
                Some(root) if root.exe_name == "applicationframehost.exe" => {
                    let children = match native.children() {
                        Ok(children) => children,
                        Err(error) => {
                            tracing::warn!(kind = ?error.kind(), "child window class unavailable; requesting foreground review");
                            self.failures = self.failures.saturating_add(1);
                            self.processes.evict_process(owner);
                            return Resolution::Review;
                        }
                    };
                    if let Some(child) = foreground::ChildWindow::content(&children, owner) {
                        let metadata = self.process(child.process);
                        if Window::from_handle(child.window).process() != child.process {
                            self.processes.evict_process(child.process);
                            return Resolution::Review;
                        }
                        metadata
                    } else {
                        None
                    }
                }
                Some(_) | None => root.clone(),
            };
            if native.process() != owner || owner.get() == 0 {
                self.processes.evict_process(owner);
                return Resolution::Review;
            }
            let created = root.as_ref().map(|root| root.created);
            let mut privacy = PrivacyContext {
                capture_excluded: native.display_affinity().signal(),
                private_window: SystemSignal::Unknown,
                ..PrivacyContext::default()
            };
            let (app, exe_name, display_name) = match metadata {
                Some(metadata) => {
                    privacy.browser = match metadata.exe_name.as_str() {
                        "msedge.exe" | "chrome.exe" | "firefox.exe" | "brave.exe"
                        | "vivaldi.exe" | "opera.exe" => BrowserContext::Offline,
                        _other_executable => BrowserContext::NotBrowser,
                    };
                    (
                        metadata.identity,
                        Some(metadata.exe_name),
                        metadata.display_name,
                    )
                }
                None => (AppIdentity::Unresolved(class.clone()), None, class.clone()),
            };
            Foreground::Window {
                facts: Box::new(WindowFacts {
                    handle: window,
                    process: owner,
                    process_created_utc: created,
                    class,
                    app,
                    exe_name,
                    display_name,
                    title,
                }),
                privacy,
            }
        };
        Resolution::Event(ObservedEvent {
            at,
            event: CaptureEvent::Foreground {
                generation,
                source,
                foreground,
            },
        })
    }
    fn process(&mut self, process: ProcessId) -> Option<Metadata> {
        match self.resolve_process(process) {
            Ok(metadata) => Some(metadata),
            Err(error) => {
                tracing::debug!(kind = ?error.kind(), "process identity unresolved");
                self.failures = self.failures.saturating_add(1);
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
        let display_name = match self.names.find(&app_key) {
            Some(name) => name.clone(),
            None => {
                let name = match version::FileDescription::read(&image_path) {
                    Ok(Some(description)) => description.into_string(),
                    Ok(None) => exe_name.clone(),
                    Err(error) => {
                        tracing::warn!(kind = ?error.kind(), "application description unavailable; using executable filename");
                        exe_name.clone()
                    }
                };
                self.names.insert(app_key, name.clone());
                name
            }
        };
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
            || length > Appx::APPLICATION_USER_MODEL_ID_MAX_LENGTH
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
        let terminator = buffer
            .iter()
            .position(|unit| *unit == 0)
            .ok_or(ResolveProcessError::InvalidAumid)?;
        if terminator == 0 {
            return Err(ResolveProcessError::InvalidAumid);
        }
        String::from_utf16(&buffer[..terminator])
            .map(Some)
            .map_err(|source| ResolveProcessError::DecodeText { source })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            event_time_ms: 0,
            generation: quartz_domain::capture::ForegroundGeneration::new(1),
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
