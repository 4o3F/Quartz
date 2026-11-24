//! Main-thread event hooks and capture-thread process identity resolution.

mod apartment;
mod desktop;
mod frame_aumid;
mod hooks;
mod identity;
mod version;
use apartment::CaptureApartment;
pub(super) use hooks::{ForegroundCommand, ForegroundPort, HookHealth, ObserveForegroundError};
use identity::Resolver;

mod window;
use window::Window;

/// Creates and releases the resolver's COM resources on the calling capture thread.
pub(super) fn create_resolver() -> Result<
    Box<dyn crate::core::foreground::ForegroundResolve>,
    Box<dyn std::error::Error + Send + Sync>,
> {
    let apartment = CaptureApartment::initialize()?;
    Ok(Box::new(CaptureResolver {
        resolver: Resolver::new(),
        _apartment: apartment,
    }))
}

struct CaptureResolver {
    resolver: Resolver,
    // Fields are dropped in declaration order, releasing interfaces before COM.
    _apartment: CaptureApartment,
}
impl crate::core::foreground::ForegroundResolve for CaptureResolver {
    fn resolve(
        &mut self,
        snapshot: crate::core::foreground::Snapshot,
    ) -> crate::core::foreground::Resolution {
        self.resolver.resolve(snapshot)
    }
}
