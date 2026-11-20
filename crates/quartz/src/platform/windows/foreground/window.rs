//! Borrowed native windows; metadata queries run only on the capture worker.

use crate::core::foreground::{ChildWindow, DisplayAffinity, Visibility};
use quartz_domain::capture::{ProcessId, WindowHandle};
use windows::{
    Win32::{
        Foundation::{ERROR_INVALID_WINDOW_HANDLE, HWND, LPARAM},
        UI::WindowsAndMessaging,
    },
    core::{BOOL, HRESULT},
};

/// Window classes are limited to 256 characters by RegisterClassExW, capture 02 §2.1.
const CLASS_BUFFER_UNITS: usize = 257;

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(super) enum ReadWindowClassError {
    #[snafu(display("could not read window class"))]
    Query { source: windows::core::Error },
    #[snafu(display("window class length exceeds addressable memory"))]
    Count { source: std::num::TryFromIntError },
    #[snafu(display("window class contains invalid UTF-16"))]
    Decode { source: std::string::FromUtf16Error },
}
#[derive(Debug)]
pub(super) enum ReadWindowClassErrorKind {
    Query,
    Count,
    Decode,
}
impl ReadWindowClassError {
    pub(super) fn log_level(&self) -> tracing::Level {
        match self {
            Self::Query { source }
                if source.code() == HRESULT::from_win32(ERROR_INVALID_WINDOW_HANDLE.0) =>
            {
                tracing::Level::DEBUG
            }
            _ => tracing::Level::WARN,
        }
    }
    pub(super) fn kind(&self) -> ReadWindowClassErrorKind {
        match self {
            Self::Query { .. } => ReadWindowClassErrorKind::Query,
            Self::Count { .. } => ReadWindowClassErrorKind::Count,
            Self::Decode { .. } => ReadWindowClassErrorKind::Decode,
        }
    }
}
struct ChildEnumeration {
    windows: Vec<ChildWindow>,
    result: Result<(), ReadWindowClassError>,
}
#[derive(Clone, Copy)]
pub(super) struct Window(HWND);
impl Window {
    pub(super) fn raw(self) -> HWND {
        self.0
    }
    pub(super) fn from_native(window: HWND) -> Self {
        Self(window)
    }
    pub(super) fn from_handle(window: WindowHandle) -> Self {
        Self(HWND(std::ptr::with_exposed_provenance_mut(
            usize::try_from(window.get()).expect("native window originated on this platform"),
        )))
    }
    pub(super) fn handle(self) -> WindowHandle {
        WindowHandle::new(
            u64::try_from(self.0.0.expose_provenance()).expect("Windows pointer fits u64"),
        )
    }
    pub(super) fn process(self) -> ProcessId {
        let window = self.0;
        let mut process = 0;
        // SAFETY: Only the writable process ID is accessed; an invalid HWND returns zero.
        unsafe { WindowsAndMessaging::GetWindowThreadProcessId(window, Some(&mut process)) };
        ProcessId::new(process)
    }
    pub(super) fn class(self) -> Result<String, ReadWindowClassError> {
        let window = self.0;
        let mut class = [0; CLASS_BUFFER_UNITS];
        // SAFETY: The stack buffer is writable for the complete API call.
        let length = unsafe { WindowsAndMessaging::GetClassNameW(window, &mut class) };
        if length <= 0 {
            return Err(ReadWindowClassError::Query {
                source: windows::core::Error::from_thread(),
            });
        }
        let length =
            usize::try_from(length).map_err(|source| ReadWindowClassError::Count { source })?;
        String::from_utf16(&class[..length])
            .map_err(|source| ReadWindowClassError::Decode { source })
    }
    pub(super) fn display_affinity(self) -> DisplayAffinity {
        let window = self.0;
        let mut affinity = 0;
        // SAFETY: affinity is a writable output and no HWND is dereferenced by Rust.
        let queried =
            unsafe { WindowsAndMessaging::GetWindowDisplayAffinity(window, &mut affinity) };
        match queried {
            Ok(()) => DisplayAffinity::from_raw(Some(affinity)),
            Err(error) => {
                tracing::debug!(code = error.code().0, "display affinity unavailable");
                DisplayAffinity::Unavailable
            }
        }
    }
    pub(super) fn children(self) -> Result<Vec<ChildWindow>, ReadWindowClassError> {
        let parent = self.0;
        let mut enumeration = ChildEnumeration {
            windows: Vec::new(),
            result: Ok(()),
        };
        let pointer = std::ptr::from_mut(&mut enumeration);
        // EnumChildWindows has no documented return status; the callback records class-query errors.
        // SAFETY: Enumeration is synchronous and the context stays uniquely borrowed until return.
        let _ = unsafe {
            WindowsAndMessaging::EnumChildWindows(
                Some(parent),
                Some(enumerate_child),
                LPARAM(pointer.expose_provenance().cast_signed()),
            )
        };
        enumeration.result.map(|()| enumeration.windows)
    }
}
unsafe extern "system" fn enumerate_child(window: HWND, parameter: LPARAM) -> BOOL {
    let pointer =
        std::ptr::with_exposed_provenance_mut::<ChildEnumeration>(parameter.0.cast_unsigned());
    // SAFETY: children() supplies this unique enumeration pointer for this synchronous call.
    let enumeration = unsafe { &mut *pointer };
    let native = Window::from_native(window);
    let class = match native.class() {
        Ok(class) => class,
        Err(error) => {
            enumeration.result = Err(error);
            return BOOL::from(false);
        }
    };
    // SAFETY: IsWindowVisible queries an opaque handle without passing Rust references.
    let visibility = if unsafe { WindowsAndMessaging::IsWindowVisible(window) }.as_bool() {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    enumeration.windows.push(ChildWindow {
        window: native.handle(),
        process: native.process(),
        class,
        visibility,
    });
    BOOL::from(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE};

    #[test]
    fn only_invalid_window_handle_class_queries_use_debug_level() {
        let invalid_window =
            windows::core::Error::from_hresult(HRESULT::from_win32(ERROR_INVALID_WINDOW_HANDLE.0));
        assert_eq!(
            ReadWindowClassError::Query {
                source: invalid_window,
            }
            .log_level(),
            tracing::Level::DEBUG
        );
        for code in [ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE] {
            let source = windows::core::Error::from_hresult(HRESULT::from_win32(code.0));
            assert_eq!(
                ReadWindowClassError::Query { source }.log_level(),
                tracing::Level::WARN
            );
        }
        let count = u8::try_from(u16::MAX).expect_err("out-of-range count");
        assert_eq!(
            ReadWindowClassError::Count { source: count }.log_level(),
            tracing::Level::WARN
        );
        let decode = String::from_utf16(&[0xD800]).expect_err("unpaired surrogate");
        assert_eq!(
            ReadWindowClassError::Decode { source: decode }.log_level(),
            tracing::Level::WARN
        );
    }

    #[test]
    fn invalid_window_class_is_an_error_instead_of_empty_rule_input() {
        let result = Window::from_native(HWND::default()).class();
        assert!(matches!(result, Err(ReadWindowClassError::Query { .. })));
    }
}
