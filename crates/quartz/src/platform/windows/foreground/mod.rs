//! Main-thread event hooks and capture-thread process identity resolution.

mod apartment;
mod desktop;
mod frame_aumid;
mod hooks;
mod identity;
mod version;
pub(crate) use apartment::{CaptureApartment, InitializeCaptureComError};
pub(super) use hooks::{ForegroundCommand, ForegroundPort, ObserveForegroundError};
pub(crate) use identity::{Resolution, Resolver};

mod window;
use window::Window;
