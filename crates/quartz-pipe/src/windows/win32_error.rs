//! Structured Win32 error comparisons for synchronous and asynchronous pipe APIs.

use std::io;
use windows::{Win32::Foundation::WIN32_ERROR, core::HRESULT};

pub(super) trait Win32ErrorCode {
    fn is_win32(&self, code: WIN32_ERROR) -> bool;
}
impl Win32ErrorCode for windows::core::Error {
    fn is_win32(&self, code: WIN32_ERROR) -> bool {
        self.code() == HRESULT::from_win32(code.0)
    }
}
impl Win32ErrorCode for io::Error {
    fn is_win32(&self, code: WIN32_ERROR) -> bool {
        self.raw_os_error().is_some_and(|raw| {
            windows::core::Error::from(io::Error::from_raw_os_error(raw)).is_win32(code)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY};
    #[test]
    fn compares_codes_and_rejects_errors_without_os_codes() {
        let error = io::Error::from_raw_os_error(
            i32::try_from(ERROR_PIPE_BUSY.0).expect("Win32 code fits i32"),
        );
        assert!(error.is_win32(ERROR_PIPE_BUSY));
        assert!(!error.is_win32(ERROR_ACCESS_DENIED));
        assert!(!io::Error::other("no OS code").is_win32(ERROR_PIPE_BUSY));
    }
}
