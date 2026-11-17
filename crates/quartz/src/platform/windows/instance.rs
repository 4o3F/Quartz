//! Per-session discovery and cross-session exclusion for the core.

use std::{
    fs::{File, OpenOptions},
    os::windows::io::AsRawHandle,
};

use snafu::{ResultExt, Snafu};
use windows::{
    Win32::{
        Foundation::{
            self, ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER, ERROR_LOCK_VIOLATION, HANDLE,
        },
        Security::{self, TOKEN_QUERY, TOKEN_USER, TokenUser},
        Storage::FileSystem::{
            self, FILE_SHARE_READ, FILE_SHARE_WRITE, LOCKFILE_EXCLUSIVE_LOCK,
            LOCKFILE_FAIL_IMMEDIATELY,
        },
        System::{IO::OVERLAPPED, Threading},
    },
    core::{HRESULT, PCWSTR},
};

use super::{handle::OwnedHandle, wide};
use crate::{data_directory::DataDirectory, instance_name::UserSid};

/// Failures while identifying the current user and acquiring the core instance guards.
#[derive(Debug, Snafu)]
pub(crate) enum AcquireInstanceError {
    #[snafu(display("could not open current process token (OpenProcessToken)"))]
    OpenProcessToken { source: windows::core::Error },
    #[snafu(display("could not read current token user (GetTokenInformation)"))]
    GetTokenInformation { source: windows::core::Error },
    #[snafu(display("could not create core session mutex (CreateMutexW)"))]
    CreateMutex { source: windows::core::Error },
    #[snafu(display("could not open quartz.lock (OpenOptions::open)"))]
    OpenLockFile { source: std::io::Error },
    #[snafu(display("could not lock data directory (LockFileEx)"))]
    LockFile { source: windows::core::Error },
}

/// Keeps the session mutex and byte-range lock alive until core shutdown.
#[derive(Debug)]
pub(crate) struct CoreInstance {
    // Release the file lock before making the session mutex available again.
    _lock: File,
    _mutex: OwnedHandle,
}

/// Normal startup outcomes, distinct from operating-system failures.
#[derive(Debug)]
pub(crate) enum AcquireInstance {
    Acquired(CoreInstance),
    SameSession,
    OtherSession,
}

impl DataDirectory {
    /// Acquires the current user's session mutex and data directory lock.
    ///
    /// # Errors
    /// Returns token lookup, mutex creation, lock-file open, or file-lock failures.
    pub(crate) fn acquire_instance(&self) -> Result<AcquireInstance, AcquireInstanceError> {
        use std::os::windows::fs::OpenOptionsExt;

        let name = wide(&UserSid::read()?.core_mutex_name());
        // SAFETY: The mutex name is NUL-terminated; default security and no initial ownership are used.
        let mutex = unsafe { Threading::CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
            .context(CreateMutexSnafu)?;
        // SAFETY: GetLastError must be read immediately after the successful CreateMutexW call.
        let already_exists = unsafe { Foundation::GetLastError() } == ERROR_ALREADY_EXISTS;
        // SAFETY: CreateMutexW succeeded and transferred a uniquely owned closable handle.
        let mutex = unsafe { OwnedHandle::from_raw(mutex) };
        if already_exists {
            return Ok(AcquireInstance::SameSession);
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            // Sharing permits LockFileEx to report contention; disallow deleting the locked file.
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
            .open(self.lock_file())
            .context(OpenLockFileSnafu)?;
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: The file handle and zero-offset OVERLAPPED are valid for this synchronous call.
        // Closing the owned File releases its one-byte exclusive lock, including after a crash.
        match unsafe {
            FileSystem::LockFileEx(
                HANDLE(lock.as_raw_handle()),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                None,
                1,
                0,
                &mut overlapped,
            )
        } {
            Ok(()) => Ok(AcquireInstance::Acquired(CoreInstance {
                _lock: lock,
                _mutex: mutex,
            })),
            Err(error) if error.code() == HRESULT::from_win32(ERROR_LOCK_VIOLATION.0) => {
                Ok(AcquireInstance::OtherSession)
            }
            Err(source) => Err(source).context(LockFileSnafu),
        }
    }
}

impl UserSid {
    fn read() -> Result<Self, AcquireInstanceError> {
        let mut token = HANDLE::default();
        // SAFETY: The pseudo-handle names this process and token points to writable handle storage.
        unsafe {
            Threading::OpenProcessToken(Threading::GetCurrentProcess(), TOKEN_QUERY, &mut token)
        }
        .context(OpenProcessTokenSnafu)?;
        // SAFETY: OpenProcessToken returned a uniquely owned closable token handle.
        let token = unsafe { OwnedHandle::from_raw(token) };
        let mut bytes = 0;
        // SAFETY: A null buffer with size zero queries the required TOKEN_USER allocation size.
        let size_result =
            unsafe { Security::GetTokenInformation(token.raw(), TokenUser, None, 0, &mut bytes) };
        if let Err(source) = size_result
            && source.code() != HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0)
        {
            return Err(source).context(GetTokenInformationSnafu);
        }
        // usize storage supplies alignment for TOKEN_USER and the SID inside the returned buffer.
        let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        // SAFETY: The buffer is aligned and has at least the byte length queried above.
        unsafe {
            Security::GetTokenInformation(
                token.raw(),
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                bytes,
                &mut bytes,
            )
        }
        .context(GetTokenInformationSnafu)?;
        // SAFETY: GetTokenInformation initialized TOKEN_USER and its complete SID in this live buffer.
        // SID accessors return valid fields; the subauthority indices stay below its reported count.
        let (revision, authority, subauthorities) = unsafe {
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
            let sid = user.User.Sid;
            let revision = *sid.0.cast::<u8>();
            let authority = (*Security::GetSidIdentifierAuthority(sid)).Value;
            let count = *Security::GetSidSubAuthorityCount(sid);
            let subauthorities: Vec<_> = (0..u32::from(count))
                .map(|index| *Security::GetSidSubAuthority(sid, index))
                .collect();
            (revision, authority, subauthorities)
        };
        Ok(Self::from_components(revision, authority, &subauthorities))
    }
}
