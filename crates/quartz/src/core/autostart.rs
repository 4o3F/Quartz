//! Windows login autostart port and exact-match command evaluation (01 §8).

use quartz_protocol::AutostartEnabled;
use snafu::Snafu;
use std::fmt;

/// CLI flag appended to the quoted executable path in the Windows Run value (01 §8).
const AUTOSTART_FLAG: &str = "--autostart";

/// Formats the exact Windows Run registry value data for `exe_path`.
#[must_use]
pub(crate) fn format_run_command(exe_path: &str) -> String {
    format!("\"{exe_path}\" {AUTOSTART_FLAG}")
}

/// Typed failures when reading or updating the Windows autostart registration.
#[derive(Debug, Snafu)]
pub(crate) enum AccessAutostartError {
    #[snafu(display("could not resolve current executable path for autostart"))]
    ResolveExecutable { source: std::io::Error },
    #[snafu(display("current executable path is not valid UTF-8"))]
    EncodeExecutablePath,
    #[snafu(display("could not read autostart registry value"))]
    ReadRegistry {
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[snafu(display("could not write autostart registry value"))]
    WriteRegistry {
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[snafu(display("could not delete autostart registry value"))]
    DeleteRegistry {
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[snafu(display("autostart registry value length exceeds platform bounds"))]
    ConvertRegistryLength { source: std::num::TryFromIntError },
    #[snafu(display("autostart registry value is not valid UTF-16"))]
    DecodeRegistryValue { source: std::string::FromUtf16Error },
}

/// Low-level access to the `Quartz` string value under `HKCU\...\Run`.
pub(crate) trait RunKeyRegistry: Send + Sync {
    /// Returns the stored `REG_SZ` string, or `None` when the key/value is absent or non-string.
    fn read_quartz_value(&self) -> Result<Option<String>, AccessAutostartError>;
    /// Creates or overwrites the `Quartz` `REG_SZ` value with `command`.
    fn write_quartz_value(&self, command: &str) -> Result<(), AccessAutostartError>;
    /// Deletes the `Quartz` value; a missing key or missing value succeeds.
    fn delete_quartz_value(&self) -> Result<(), AccessAutostartError>;
}

/// Core-facing autostart port injected via `PlatformPorts`.
pub(crate) trait AutostartPort: Send + Sync {
    /// Reads current autostart status directly from the registry.
    fn state(&self) -> Result<AutostartEnabled, AccessAutostartError>;
    /// Enables or disables autostart for the current executable and returns the new state.
    fn set(&self, desired: AutostartEnabled) -> Result<AutostartEnabled, AccessAutostartError>;
}

/// Evaluates and updates the Run key entry against the current executable path.
pub(crate) struct RunKeyAutostart<R> {
    registry: R,
    resolve_exe: Box<dyn Fn() -> Result<String, AccessAutostartError> + Send + Sync>,
}

impl<R> fmt::Debug for RunKeyAutostart<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunKeyAutostart").finish_non_exhaustive()
    }
}

impl<R> RunKeyAutostart<R> {
    /// Creates an autostart port backed by `registry` and an executable-path resolver.
    pub(crate) fn new(
        registry: R,
        resolve_exe: impl Fn() -> Result<String, AccessAutostartError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            registry,
            resolve_exe: Box::new(resolve_exe),
        }
    }
}

impl<R: RunKeyRegistry> AutostartPort for RunKeyAutostart<R> {
    fn state(&self) -> Result<AutostartEnabled, AccessAutostartError> {
        let expected = format_run_command(&(self.resolve_exe)()?);
        let actual = self.registry.read_quartz_value()?;
        Ok(if actual.as_deref() == Some(expected.as_str()) {
            AutostartEnabled::Enabled
        } else {
            AutostartEnabled::Disabled
        })
    }

    fn set(&self, desired: AutostartEnabled) -> Result<AutostartEnabled, AccessAutostartError> {
        match desired {
            AutostartEnabled::Enabled => {
                let expected = format_run_command(&(self.resolve_exe)()?);
                self.registry.write_quartz_value(&expected)?;
                Ok(AutostartEnabled::Enabled)
            }
            AutostartEnabled::Disabled => {
                self.registry.delete_quartz_value()?;
                Ok(AutostartEnabled::Disabled)
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Fault mode for the in-memory autostart registry fake.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum FakeRegistryFault {
        #[default]
        None,
        FailRead,
        FailWrite,
        FailDelete,
    }

    #[derive(Debug, Default)]
    struct FakeState {
        value: Option<String>,
        fault: FakeRegistryFault,
    }

    /// In-memory `RunKeyRegistry` fake so tests never touch the Windows registry.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct FakeRunKey {
        state: Arc<Mutex<FakeState>>,
    }

    impl FakeRunKey {
        pub(crate) fn with_value(value: Option<String>) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeState {
                    value,
                    fault: FakeRegistryFault::None,
                })),
            }
        }

        pub(crate) fn set_raw(&self, value: Option<String>) {
            self.state.lock().expect("fake registry lock").value = value;
        }

        pub(crate) fn raw(&self) -> Option<String> {
            self.state.lock().expect("fake registry lock").value.clone()
        }

        pub(crate) fn set_fault(&self, fault: FakeRegistryFault) {
            self.state.lock().expect("fake registry lock").fault = fault;
        }
    }

    impl RunKeyRegistry for FakeRunKey {
        fn read_quartz_value(&self) -> Result<Option<String>, AccessAutostartError> {
            let state = self.state.lock().expect("fake registry lock");
            if state.fault == FakeRegistryFault::FailRead {
                return Err(AccessAutostartError::ReadRegistry {
                    source: Box::new(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "simulated registry read failure",
                    )),
                });
            }
            Ok(state.value.clone())
        }

        fn write_quartz_value(&self, command: &str) -> Result<(), AccessAutostartError> {
            let mut state = self.state.lock().expect("fake registry lock");
            if state.fault == FakeRegistryFault::FailWrite {
                return Err(AccessAutostartError::WriteRegistry {
                    source: Box::new(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "simulated registry write failure",
                    )),
                });
            }
            state.value = Some(command.to_owned());
            Ok(())
        }

        fn delete_quartz_value(&self) -> Result<(), AccessAutostartError> {
            let mut state = self.state.lock().expect("fake registry lock");
            if state.fault == FakeRegistryFault::FailDelete {
                return Err(AccessAutostartError::DeleteRegistry {
                    source: Box::new(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "simulated registry delete failure",
                    )),
                });
            }
            state.value = None;
            Ok(())
        }
    }

    /// Builds an in-memory `AutostartPort` bound to a fixed executable path.
    pub(crate) fn in_memory_autostart(
        exe_path: impl Into<String>,
    ) -> (Arc<dyn AutostartPort>, FakeRunKey) {
        let exe = exe_path.into();
        let registry = FakeRunKey::default();
        let port: Arc<dyn AutostartPort> =
            Arc::new(RunKeyAutostart::new(registry.clone(), move || {
                Ok(exe.clone())
            }));
        (port, registry)
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::*, *};

    const CURRENT_EXE: &str = r"C:\Program Files\Quartz\quartz.exe";
    const STALE_EXE: &str = r"C:\Users\alice\Downloads\quartz.exe";

    #[test]
    fn registry_state_maps_exact_match_stale_path_and_missing_value() {
        let (port, registry) = in_memory_autostart(CURRENT_EXE);
        assert_eq!(
            port.state().expect("missing value"),
            AutostartEnabled::Disabled
        );

        registry.set_raw(Some(format_run_command(STALE_EXE)));
        assert_eq!(
            port.state().expect("stale path"),
            AutostartEnabled::Disabled
        );

        registry.set_raw(Some(format!("\"{CURRENT_EXE}\"")));
        assert_eq!(
            port.state().expect("missing flag"),
            AutostartEnabled::Disabled
        );

        registry.set_raw(Some(format_run_command(CURRENT_EXE)));
        assert_eq!(
            port.state().expect("exact match"),
            AutostartEnabled::Enabled
        );
    }

    #[test]
    fn enabling_autostart_writes_quoted_exe_with_flag_and_overwrites_stale_path() {
        let registry = FakeRunKey::with_value(Some(format_run_command(STALE_EXE)));
        let port = RunKeyAutostart::new(registry.clone(), || Ok(CURRENT_EXE.to_owned()));

        assert_eq!(
            port.set(AutostartEnabled::Enabled)
                .expect("enable autostart"),
            AutostartEnabled::Enabled
        );
        assert_eq!(
            registry.raw().as_deref(),
            Some(r#""C:\Program Files\Quartz\quartz.exe" --autostart"#)
        );
        assert_eq!(port.state().expect("re-read"), AutostartEnabled::Enabled);
    }

    #[test]
    fn disabling_autostart_removes_value_and_succeeds_when_missing() {
        let (port, registry) = in_memory_autostart(CURRENT_EXE);
        assert_eq!(
            port.set(AutostartEnabled::Disabled)
                .expect("disable absent value"),
            AutostartEnabled::Disabled
        );
        assert_eq!(registry.raw(), None);

        port.set(AutostartEnabled::Enabled).expect("enable");
        assert!(registry.raw().is_some());
        assert_eq!(
            port.set(AutostartEnabled::Disabled)
                .expect("disable present value"),
            AutostartEnabled::Disabled
        );
        assert_eq!(registry.raw(), None);
    }

    #[test]
    fn registry_read_failure_returns_typed_error_instead_of_disabled() {
        let (port, registry) = in_memory_autostart(CURRENT_EXE);
        registry.set_fault(FakeRegistryFault::FailRead);
        assert!(matches!(
            port.state(),
            Err(AccessAutostartError::ReadRegistry { .. })
        ));
    }
}
