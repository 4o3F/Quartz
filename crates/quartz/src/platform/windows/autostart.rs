//! Windows `HKCU\...\Run` registry backing for login autostart (01 §8).

use super::WideString;
use crate::core::autostart::{
    AccessAutostartError, AutostartPort, RunKeyAutostart, RunKeyRegistry,
};
use std::sync::Arc;
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR},
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
            REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW,
            RegQueryValueExW, RegSetValueExW,
        },
    },
    core::{HRESULT, PCWSTR},
};

/// Current-user Windows startup registry key (01 §8).
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Value name owned by Quartz inside [`RUN_SUBKEY`].
const AUTOSTART_VALUE_NAME: &str = "Quartz";
/// Retry once if the value grows between the size query and data read.
const MAX_QUERY_ATTEMPTS: usize = 2;
/// Byte width of one UTF-16 code unit.
const UTF16_UNIT_BYTES: usize = size_of::<u16>();

fn win32_source(code: WIN32_ERROR) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(windows::core::Error::from_hresult(HRESULT::from_win32(
        code.0,
    )))
}

fn current_exe_utf8() -> Result<String, AccessAutostartError> {
    let path = std::env::current_exe()
        .map_err(|source| AccessAutostartError::ResolveExecutable { source })?;
    path.into_os_string()
        .into_string()
        .map_err(|_| AccessAutostartError::EncodeExecutablePath)
}

/// Closes a uniquely owned Win32 registry key handle on drop.
#[derive(Debug)]
struct OwnedRegKey(HKEY);

impl OwnedRegKey {
    fn raw(&self) -> HKEY {
        self.0
    }
}

impl Drop for OwnedRegKey {
    fn drop(&mut self) {
        // SAFETY: Construction transfers unique ownership of a valid HKEY opened by RegOpenKeyExW
        // or RegCreateKeyExW. Best-effort teardown: Drop cannot propagate close failures.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

#[derive(Debug, Clone, Copy)]
struct WindowsRunKey;

impl RunKeyRegistry for WindowsRunKey {
    fn read_quartz_value(&self) -> Result<Option<String>, AccessAutostartError> {
        let subkey = WideString::new(RUN_SUBKEY);
        let value_name = WideString::new(AUTOSTART_VALUE_NAME);
        let mut raw_key = HKEY::default();
        // SAFETY: `subkey` is NUL-terminated UTF-16 and `raw_key` is writable HKEY storage.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                None,
                KEY_READ,
                &mut raw_key,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            return Err(AccessAutostartError::ReadRegistry {
                source: win32_source(status),
            });
        }
        let key = OwnedRegKey(raw_key);

        for _ in 0..MAX_QUERY_ATTEMPTS {
            let mut value_type = REG_VALUE_TYPE::default();
            let mut byte_len: u32 = 0;
            // SAFETY: `key` is a valid open HKEY, `value_name` is NUL-terminated UTF-16, and
            // passing `lpdata = None` queries only the required byte length and type.
            let status = unsafe {
                RegQueryValueExW(
                    key.raw(),
                    PCWSTR(value_name.as_ptr()),
                    None,
                    Some(&mut value_type),
                    None,
                    Some(&mut byte_len),
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if status != ERROR_SUCCESS {
                return Err(AccessAutostartError::ReadRegistry {
                    source: win32_source(status),
                });
            }
            if value_type != REG_SZ {
                return Ok(None);
            }
            let capacity = usize::try_from(byte_len)
                .map_err(|source| AccessAutostartError::ConvertRegistryLength { source })?;
            if capacity == 0 {
                return Ok(Some(String::new()));
            }
            let mut buffer = vec![0u8; capacity];
            // SAFETY: `buffer` has `byte_len` writable bytes and `key`/`value_name` remain valid
            // for the duration of the synchronous call.
            let status = unsafe {
                RegQueryValueExW(
                    key.raw(),
                    PCWSTR(value_name.as_ptr()),
                    None,
                    Some(&mut value_type),
                    Some(buffer.as_mut_ptr()),
                    Some(&mut byte_len),
                )
            };
            if status == ERROR_MORE_DATA {
                continue;
            }
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if status != ERROR_SUCCESS {
                return Err(AccessAutostartError::ReadRegistry {
                    source: win32_source(status),
                });
            }
            if value_type != REG_SZ {
                return Ok(None);
            }
            let actual = usize::try_from(byte_len)
                .map_err(|source| AccessAutostartError::ConvertRegistryLength { source })?;
            let Some(data) = buffer.get(..actual) else {
                return Err(AccessAutostartError::ReadRegistry {
                    source: win32_source(ERROR_MORE_DATA),
                });
            };
            let (pairs, rest) = data.as_chunks::<UTF16_UNIT_BYTES>();
            if !rest.is_empty() {
                return Ok(None);
            }
            let mut units: Vec<u16> = pairs.iter().copied().map(u16::from_le_bytes).collect();
            while units.last() == Some(&0) {
                units.pop();
            }
            let text = String::from_utf16(&units)
                .map_err(|source| AccessAutostartError::DecodeRegistryValue { source })?;
            return Ok(Some(text));
        }

        Err(AccessAutostartError::ReadRegistry {
            source: win32_source(ERROR_MORE_DATA),
        })
    }

    fn write_quartz_value(&self, command: &str) -> Result<(), AccessAutostartError> {
        let subkey = WideString::new(RUN_SUBKEY);
        let value_name = WideString::new(AUTOSTART_VALUE_NAME);
        let mut raw_key = HKEY::default();
        // SAFETY: `subkey` is NUL-terminated UTF-16 and `raw_key` receives the opened/created key.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut raw_key,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(AccessAutostartError::WriteRegistry {
                source: win32_source(status),
            });
        }
        let key = OwnedRegKey(raw_key);
        let bytes: Vec<u8> = command
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: `key` is a valid open HKEY with KEY_SET_VALUE, `value_name` is NUL-terminated
        // UTF-16, and `bytes` holds the NUL-terminated UTF-16LE REG_SZ payload.
        let status = unsafe {
            RegSetValueExW(
                key.raw(),
                PCWSTR(value_name.as_ptr()),
                None,
                REG_SZ,
                Some(&bytes),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(AccessAutostartError::WriteRegistry {
                source: win32_source(status),
            });
        }
        Ok(())
    }

    fn delete_quartz_value(&self) -> Result<(), AccessAutostartError> {
        let subkey = WideString::new(RUN_SUBKEY);
        let value_name = WideString::new(AUTOSTART_VALUE_NAME);
        let mut raw_key = HKEY::default();
        // SAFETY: `subkey` is NUL-terminated UTF-16 and `raw_key` receives the opened key.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                None,
                KEY_SET_VALUE,
                &mut raw_key,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        if status != ERROR_SUCCESS {
            return Err(AccessAutostartError::DeleteRegistry {
                source: win32_source(status),
            });
        }
        let key = OwnedRegKey(raw_key);
        // SAFETY: `key` is a valid open HKEY with KEY_SET_VALUE and `value_name` is NUL-terminated UTF-16.
        let status = unsafe { RegDeleteValueW(key.raw(), PCWSTR(value_name.as_ptr())) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        Err(AccessAutostartError::DeleteRegistry {
            source: win32_source(status),
        })
    }
}

/// Returns the production Windows autostart port backed by `HKCU\...\Run`.
pub(super) fn autostart_port() -> Arc<dyn AutostartPort> {
    Arc::new(RunKeyAutostart::new(WindowsRunKey, current_exe_utf8))
}
