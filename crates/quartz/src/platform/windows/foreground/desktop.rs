//! Input-desktop comparison without switching the calling thread's desktop.

use crate::core::foreground::Desktop;
use windows::{
    Win32::{
        Foundation::{ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, HANDLE},
        System::{
            StationsAndDesktops::{
                self, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, HDESK, UOI_NAME,
            },
            Threading,
        },
    },
    core::HRESULT,
};

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
enum ReadDesktopError {
    #[snafu(display("could not open input desktop"))]
    Open { source: windows::core::Error },
    #[snafu(display("could not read thread desktop"))]
    Thread { source: windows::core::Error },
    #[snafu(display("could not query desktop name length"))]
    NameLength { source: windows::core::Error },
    #[snafu(display("could not read desktop name"))]
    Name { source: windows::core::Error },
    #[snafu(display("desktop name length exceeds addressable memory"))]
    Size { source: std::num::TryFromIntError },
    #[snafu(display("desktop name has an invalid buffer layout"))]
    Layout,
}
#[derive(Debug)]
enum ReadDesktopErrorKind {
    Open,
    Thread,
    NameLength,
    Name,
    Size,
    Layout,
}
impl ReadDesktopError {
    fn log_level(&self) -> tracing::Level {
        match self {
            Self::Open { source }
                if source.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) =>
            {
                tracing::Level::DEBUG
            }
            _ => tracing::Level::WARN,
        }
    }
    fn kind(&self) -> ReadDesktopErrorKind {
        match self {
            Self::Open { .. } => ReadDesktopErrorKind::Open,
            Self::Thread { .. } => ReadDesktopErrorKind::Thread,
            Self::NameLength { .. } => ReadDesktopErrorKind::NameLength,
            Self::Name { .. } => ReadDesktopErrorKind::Name,
            Self::Size { .. } => ReadDesktopErrorKind::Size,
            Self::Layout => ReadDesktopErrorKind::Layout,
        }
    }
}
#[derive(Clone, Copy)]
struct DesktopHandle(HDESK);
impl DesktopHandle {
    fn current() -> Result<Self, ReadDesktopError> {
        // SAFETY: The returned desktop is borrowed from this thread and must not be closed.
        unsafe { StationsAndDesktops::GetThreadDesktop(Threading::GetCurrentThreadId()) }
            .map(Self)
            .map_err(|source| ReadDesktopError::Thread { source })
    }
    fn name(self) -> Result<Vec<u16>, ReadDesktopError> {
        let mut bytes = 0;
        // SAFETY: This call queries the required output byte count using a live desktop handle.
        match unsafe {
            StationsAndDesktops::GetUserObjectInformationW(
                HANDLE(self.0.0),
                UOI_NAME,
                None,
                0,
                Some(&mut bytes),
            )
        } {
            Ok(()) => {}
            Err(source) if source.code() == HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0) => {}
            Err(source) => return Err(ReadDesktopError::NameLength { source }),
        }
        let size = usize::try_from(bytes).map_err(|source| ReadDesktopError::Size { source })?;
        if size == 0 || !size.is_multiple_of(size_of::<u16>()) {
            return Err(ReadDesktopError::Layout);
        }
        let mut buffer = vec![0u16; size / size_of::<u16>()];
        let mut written = 0;
        // SAFETY: The writable buffer has exactly the byte count returned by the size query.
        unsafe {
            StationsAndDesktops::GetUserObjectInformationW(
                HANDLE(self.0.0),
                UOI_NAME,
                Some(buffer.as_mut_ptr().cast()),
                bytes,
                Some(&mut written),
            )
        }
        .map_err(|source| ReadDesktopError::Name { source })?;
        if written > bytes || buffer.last() != Some(&0) || buffer.first() == Some(&0) {
            return Err(ReadDesktopError::Layout);
        }
        Ok(buffer)
    }
}
struct InputDesktop(DesktopHandle);
impl InputDesktop {
    fn open() -> Result<Self, ReadDesktopError> {
        // SAFETY: Opens only read access and does not change the thread's desktop.
        unsafe {
            StationsAndDesktops::OpenInputDesktop(
                DESKTOP_CONTROL_FLAGS::default(),
                false,
                DESKTOP_READOBJECTS,
            )
        }
        .map(|handle| Self(DesktopHandle(handle)))
        .map_err(|source| ReadDesktopError::Open { source })
    }
}
impl Drop for InputDesktop {
    fn drop(&mut self) {
        // SAFETY: This uniquely owned handle came from OpenInputDesktop.
        // Best-effort teardown: Drop cannot report desktop closure failures.
        let _ = unsafe { StationsAndDesktops::CloseDesktop(self.0.0) };
    }
}
impl Desktop {
    pub(super) fn observe() -> Self {
        match Self::query() {
            Ok(desktop) => desktop,
            Err(error) => {
                if error.log_level() == tracing::Level::DEBUG {
                    tracing::debug!(kind = ?error.kind(), "desktop observation failed");
                } else {
                    tracing::warn!(kind = ?error.kind(), "desktop observation failed");
                }
                Self::Unknown
            }
        }
    }
    fn query() -> Result<Self, ReadDesktopError> {
        let input = InputDesktop::open()?;
        let input_name = input.0.name()?;
        let current_name = DesktopHandle::current()?.name()?;
        Ok(Self::compare_names(&input_name, &current_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::ERROR_INVALID_HANDLE;

    #[test]
    fn only_input_desktop_access_denial_uses_debug_level() {
        let denied = windows::core::Error::from_hresult(HRESULT::from_win32(ERROR_ACCESS_DENIED.0));
        let error = ReadDesktopError::Open {
            source: denied.clone(),
        };
        assert_eq!(error.log_level(), tracing::Level::DEBUG);
        assert_eq!(
            ReadDesktopError::Thread { source: denied }.log_level(),
            tracing::Level::WARN
        );
        let invalid =
            windows::core::Error::from_hresult(HRESULT::from_win32(ERROR_INVALID_HANDLE.0));
        assert_eq!(
            ReadDesktopError::Open { source: invalid }.log_level(),
            tracing::Level::WARN
        );
        assert_eq!(ReadDesktopError::Layout.log_level(), tracing::Level::WARN);
    }
}
