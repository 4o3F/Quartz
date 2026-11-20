//! Explicit per-user DACL applied to every pipe instance.

use super::{
    super::{WideString, structure_size},
    identity::LocalAllocation,
};
use snafu::{ResultExt, Snafu};
use windows::{
    Win32::{
        Foundation::HLOCAL,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
    },
    core::PCWSTR,
};

#[derive(Debug, Snafu)]
#[snafu(display(
    "could not create pipe security descriptor (ConvertStringSecurityDescriptorToSecurityDescriptorW)"
))]
pub(crate) struct CreatePipeSecurityError {
    source: windows::core::Error,
}
#[derive(Debug)]
pub(super) struct PipeSecurity {
    descriptor: LocalAllocation,
}
impl PipeSecurity {
    pub(super) fn new(
        sid: &crate::instance_name::UserSid,
    ) -> Result<Self, CreatePipeSecurityError> {
        let text = WideString::new(&format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})"));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: text is terminated and descriptor is writable output; ownership transfers on success.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(text.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .context(CreatePipeSecuritySnafu)?;
        Ok(Self {
            descriptor: LocalAllocation(HLOCAL(descriptor.0)),
        })
    }
    pub(super) fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: structure_size::<SECURITY_ATTRIBUTES>(),
            lpSecurityDescriptor: self.descriptor.0.0,
            bInheritHandle: false.into(),
        }
    }
}
