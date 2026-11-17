//! Native Messaging host launched by browsers with browser-defined arguments.
//!
//! Chrome supplies an origin and optional --parent-window=N; Firefox supplies a manifest path and extension ID.

fn main() -> std::process::ExitCode {
    // TODO(M5): forward length-prefixed stdio messages to the core named pipe (05 §2).
    std::process::ExitCode::FAILURE
}
