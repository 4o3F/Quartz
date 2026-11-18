//! Capture limits fixed by the design, shared by the core and its shells.

use std::time::Duration;

/// Confirmation interval, capture 02 §4.3.
pub const CHECKPOINT_PERIOD: Duration = Duration::from_secs(59);
/// Maximum review interval, capture 02 §1.1.
pub const REVIEW_PERIOD: Duration = Duration::from_secs(60);
/// Transaction batching allowance, data model 03 §4.2.
pub const BATCH_WAIT: Duration = Duration::from_secs(1);
/// Clock-segment drift tolerance, capture 02 §5.
pub const CLOCK_DRIFT: Duration = Duration::from_secs(2);
/// Title cap before privacy evaluation, capture 02 §1.1.
pub const TITLE_UTF16_UNITS: usize = 4_096;
/// Idle recovery polling interval, capture 02 §3.1.
pub const IDLE_POLL: Duration = Duration::from_secs(5);
/// Default idle threshold, capture 02 §3.1.
pub const IDLE_THRESHOLD: Duration = Duration::from_secs(180);
/// Changes exceeding this count indicate a title storm, capture 02 §1.3.
pub const TITLE_STORM_COUNT: usize = 50;
/// Per-instance title-storm window, capture 02 §1.3.
pub const TITLE_STORM_WINDOW: Duration = Duration::from_secs(10);
/// Application and audible grant cap, capture 02 §3.2.
pub const EXCEPTION_CAP: Duration = Duration::from_secs(7_200);
/// Failure buffer retention, architecture 01 §6.
pub const BACKLOG_RETENTION: Duration = Duration::from_secs(600);
/// Main-thread observation capacity, architecture 01 §6.
pub const MAIN_QUEUE_CAPACITY: usize = 1_024;
/// Database command capacity, architecture 01 §6.
pub const WRITE_QUEUE_CAPACITY: usize = 4_096;
/// Browser privacy-context wait, privacy 06 §5.
pub const PRIVACY_CONTEXT_WAIT: Duration = Duration::from_secs(2);
/// UTC storage-day length, capture 02 §4.2.
pub const UTC_DAY: Duration = Duration::from_secs(86_400);
