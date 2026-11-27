//! Locale-aware text: typed message ids and the formatter that fills them (10 section 6.1).

mod bytes;
mod formatter;
mod message;

pub(crate) use formatter::Formatter;
#[cfg(windows)]
pub(crate) use formatter::{DurationParts, SourceText};
pub(crate) use message::{Arg, Msg};
