//! Mutual pipe peer authentication using token, session, and canonical image paths.

use super::super::handle::OwnedHandle;
use super::win32_error::Win32ErrorCode;
use crate::{instance_name::UserSid, ipc::AllowedRoles};
use snafu::{ResultExt, Snafu};
use std::{
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, HLOCAL, LocalFree},
        Globalization::{CSTR_EQUAL, CompareStringOrdinal},
        Security::{
            self, Authorization::ConvertSidToStringSidW, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        System::{
            RemoteDesktop::ProcessIdToSessionId,
            Threading::{self, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    },
    core::PWSTR,
};

/// Maximum extended Windows path length, including its terminator (04 §1).
const IMAGE_PATH_CAPACITY: usize = 32768;

#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum CheckPeerError {
    #[snafu(display("could not inspect peer ({call})"))]
    Windows {
        call: &'static str,
        source: windows::core::Error,
    },
    #[snafu(display("could not decode SID string"))]
    DecodeSid { source: std::string::FromUtf16Error },
    #[snafu(display("could not normalize executable path"))]
    Path { source: std::io::Error },
    #[snafu(display("peer belongs to another user"))]
    User,
    #[snafu(display("peer belongs to another session"))]
    Session,
    #[snafu(display("peer executable is not allowed"))]
    Executable,
}

/// Owns a LocalAlloc allocation returned by the Windows security APIs.
#[derive(Debug)]
pub(super) struct LocalAllocation(pub HLOCAL);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: The allocation was returned by a LocalAlloc-based API and is uniquely owned.
        let remaining = unsafe { LocalFree(Some(self.0)) };
        if !remaining.0.is_null() {
            tracing::warn!("could not free Windows security allocation");
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProcessIdentity {
    sid: UserSid,
    session: u32,
    image: PathBuf,
}
impl ProcessIdentity {
    pub(crate) fn current() -> Result<Self, CheckPeerError> {
        // SAFETY: GetCurrentProcessId takes no pointers or ownership.
        Self::read(unsafe { Threading::GetCurrentProcessId() })
    }
    pub(crate) fn sid(&self) -> &UserSid {
        &self.sid
    }
    pub(crate) fn pipe_name(&self) -> super::PipeName {
        super::PipeName::from_sid(&self.sid)
    }
    pub(crate) fn read(pid: u32) -> Result<Self, CheckPeerError> {
        // SAFETY: Query-only access, no inheritance, and a PID supplied by the kernel.
        let process =
            unsafe { Threading::OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
                .context(check_peer_error::WindowsSnafu {
                    call: "OpenProcess",
                })?;
        // SAFETY: OpenProcess transferred a uniquely owned valid process handle.
        let process = unsafe { OwnedHandle::from_raw(process) };
        let mut token = HANDLE::default();
        // SAFETY: The process handle is live and token is writable output storage.
        unsafe { Threading::OpenProcessToken(process.raw(), TOKEN_QUERY, &mut token) }.context(
            check_peer_error::WindowsSnafu {
                call: "OpenProcessToken",
            },
        )?;
        // SAFETY: OpenProcessToken transferred a uniquely owned valid token handle.
        let token = unsafe { OwnedHandle::from_raw(token) };
        let mut bytes = 0;
        // SAFETY: A null buffer queries the TOKEN_USER allocation size.
        let queried =
            unsafe { Security::GetTokenInformation(token.raw(), TokenUser, None, 0, &mut bytes) };
        if let Err(source) = queried
            && !source.is_win32(ERROR_INSUFFICIENT_BUFFER)
        {
            return Err(CheckPeerError::Windows {
                call: "GetTokenInformation(size)",
                source,
            });
        }
        let mut buffer = vec![
            0usize;
            usize::try_from(bytes)
                .expect("DWORD fits Windows usize")
                .div_ceil(size_of::<usize>())
        ];
        // SAFETY: usize storage supplies TOKEN_USER alignment and at least the requested byte capacity.
        unsafe {
            Security::GetTokenInformation(
                token.raw(),
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                bytes,
                &mut bytes,
            )
        }
        .context(check_peer_error::WindowsSnafu {
            call: "GetTokenInformation",
        })?;
        let mut sid = PWSTR::null();
        // SAFETY: TOKEN_USER and its SID are initialized inside the live aligned buffer.
        unsafe {
            ConvertSidToStringSidW((*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid, &mut sid)
        }
        .context(check_peer_error::WindowsSnafu {
            call: "ConvertSidToStringSidW",
        })?;
        let _allocation = LocalAllocation(HLOCAL(sid.0.cast()));
        // SAFETY: ConvertSidToStringSidW returned a live NUL-terminated UTF-16 SID string.
        let sid = unsafe { sid.to_string() }.context(check_peer_error::DecodeSidSnafu)?;
        let mut session = 0;
        // SAFETY: session is writable DWORD storage and pid came from the kernel.
        unsafe { ProcessIdToSessionId(pid, &mut session) }.context(
            check_peer_error::WindowsSnafu {
                call: "ProcessIdToSessionId",
            },
        )?;
        let mut image = vec![0u16; IMAGE_PATH_CAPACITY];
        let mut length = u32::try_from(image.len()).expect("Windows path capacity fits DWORD");
        // SAFETY: The live process has query rights and image has the declared writable capacity.
        unsafe {
            Threading::QueryFullProcessImageNameW(
                process.raw(),
                PROCESS_NAME_WIN32,
                PWSTR(image.as_mut_ptr()),
                &mut length,
            )
        }
        .context(check_peer_error::WindowsSnafu {
            call: "QueryFullProcessImageNameW",
        })?;
        image.truncate(usize::try_from(length).expect("DWORD fits Windows usize"));
        Ok(Self {
            sid: UserSid::from_token_string(sid),
            session,
            image: PathBuf::from(OsString::from_wide(&image)),
        })
    }
    pub(crate) fn check(&self, pid: u32) -> Result<AllowedRoles, CheckPeerError> {
        let peer = Self::read(pid)?;
        if peer.sid != self.sid {
            return Err(CheckPeerError::User);
        }
        if peer.session != self.session {
            return Err(CheckPeerError::Session);
        }
        let own = std::env::current_exe().context(check_peer_error::PathSnafu)?;
        if same_image(&peer.image, &own)? {
            return Ok(AllowedRoles::Quartz);
        }
        let host = own.with_file_name("quartz-host.exe");
        // A missing host is a normal installation state before M5. Other errors remain failures.
        match same_image(&peer.image, &host) {
            Ok(true) => Ok(AllowedRoles::Host),
            Ok(false) => Err(CheckPeerError::Executable),
            Err(CheckPeerError::Path { source })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Err(CheckPeerError::Executable)
            }
            Err(error) => Err(error),
        }
    }
    pub(crate) fn check_server(&self, pid: u32) -> Result<(), CheckPeerError> {
        if self.check(pid)? == AllowedRoles::Quartz {
            Ok(())
        } else {
            Err(CheckPeerError::Executable)
        }
    }
}

fn same_image(left: &Path, right: &Path) -> Result<bool, CheckPeerError> {
    let left = std::fs::canonicalize(left).context(check_peer_error::PathSnafu)?;
    let right = std::fs::canonicalize(right).context(check_peer_error::PathSnafu)?;
    let left: Vec<_> = left.as_os_str().encode_wide().collect();
    let right: Vec<_> = right.as_os_str().encode_wide().collect();
    // SAFETY: Both slices contain live UTF-16 data; the API takes their explicit lengths.
    Ok(unsafe { CompareStringOrdinal(&left, &right, true) } == CSTR_EQUAL)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_comparison_normalizes_paths_and_ignores_case() {
        let own = std::env::current_exe().expect("test executable");
        let uppercase = PathBuf::from(own.to_string_lossy().to_uppercase());
        assert!(same_image(&own, &uppercase).expect("canonical paths"));
        assert!(
            same_image(
                &own,
                &own.parent()
                    .expect("parent")
                    .join(".")
                    .join(own.file_name().expect("file"))
            )
            .expect("dot normalization")
        );
        let other = tempfile::NamedTempFile::new().expect("different executable path");
        assert!(!same_image(&own, other.path()).expect("different paths"));
    }
}
