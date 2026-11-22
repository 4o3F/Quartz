//! Windows errors retained after GPUI exits must not retain apartment-owned error information.

use snafu::Snafu;
use windows::core::HRESULT;

/// A Windows failure captured while its COM error information is still valid.
#[derive(Debug, Snafu)]
#[snafu(display("{message} ({code})"))]
pub struct CallWindowsError {
    code: HRESULT,
    message: String,
}
impl CallWindowsError {
    /// The original HRESULT, independent of the lifetime of the COM apartment.
    pub const fn code(&self) -> HRESULT {
        self.code
    }
}
impl From<windows::core::Error> for CallWindowsError {
    fn from(source: windows::core::Error) -> Self {
        Self {
            code: source.code(),
            message: source.message(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::E_FAIL;

    #[test]
    fn captured_error_preserves_code_and_description_without_native_error_information() {
        let source = windows::core::Error::from_hresult(E_FAIL);
        let message = source.message();
        let captured = CallWindowsError::from(source);
        assert_eq!(captured.code(), E_FAIL);
        assert!(captured.to_string().contains(&message));
        assert!(captured.to_string().contains(&E_FAIL.to_string()));
    }
}
