//! FileDescription selection using version-resource translations (i18n 10 §5).

use crate::core::foreground::Translation;
use windows::{
    Win32::{
        Foundation::{
            self, ERROR_RESOURCE_DATA_NOT_FOUND, ERROR_RESOURCE_LANG_NOT_FOUND,
            ERROR_RESOURCE_NAME_NOT_FOUND, ERROR_RESOURCE_TYPE_NOT_FOUND, ERROR_SUCCESS,
        },
        Globalization,
        Storage::FileSystem,
    },
    core::{PCWSTR, w},
};

/// Upper bound for a version-resource allocation, capture 02 §2.1 resource bounds.
const VERSION_RESOURCE_BYTES: usize = 1_048_576;
/// Windows MUI_LANGUAGE_ID strings are hexadecimal, i18n 10 §5.
const LANGUAGE_ID_RADIX: u32 = 16;

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(super) enum ReadVersionInfoError {
    #[snafu(display("could not query version-resource size"))]
    SizeQuery { source: windows::core::Error },
    #[snafu(display("could not read version resource"))]
    Read { source: windows::core::Error },
    #[snafu(display("version-resource size exceeds addressable memory"))]
    Size { source: std::num::TryFromIntError },
    #[snafu(display("version resource exceeds the allocation limit"))]
    ResourceLimit,
    #[snafu(display("version resource has an invalid buffer layout"))]
    Layout,
    #[snafu(display("version text contains invalid UTF-16"))]
    Decode { source: std::string::FromUtf16Error },
    #[snafu(display("could not read Windows display languages"))]
    Languages { source: windows::core::Error },
    #[snafu(display("Windows display language identifier is invalid"))]
    LanguageId { source: std::num::ParseIntError },
}
#[derive(Debug)]
pub(super) enum ReadVersionInfoErrorKind {
    SizeQuery,
    Read,
    Size,
    ResourceLimit,
    Layout,
    Decode,
    Languages,
    LanguageId,
}
impl ReadVersionInfoError {
    pub(super) fn kind(&self) -> ReadVersionInfoErrorKind {
        match self {
            Self::SizeQuery { .. } => ReadVersionInfoErrorKind::SizeQuery,
            Self::Read { .. } => ReadVersionInfoErrorKind::Read,
            Self::Size { .. } => ReadVersionInfoErrorKind::Size,
            Self::ResourceLimit => ReadVersionInfoErrorKind::ResourceLimit,
            Self::Layout => ReadVersionInfoErrorKind::Layout,
            Self::Decode { .. } => ReadVersionInfoErrorKind::Decode,
            Self::Languages { .. } => ReadVersionInfoErrorKind::Languages,
            Self::LanguageId { .. } => ReadVersionInfoErrorKind::LanguageId,
        }
    }
}
pub(super) struct FileDescription(String);
impl FileDescription {
    pub(super) fn into_string(self) -> String {
        self.0
    }
    pub(super) fn read(path: &str) -> Result<Option<Self>, ReadVersionInfoError> {
        match Resource::read(path)? {
            Some(resource) => resource
                .description()
                .map(|description| description.map(Self)),
            None => Ok(None),
        }
    }
}
struct Resource {
    bytes: Vec<u8>,
}
struct Value<'a> {
    resource: &'a Resource,
    pointer: *mut std::ffi::c_void,
    length: usize,
}
impl Resource {
    fn read(path: &str) -> Result<Option<Self>, ReadVersionInfoError> {
        let path = super::super::WideString::new(path);
        // SAFETY: The path remains NUL-terminated, and clearing last error distinguishes malformed results.
        let length = unsafe {
            Foundation::SetLastError(ERROR_SUCCESS);
            FileSystem::GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None)
        };
        if length == 0 {
            // SAFETY: Reads this thread's error immediately after GetFileVersionInfoSizeW.
            let error = unsafe { Foundation::GetLastError() };
            return match error {
                ERROR_RESOURCE_DATA_NOT_FOUND
                | ERROR_RESOURCE_TYPE_NOT_FOUND
                | ERROR_RESOURCE_NAME_NOT_FOUND
                | ERROR_RESOURCE_LANG_NOT_FOUND => Ok(None),
                ERROR_SUCCESS => Err(ReadVersionInfoError::Layout),
                code => Err(ReadVersionInfoError::SizeQuery {
                    source: windows::core::Error::from_hresult(windows::core::HRESULT::from_win32(
                        code.0,
                    )),
                }),
            };
        }
        let size =
            usize::try_from(length).map_err(|source| ReadVersionInfoError::Size { source })?;
        if size > VERSION_RESOURCE_BYTES {
            return Err(ReadVersionInfoError::ResourceLimit);
        }
        let mut resource = Self {
            bytes: vec![0; size],
        };
        // SAFETY: The live buffer has the size returned by GetFileVersionInfoSizeW.
        unsafe {
            FileSystem::GetFileVersionInfoW(
                PCWSTR(path.as_ptr()),
                None,
                length,
                resource.bytes.as_mut_ptr().cast(),
            )
        }
        .map_err(|source| ReadVersionInfoError::Read { source })?;
        Ok(Some(resource))
    }
    fn query(&self, key: PCWSTR) -> Result<Option<Value<'_>>, ReadVersionInfoError> {
        let mut pointer = std::ptr::null_mut();
        let mut length = 0;
        // SAFETY: The block was initialized by GetFileVersionInfoW; both outputs are writable.
        if !unsafe {
            FileSystem::VerQueryValueW(self.bytes.as_ptr().cast(), key, &mut pointer, &mut length)
        }
        .as_bool()
        {
            // VerQueryValueW returns false when the requested resource key is absent.
            return Ok(None);
        }
        Ok(Some(Value {
            resource: self,
            pointer,
            length: usize::try_from(length)
                .map_err(|source| ReadVersionInfoError::Size { source })?,
        }))
    }
    fn description(&self) -> Result<Option<String>, ReadVersionInfoError> {
        let Some(value) = self.query(w!("\\VarFileInfo\\Translation"))? else {
            return Ok(None);
        };
        let translations = value.translations()?;
        let preferred = match UiLanguages::read() {
            Ok(languages) => languages,
            Err(error) => {
                tracing::warn!(kind = ?error.kind(), "display languages unavailable; using version-resource fallback order");
                UiLanguages {
                    identifiers: Vec::new(),
                }
            }
        };
        let Some(selected) = Translation::select(&translations, &preferred.identifiers) else {
            return Ok(None);
        };
        let key = super::super::WideString::new(&format!(
            "\\StringFileInfo\\{:04x}{:04x}\\FileDescription",
            selected.language, selected.codepage
        ));
        match self.query(PCWSTR(key.as_ptr()))? {
            Some(value) => value.text(),
            None => Ok(None),
        }
    }
}
impl Value<'_> {
    fn bytes(&self, length: usize) -> Result<&[u8], ReadVersionInfoError> {
        let offset = self
            .pointer
            .addr()
            .checked_sub(self.resource.bytes.as_ptr().addr())
            .ok_or(ReadVersionInfoError::Layout)?;
        let end = offset
            .checked_add(length)
            .ok_or(ReadVersionInfoError::Layout)?;
        self.resource
            .bytes
            .get(offset..end)
            .ok_or(ReadVersionInfoError::Layout)
    }
    fn translations(&self) -> Result<Vec<Translation>, ReadVersionInfoError> {
        let (entries, remainder) = self
            .bytes(self.length)?
            .as_chunks::<{ size_of::<Translation>() }>();
        if !remainder.is_empty() {
            return Err(ReadVersionInfoError::Layout);
        }
        Ok(entries
            .iter()
            .map(|chunk| Translation {
                language: u16::from_le_bytes([chunk[0], chunk[1]]),
                codepage: u16::from_le_bytes([chunk[2], chunk[3]]),
            })
            .collect())
    }
    fn text(&self) -> Result<Option<String>, ReadVersionInfoError> {
        let length = self
            .length
            .checked_mul(size_of::<u16>())
            .ok_or(ReadVersionInfoError::Layout)?;
        let units: Vec<_> = self
            .bytes(length)?
            .as_chunks::<{ size_of::<u16>() }>()
            .0
            .iter()
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        let text =
            String::from_utf16(&units).map_err(|source| ReadVersionInfoError::Decode { source })?;
        Ok((!text.trim().is_empty()).then_some(text))
    }
}
struct UiLanguages {
    identifiers: Vec<u16>,
}
impl UiLanguages {
    fn read() -> Result<Self, ReadVersionInfoError> {
        use windows::Win32::Globalization::MUI_LANGUAGE_ID;
        let mut count = 0;
        let mut length = 0;
        // SAFETY: The first call queries a MULTI_SZ length only.
        unsafe {
            Globalization::GetUserPreferredUILanguages(
                MUI_LANGUAGE_ID,
                &mut count,
                None,
                &mut length,
            )
        }
        .map_err(|source| ReadVersionInfoError::Languages { source })?;
        let size =
            usize::try_from(length).map_err(|source| ReadVersionInfoError::Size { source })?;
        if size == 0 {
            return Err(ReadVersionInfoError::Layout);
        }
        let mut buffer = vec![0; size];
        // SAFETY: The buffer holds the queried number of UTF-16 units.
        unsafe {
            Globalization::GetUserPreferredUILanguages(
                MUI_LANGUAGE_ID,
                &mut count,
                Some(windows::core::PWSTR(buffer.as_mut_ptr())),
                &mut length,
            )
        }
        .map_err(|source| ReadVersionInfoError::Languages { source })?;
        let identifiers = buffer
            .split(|unit| *unit == 0)
            .filter(|units| !units.is_empty())
            .map(|units| {
                let text = String::from_utf16(units)
                    .map_err(|source| ReadVersionInfoError::Decode { source })?;
                u16::from_str_radix(&text, LANGUAGE_ID_RADIX)
                    .map_err(|source| ReadVersionInfoError::LanguageId { source })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { identifiers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_executable_preserves_the_query_error() {
        let directory = tempfile::tempdir().expect("temporary version query directory");
        let path = directory.path().join("missing.exe");
        let path = path.to_str().expect("Unicode test path");
        assert!(matches!(
            FileDescription::read(path),
            Err(ReadVersionInfoError::SizeQuery { .. })
        ));
    }
    #[test]
    fn malformed_version_values_are_errors_instead_of_missing_metadata() {
        let resource = Resource {
            bytes: vec![0x00, 0xD8, 0, 0],
        };
        let value = Value {
            resource: &resource,
            pointer: resource.bytes.as_ptr().cast_mut().cast(),
            length: 2,
        };
        assert!(matches!(
            value.text(),
            Err(ReadVersionInfoError::Decode { .. })
        ));
        let partial_translation = Value {
            resource: &resource,
            pointer: resource.bytes.as_ptr().cast_mut().cast(),
            length: 3,
        };
        assert!(matches!(
            partial_translation.translations(),
            Err(ReadVersionInfoError::Layout)
        ));
        let outside = Value {
            resource: &resource,
            pointer: std::ptr::null_mut(),
            length: 2,
        };
        assert!(matches!(outside.text(), Err(ReadVersionInfoError::Layout)));
    }
}
