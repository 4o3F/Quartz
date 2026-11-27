//! Windows HKCU registry backing for browser Native Messaging hosts (05 §2.1).

use std::path::PathBuf;
use std::sync::Arc;

use quartz_protocol::BrowserKind;
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR},
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
            REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegOpenKeyExW,
            RegQueryValueExW, RegSetValueExW,
        },
    },
    core::{HRESULT, PCWSTR},
};

use super::WideString;
use crate::core::browser_hosts::{
    BrowserHostRegistry, BrowserHostsPort, ManageBrowserHostsError, NativeHostManager,
    RegistryHostEntry, StdBrowserHostFs, registry_subkey, resolve_host_executable,
};

/// Retry once if the default value grows between the size query and data read.
const MAX_QUERY_ATTEMPTS: usize = 2;

/// Byte width of one UTF-16 code unit.
const UTF16_UNIT_BYTES: usize = size_of::<u16>();

fn win32_source(code: WIN32_ERROR) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(windows::core::Error::from_hresult(HRESULT::from_win32(
        code.0,
    )))
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
struct WindowsBrowserHostRegistry;

impl BrowserHostRegistry for WindowsBrowserHostRegistry {
    fn read_default_value(
        &self,
        browser: BrowserKind,
    ) -> Result<RegistryHostEntry, ManageBrowserHostsError> {
        let subkey = WideString::new(registry_subkey(browser));
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
            return Ok(RegistryHostEntry::Absent);
        }
        if status != ERROR_SUCCESS {
            return Err(ManageBrowserHostsError::ReadHostRegistry {
                browser,
                source: win32_source(status),
            });
        }
        let key = OwnedRegKey(raw_key);

        for _ in 0..MAX_QUERY_ATTEMPTS {
            let mut value_type = REG_VALUE_TYPE::default();
            let mut byte_len: u32 = 0;
            // SAFETY: `key` is a valid open HKEY, `PCWSTR::null()` selects the key's default value,
            // and passing `lpdata = None` queries only the required byte length and type.
            let status = unsafe {
                RegQueryValueExW(
                    key.raw(),
                    PCWSTR::null(),
                    None,
                    Some(&mut value_type),
                    None,
                    Some(&mut byte_len),
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(RegistryHostEntry::WithoutStringValue);
            }
            if status != ERROR_SUCCESS {
                return Err(ManageBrowserHostsError::ReadHostRegistry {
                    browser,
                    source: win32_source(status),
                });
            }
            if value_type != REG_SZ {
                return Ok(RegistryHostEntry::WithoutStringValue);
            }
            let capacity = usize::try_from(byte_len).map_err(|source| {
                ManageBrowserHostsError::ConvertHostRegistryLength { browser, source }
            })?;
            if capacity == 0 {
                return Ok(RegistryHostEntry::Value(String::new()));
            }
            let mut buffer = vec![0u8; capacity];
            // SAFETY: `buffer` has `byte_len` writable bytes and `key` remains valid for the
            // duration of the synchronous call; `PCWSTR::null()` selects the default value.
            let status = unsafe {
                RegQueryValueExW(
                    key.raw(),
                    PCWSTR::null(),
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
                return Ok(RegistryHostEntry::WithoutStringValue);
            }
            if status != ERROR_SUCCESS {
                return Err(ManageBrowserHostsError::ReadHostRegistry {
                    browser,
                    source: win32_source(status),
                });
            }
            if value_type != REG_SZ {
                return Ok(RegistryHostEntry::WithoutStringValue);
            }
            let actual = usize::try_from(byte_len).map_err(|source| {
                ManageBrowserHostsError::ConvertHostRegistryLength { browser, source }
            })?;
            let Some(data) = buffer.get(..actual) else {
                return Err(ManageBrowserHostsError::ReadHostRegistry {
                    browser,
                    source: win32_source(ERROR_MORE_DATA),
                });
            };
            let (pairs, rest) = data.as_chunks::<UTF16_UNIT_BYTES>();
            if !rest.is_empty() {
                return Ok(RegistryHostEntry::WithoutStringValue);
            }
            let mut units: Vec<u16> = pairs.iter().copied().map(u16::from_le_bytes).collect();
            while units.last() == Some(&0) {
                units.pop();
            }
            let text = String::from_utf16(&units).map_err(|source| {
                ManageBrowserHostsError::DecodeHostRegistryValue { browser, source }
            })?;
            return Ok(RegistryHostEntry::Value(text));
        }

        Err(ManageBrowserHostsError::ReadHostRegistry {
            browser,
            source: win32_source(ERROR_MORE_DATA),
        })
    }

    fn write_default_value(
        &self,
        browser: BrowserKind,
        manifest_path: &str,
    ) -> Result<(), ManageBrowserHostsError> {
        let subkey = WideString::new(registry_subkey(browser));
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
            return Err(ManageBrowserHostsError::WriteHostRegistry {
                browser,
                source: win32_source(status),
            });
        }
        let key = OwnedRegKey(raw_key);
        let bytes: Vec<u8> = manifest_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: `key` is a valid open HKEY with KEY_SET_VALUE, `PCWSTR::null()` selects the
        // default value, and `bytes` holds the NUL-terminated UTF-16LE REG_SZ payload.
        let status =
            unsafe { RegSetValueExW(key.raw(), PCWSTR::null(), None, REG_SZ, Some(&bytes)) };
        if status != ERROR_SUCCESS {
            return Err(ManageBrowserHostsError::WriteHostRegistry {
                browser,
                source: win32_source(status),
            });
        }
        Ok(())
    }

    fn delete_host_key(&self, browser: BrowserKind) -> Result<(), ManageBrowserHostsError> {
        let subkey = WideString::new(registry_subkey(browser));
        // SAFETY: `subkey` is NUL-terminated UTF-16 naming the HKCU subkey to delete.
        let status = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(subkey.as_ptr())) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        Err(ManageBrowserHostsError::DeleteHostRegistry {
            browser,
            source: win32_source(status),
        })
    }
}

/// Returns the production Windows Native Messaging host port backed by HKCU and `manifests_dir`.
pub(super) fn browser_hosts_port(manifests_dir: PathBuf) -> Arc<dyn BrowserHostsPort> {
    Arc::new(NativeHostManager::new(
        WindowsBrowserHostRegistry,
        StdBrowserHostFs,
        manifests_dir,
        resolve_host_executable,
    ))
}
