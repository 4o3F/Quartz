//! Locale-aware text: typed message ids and the formatter that fills them (10 section 6.1).

mod formatter;
mod message;

pub(crate) use formatter::{DurationParts, Formatter, SourceText};
pub(crate) use message::{Arg, Msg};
