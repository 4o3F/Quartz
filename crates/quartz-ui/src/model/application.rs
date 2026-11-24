//! Tracked applications.

/// Placeholder glyph standing in for an application icon (11 section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Glyph {
    #[cfg_attr(
        all(not(test), not(feature = "preview")),
        expect(dead_code, reason = "used by preview fixtures and their model tests")
    )]
    Editor,
    #[cfg_attr(
        all(not(test), not(feature = "preview")),
        expect(dead_code, reason = "used by preview fixtures and their model tests")
    )]
    Browser,
    #[cfg_attr(
        all(not(test), not(feature = "preview")),
        expect(dead_code, reason = "used by preview fixtures and their model tests")
    )]
    Chat,
    #[cfg_attr(
        all(not(test), not(feature = "preview")),
        expect(dead_code, reason = "used by preview fixtures and their model tests")
    )]
    Messenger,
    Terminal,
}

/// Identifies an application within one day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct AppId(u16);

impl AppId {
    /// Wraps an application id from the data source.
    #[must_use]
    pub(crate) const fn new(id: u16) -> Self {
        Self(id)
    }
}

/// A tracked application.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AppInfo {
    pub(crate) id: AppId,
    pub(crate) name: String,
    /// Short name used where space is tight, such as swimlane labels.
    pub(crate) short_name: String,
    pub(crate) exe: String,
    pub(crate) glyph: Glyph,
}
