//! Window property-store queries and pure hosted-application identity selection.

use super::Window;
use windows::Win32::{
    Storage::{EnhancedStorage::PKEY_AppUserModel_ID, Packaging::Appx},
    System::{
        Com::StructuredStorage::{self, PROPVARIANT},
        Variant::{VT_BSTR, VT_EMPTY, VT_LPWSTR},
    },
    UI::{
        Shell::PropertiesSystem::{self, IPropertyStore},
        WindowsAndMessaging::STRSAFE_E_INSUFFICIENT_BUFFER,
    },
};

/// APPLICATION_USER_MODEL_ID_MAX_LENGTH (130 UTF-16 units) plus the NUL terminator.
pub(super) const AUMID_BUFFER_UNITS: usize = 131;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Aumid(String);
#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(super) enum DecodeAumidError {
    #[snafu(display("application user model identity is empty"))]
    Empty,
    #[snafu(display("application user model identity exceeds the UTF-16 length limit"))]
    TooLong,
    #[snafu(display("application user model identity contains an interior terminator"))]
    InteriorNul,
    #[snafu(display("application user model identity has no terminator"))]
    MissingTerminator,
    #[snafu(display("application user model identity contains invalid UTF-16"))]
    DecodeText { source: std::string::FromUtf16Error },
}
impl Aumid {
    pub(super) fn from_utf16(units: &[u16]) -> Result<Self, DecodeAumidError> {
        if units.is_empty() {
            return Err(DecodeAumidError::Empty);
        }
        if units.len() > Self::max_units() {
            return Err(DecodeAumidError::TooLong);
        }
        if units.contains(&0) {
            return Err(DecodeAumidError::InteriorNul);
        }
        String::from_utf16(units)
            .map(Self)
            .map_err(|source| DecodeAumidError::DecodeText { source })
    }
    pub(super) fn from_terminated(units: &[u16]) -> Result<Self, DecodeAumidError> {
        let end = units
            .iter()
            .position(|unit| *unit == 0)
            .ok_or(DecodeAumidError::MissingTerminator)?;
        Self::from_utf16(&units[..end])
    }
    fn max_units() -> usize {
        usize::try_from(Appx::APPLICATION_USER_MODEL_ID_MAX_LENGTH).expect("AUMID limit fits usize")
    }
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
    pub(super) fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(super) enum ReadFrameAumidError {
    #[snafu(display("could not open window property store"))]
    OpenStore { source: windows::core::Error },
    #[snafu(display("could not read window application user model property"))]
    ReadProperty { source: windows::core::Error },
    #[snafu(display("window application user model property is not a string"))]
    InvalidType,
    #[snafu(display("could not convert window application user model property to a string"))]
    ConvertText { source: windows::core::Error },
    #[snafu(display("could not decode window application user model identity"))]
    Decode { source: DecodeAumidError },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrameAumidUnavailable {
    Missing,
    OpenStore,
    ReadProperty,
    InvalidType,
    ConvertText,
    Empty,
    TooLong,
    InvalidText,
}
impl ReadFrameAumidError {
    fn kind(&self) -> FrameAumidUnavailable {
        match self {
            Self::OpenStore { .. } => FrameAumidUnavailable::OpenStore,
            Self::ReadProperty { .. } => FrameAumidUnavailable::ReadProperty,
            Self::InvalidType => FrameAumidUnavailable::InvalidType,
            Self::ConvertText { .. } => FrameAumidUnavailable::ConvertText,
            Self::Decode {
                source: DecodeAumidError::Empty,
            } => FrameAumidUnavailable::Empty,
            Self::Decode {
                source: DecodeAumidError::TooLong,
            } => FrameAumidUnavailable::TooLong,
            Self::Decode { .. } => FrameAumidUnavailable::InvalidText,
        }
    }
}

pub(super) enum HostedIdentity<Content> {
    Content(Content),
    Frame(Aumid),
    Unresolved(FrameAumidUnavailable),
}
impl<Content> HostedIdentity<Content> {
    pub(super) fn from_evidence(
        content: Option<Content>,
        frame: Result<Option<Aumid>, ReadFrameAumidError>,
    ) -> Self {
        match content {
            Some(metadata) => Self::Content(metadata),
            None => match frame {
                Ok(Some(aumid)) => Self::Frame(aumid),
                Ok(None) => Self::Unresolved(FrameAumidUnavailable::Missing),
                Err(error) => Self::Unresolved(error.kind()),
            },
        }
    }
}

// windows-rs PROPVARIANT::Drop clears its owned value with PropVariantClear.
struct PropertyValue(PROPVARIANT);
impl PropertyValue {
    fn aumid(&self) -> Result<Option<Aumid>, ReadFrameAumidError> {
        match self.0.vt() {
            VT_EMPTY => return Ok(None),
            VT_BSTR | VT_LPWSTR => {}
            _ => return Err(ReadFrameAumidError::InvalidType),
        }
        let mut units = [0; AUMID_BUFFER_UNITS];
        // SAFETY: The live variant has a checked string type; units is writable for its fixed cap.
        unsafe { StructuredStorage::PropVariantToString(&self.0, &mut units) }.map_err(
            |source| {
                if source.code() == STRSAFE_E_INSUFFICIENT_BUFFER {
                    ReadFrameAumidError::Decode {
                        source: DecodeAumidError::TooLong,
                    }
                } else {
                    ReadFrameAumidError::ConvertText { source }
                }
            },
        )?;
        Aumid::from_terminated(&units)
            .map(Some)
            .map_err(|source| ReadFrameAumidError::Decode { source })
    }
}
impl Window {
    pub(super) fn frame_aumid(self) -> Result<Option<Aumid>, ReadFrameAumidError> {
        // SAFETY: The caller's capture worker has initialized COM. The returned interface owns a
        // reference released by windows-rs when store leaves scope, before its apartment exits.
        let store: IPropertyStore =
            unsafe { PropertiesSystem::SHGetPropertyStoreForWindow(self.raw()) }
                .map_err(|source| ReadFrameAumidError::OpenStore { source })?;
        // SAFETY: The key is a valid SDK constant and the store interface remains live.
        let value = unsafe { store.GetValue(&PKEY_AppUserModel_ID) }
            .map(PropertyValue)
            .map_err(|source| ReadFrameAumidError::ReadProperty { source })?;
        value.aumid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_domain::capture::{ProcessId, WindowHandle};
    use windows::core::BSTR;

    #[test]
    #[ignore = "read-only check requiring an existing visible ApplicationFrameWindow"]
    fn reads_visible_frame_aumid_in_capture_mta() {
        use super::super::CaptureApartment;
        use windows::{
            Win32::{
                Foundation::{HWND, LPARAM},
                UI::WindowsAndMessaging,
            },
            core::BOOL,
        };
        unsafe extern "system" fn collect(window: HWND, parameter: LPARAM) -> BOOL {
            let pointer = std::ptr::with_exposed_provenance_mut::<Vec<WindowHandle>>(
                parameter.0.cast_unsigned(),
            );
            // SAFETY: EnumWindows synchronously borrows this test-owned vector until return.
            unsafe { &mut *pointer }.push(Window::from_native(window).handle());
            BOOL::from(true)
        }
        let _apartment = CaptureApartment::initialize().expect("capture worker MTA");
        let mut handles = Vec::<WindowHandle>::new();
        let pointer = std::ptr::from_mut(&mut handles);
        // SAFETY: The callback and writable context remain live throughout synchronous enumeration.
        unsafe {
            WindowsAndMessaging::EnumWindows(
                Some(collect),
                windows::Win32::Foundation::LPARAM(pointer.expose_provenance().cast_signed()),
            )
        }
        .expect("enumerate existing desktop windows");
        let mut found = 0;
        for handle in handles {
            let native = Window::from_handle(handle);
            if !native
                .class()
                .is_ok_and(|class| class == "ApplicationFrameWindow")
            {
                continue;
            }
            // SAFETY: Visibility only queries this opaque handle and changes no desktop state.
            if !unsafe { WindowsAndMessaging::IsWindowVisible(native.raw()) }.as_bool() {
                continue;
            }
            if let Some(aumid) = native.frame_aumid().expect("read visible frame property") {
                assert!(!aumid.as_str().is_empty());
                found += 1;
                println!("visible frame hwnd={} property=Present", handle.get());
            }
        }
        assert!(found > 0, "at least one visible frame must have an AUMID");
    }

    #[test]
    fn hosted_identity_prefers_content_then_frame_aumid_and_classifies_absence() {
        let content_process = ProcessId::new(3);
        let value: Vec<_> = "Package!App".encode_utf16().collect();
        let valid = Aumid::from_utf16(&value).expect("valid test AUMID");
        assert!(
            matches!(HostedIdentity::from_evidence(Some(content_process), Ok(Some(valid.clone()))),
            HostedIdentity::Content(process) if process == content_process)
        );
        assert!(matches!(
            HostedIdentity::<ProcessId>::from_evidence(None, Ok(Some(valid))),
            HostedIdentity::Frame(_)
        ));
        assert!(matches!(
            HostedIdentity::<ProcessId>::from_evidence(None, Ok(None)),
            HostedIdentity::Unresolved(FrameAumidUnavailable::Missing)
        ));
        let invalid = Aumid::from_utf16(&[0xD800]).expect_err("unpaired surrogate");
        assert!(matches!(
            HostedIdentity::<ProcessId>::from_evidence(
                None,
                Err(ReadFrameAumidError::Decode { source: invalid })
            ),
            HostedIdentity::Unresolved(FrameAumidUnavailable::InvalidText)
        ));
        let empty = Aumid::from_utf16(&[]).expect_err("empty AUMID");
        assert!(matches!(
            HostedIdentity::<ProcessId>::from_evidence(
                None,
                Err(ReadFrameAumidError::Decode { source: empty })
            ),
            HostedIdentity::Unresolved(FrameAumidUnavailable::Empty)
        ));
    }
    #[test]
    fn hosted_identity_falls_back_when_content_process_query_is_unavailable() {
        let content_query: Option<ProcessId> = None;
        let units: Vec<_> = "Package!App".encode_utf16().collect();
        let frame = Aumid::from_utf16(&units).expect("valid frame AUMID");
        assert!(
            matches!(HostedIdentity::from_evidence(content_query, Ok(Some(frame))),
            HostedIdentity::Frame(value) if value.as_str() == "Package!App")
        );
        assert!(matches!(
            HostedIdentity::from_evidence(content_query, Ok(None)),
            HostedIdentity::Unresolved(FrameAumidUnavailable::Missing)
        ));
        assert!(matches!(
            HostedIdentity::from_evidence(content_query, Err(ReadFrameAumidError::InvalidType)),
            HostedIdentity::Unresolved(FrameAumidUnavailable::InvalidType)
        ));
    }

    #[test]
    fn property_string_helper_preserves_utf16_and_rejects_truncated_aumids() {
        let units: Vec<_> = "Package!应用".encode_utf16().collect();
        let property = PropertyValue(PROPVARIANT::from(BSTR::from_wide(&units)));
        assert_eq!(
            property
                .aumid()
                .expect("BSTR property")
                .expect("present AUMID")
                .as_str(),
            "Package!应用"
        );
        let maximum = vec![u16::from(b'A'); Aumid::max_units()];
        let property = PropertyValue(PROPVARIANT::from(BSTR::from_wide(&maximum)));
        assert_eq!(
            property
                .aumid()
                .expect("maximum-length property")
                .expect("present AUMID")
                .as_str()
                .encode_utf16()
                .count(),
            maximum.len()
        );
        let oversized = vec![u16::from(b'A'); Aumid::max_units() + 1];
        let property = PropertyValue(PROPVARIANT::from(BSTR::from_wide(&oversized)));
        assert!(matches!(
            property.aumid(),
            Err(ReadFrameAumidError::Decode {
                source: DecodeAumidError::TooLong
            })
        ));
        let property = PropertyValue(PROPVARIANT::from(BSTR::from_wide(&[0xD800])));
        assert!(matches!(
            property.aumid(),
            Err(ReadFrameAumidError::Decode {
                source: DecodeAumidError::DecodeText { .. }
            })
        ));
        let property = PropertyValue(PROPVARIANT::from(BSTR::new()));
        assert!(matches!(
            property.aumid(),
            Err(ReadFrameAumidError::Decode {
                source: DecodeAumidError::Empty
            })
        ));
    }

    #[test]
    fn aumid_validation_bounds_utf16_and_requires_a_terminated_process_value() {
        assert_eq!(AUMID_BUFFER_UNITS, Aumid::max_units() + 1);
        let mut maximum = vec![u16::from(b'A'); Aumid::max_units()];
        assert!(Aumid::from_utf16(&maximum).is_ok());
        maximum.push(0);
        assert!(Aumid::from_terminated(&maximum).is_ok());
        assert!(matches!(
            Aumid::from_utf16(&vec![u16::from(b'A'); Aumid::max_units() + 1]),
            Err(DecodeAumidError::TooLong)
        ));
        assert!(matches!(
            Aumid::from_utf16(&[u16::from(b'A'), 0]),
            Err(DecodeAumidError::InteriorNul)
        ));
        assert!(matches!(
            Aumid::from_terminated(&[u16::from(b'A')]),
            Err(DecodeAumidError::MissingTerminator)
        ));
        assert_eq!(
            Aumid::from_terminated(&[u16::from(b'A'), 0])
                .expect("terminated identity")
                .as_str(),
            "A"
        );
        assert!(
            PropertyValue(PROPVARIANT::default())
                .aumid()
                .expect("empty property")
                .is_none()
        );
        let non_string = PROPVARIANT::from(0u32);
        assert!(matches!(
            PropertyValue(non_string).aumid(),
            Err(ReadFrameAumidError::InvalidType)
        ));
    }
}
