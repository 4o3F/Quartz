//! Main-thread event hooks and capture-thread process identity resolution.

mod desktop;
mod hooks;
mod identity;
mod version;
pub(super) use hooks::{ForegroundCommand, ForegroundPort, ObserveForegroundError};
pub(crate) use identity::{Resolution, Resolver};

mod window;
use window::Window;
