//! Platform-independent foreground filtering and metadata selection.

use quartz_domain::capture::{HookTarget, ProcessId, SystemSignal, Title, WindowHandle, limits};
use std::collections::VecDeque;

/// Bounded process metadata retention, capture 02 §2.1.
pub(crate) const PROCESS_CACHE_CAPACITY: usize = 128;
/// Bounded application display-name retention, capture 02 §2.1.
pub(crate) const DISPLAY_CACHE_CAPACITY: usize = 256;
/// UTF-16 snapshot including the terminator, capture 02 §1.1.
pub(crate) const TITLE_BUFFER_UNITS: usize = limits::TITLE_UTF16_UNITS + 1;
/// Win32 OBJID_WINDOW and CHILDID_SELF, capture 02 §1.1.
pub(crate) const WINDOW_OBJECT: i32 = 0;
pub(crate) const SELF_CHILD: i32 = 0;
/// en-US language identifier, i18n 10 §5.
const ENGLISH_US: u16 = 0x0409;
/// UTF-16 leading-surrogate range; a trailing unit may lie beyond the title cap.
const LEADING_SURROGATES: std::ops::RangeInclusive<u16> = 0xD800..=0xDBFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventKind {
    Foreground,
    Title,
    Destroy,
    Review,
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct NativeEvent {
    pub kind: EventKind,
    pub window: WindowHandle,
    pub object: i32,
    pub child: i32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventDecision {
    Accept,
    Ignore,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RebindDecision {
    Accept,
    ReadCurrent,
}
impl NativeEvent {
    pub(crate) fn accepts(self, foreground: WindowHandle) -> EventDecision {
        match self.kind {
            EventKind::Title if self.window != foreground => EventDecision::Ignore,
            EventKind::Title | EventKind::Destroy
                if self.object != WINDOW_OBJECT || self.child != SELF_CHILD =>
            {
                EventDecision::Ignore
            }
            EventKind::Title | EventKind::Destroy | EventKind::Foreground | EventKind::Review => {
                EventDecision::Accept
            }
        }
    }
}
/// Why the main thread reads current state; only the existing fallback timer can count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadCause {
    Trigger,
    Fallback,
}
/// Completed read evidence; the fallback tick shares dwmsEventTime's time base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadSample {
    Trigger,
    Fallback { read_tick: u32 },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CurrentForeground {
    pub desktop: Desktop,
    pub window: WindowHandle,
    pub title: Title,
    pub affinity: DisplayAffinity,
}
impl CurrentForeground {
    pub(crate) fn observable_window(&self) -> Option<WindowHandle> {
        (self.desktop == Desktop::Observable && self.window.get() != 0).then_some(self.window)
    }
    pub(crate) fn accepts_rebind(&self, target: HookTarget, owner: ProcessId) -> RebindDecision {
        let matches = match target {
            HookTarget::Process { window, process } => {
                self.desktop == Desktop::Observable && self.window == window && owner == process
            }
            HookTarget::Unbind => self.observable_window().is_none(),
        };
        if matches {
            RebindDecision::Accept
        } else {
            RebindDecision::ReadCurrent
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CoverageChange {
    pub previous_window: Option<WindowHandle>,
    pub reviewed_window: Option<WindowHandle>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadDecision {
    Unchanged,
    Changed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadOutcome {
    pub decision: ReadDecision,
    pub untriggered: Option<CoverageChange>,
}
/// Half a wrapping u32 millisecond cycle; ordering evidence is valid within this range.
const NATIVE_TICK_HALF_RANGE: u32 = 1 << (u32::BITS - 1);
#[derive(Debug)]
struct PendingChange {
    change: CoverageChange,
    read_tick: u32,
}
#[derive(Debug, Default)]
enum TriggerHistory {
    #[default]
    None,
    Received,
}
/// Sans-IO baseline and notification coverage decisions, capture 02 §1–§2.
#[derive(Debug, Default)]
pub(crate) struct ForegroundReads {
    baseline: Option<CurrentForeground>,
    triggers: TriggerHistory,
    // Shutdown may discard this unresolved diagnostic; its observation was already emitted.
    pending: Option<PendingChange>,
}
impl ForegroundReads {
    pub(crate) fn trigger(&mut self) {
        self.triggers = TriggerHistory::Received;
    }
    pub(crate) fn invalidate(&mut self) {
        self.baseline = None;
        self.trigger();
    }
    pub(crate) fn failed_read(&mut self, cause: ReadCause) {
        if cause == ReadCause::Trigger {
            self.trigger();
        }
    }
    pub(crate) fn accept_delivery<E>(&mut self, result: Result<(), E>) -> Result<(), E> {
        result.inspect_err(|_| self.invalidate())
    }
    pub(crate) fn retire(&mut self, window: WindowHandle) {
        if self
            .baseline
            .as_ref()
            .is_some_and(|state| state.window == window)
        {
            self.invalidate();
        }
    }
    pub(crate) fn native_event(&mut self, event_time_ms: u32) -> Option<CoverageChange> {
        let pending = self.pending.take()?;
        if pending.read_tick.wrapping_sub(event_time_ms) < NATIVE_TICK_HALF_RANGE {
            None
        } else {
            Some(pending.change)
        }
    }
    pub(crate) fn observe(
        &mut self,
        sample: ReadSample,
        current: &CurrentForeground,
    ) -> ReadOutcome {
        let untriggered = match sample {
            ReadSample::Fallback { .. } => self.pending.take().map(|pending| pending.change),
            ReadSample::Trigger => {
                self.trigger();
                None
            }
        };
        let decision = if self.baseline.as_ref() == Some(current) {
            ReadDecision::Unchanged
        } else {
            if let ReadSample::Fallback { read_tick } = sample
                && matches!(self.triggers, TriggerHistory::None)
                && let Some(previous) = &self.baseline
            {
                self.pending = Some(PendingChange {
                    change: CoverageChange {
                        previous_window: previous.observable_window(),
                        reviewed_window: current.observable_window(),
                    },
                    read_tick,
                });
            }
            self.baseline = Some(current.clone());
            ReadDecision::Changed
        };
        self.triggers = TriggerHistory::None;
        ReadOutcome {
            decision,
            untriggered,
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum TitleRead {
    Copied(usize),
    Failed,
}
impl TitleRead {
    pub(crate) fn snapshot(self, buffer: &[u16]) -> Title {
        let Self::Copied(count) = self else {
            return Title::unreadable();
        };
        let Some(units) = buffer.get(..count) else {
            return Title::unreadable();
        };
        // A saturated GetWindowTextW result cannot distinguish exact-cap text from longer text.
        // Do not turn a split surrogate at the cap into a replacement character.
        let units = if count == limits::TITLE_UTF16_UNITS
            && units
                .last()
                .is_some_and(|unit| LEADING_SURROGATES.contains(unit))
        {
            &units[..count - 1]
        } else {
            units
        };
        let Ok(text) = String::from_utf16(units) else {
            return Title::unreadable();
        };
        if count >= limits::TITLE_UTF16_UNITS {
            Title::truncated(&text)
        } else {
            Title::readable(&text)
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Visibility {
    Visible,
    Hidden,
}
#[derive(Debug)]
pub(crate) struct ChildWindow {
    pub window: WindowHandle,
    pub process: ProcessId,
    pub class: String,
    pub visibility: Visibility,
}
impl ChildWindow {
    pub(crate) fn content(children: &[Self], host: ProcessId) -> Option<&Self> {
        children.iter().find(|child| {
            child.process != host
                && child.visibility == Visibility::Visible
                && child.class == "Windows.UI.Core.CoreWindow"
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisplayAffinity {
    None,
    Monitor,
    Exclude,
    Unavailable,
}
impl DisplayAffinity {
    pub(crate) fn from_raw(value: Option<u32>) -> Self {
        /// WDA values from winuser.h, privacy 06 §3.2.
        const WDA_NONE: u32 = 0;
        const WDA_MONITOR: u32 = 1;
        const WDA_EXCLUDE_FROM_CAPTURE: u32 = 0x11;
        match value {
            Some(WDA_NONE) => Self::None,
            Some(WDA_MONITOR) => Self::Monitor,
            Some(WDA_EXCLUDE_FROM_CAPTURE) => Self::Exclude,
            Some(_) | None => Self::Unavailable,
        }
    }
    pub(crate) fn signal(self) -> SystemSignal {
        match self {
            Self::None => SystemSignal::Absent,
            Self::Monitor | Self::Exclude => SystemSignal::Present,
            Self::Unavailable => SystemSignal::Unknown,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct Translation {
    pub language: u16,
    pub codepage: u16,
}
impl Translation {
    pub(crate) fn select(available: &[Translation], preferred: &[u16]) -> Option<Translation> {
        preferred
            .iter()
            .chain(std::iter::once(&ENGLISH_US))
            .find_map(|language| available.iter().find(|entry| entry.language == *language))
            .or_else(|| available.first())
            .copied()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessKey {
    pub process: ProcessId,
    pub created: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheRetention {
    Keep,
    Evict,
}
#[derive(Debug)]
pub(crate) struct BoundedCache<K, V> {
    entries: VecDeque<CacheEntry<K, V>>,
    capacity: usize,
}
#[derive(Debug)]
struct CacheEntry<K, V> {
    key: K,
    value: V,
}
impl<K: PartialEq, V> BoundedCache<K, V> {
    pub(crate) fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "cache capacity must be positive");
        Self {
            entries: VecDeque::new(),
            capacity,
        }
    }
    pub(crate) fn find(&mut self, key: &K) -> Option<&V> {
        let index = self.entries.iter().position(|entry| entry.key == *key)?;
        let entry = self
            .entries
            .remove(index)
            .expect("located cache entry exists");
        self.entries.push_back(entry);
        Some(
            &self
                .entries
                .back()
                .expect("touched cache entry remains present")
                .value,
        )
    }
    pub(crate) fn insert(&mut self, key: K, value: V) {
        self.entries.retain(|entry| entry.key != key);
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(CacheEntry { key, value });
    }
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> CacheRetention) {
        self.entries
            .retain(|entry| match keep(&entry.key, &entry.value) {
                CacheRetention::Keep => true,
                CacheRetention::Evict => false,
            });
    }
}

impl<V> BoundedCache<ProcessKey, V> {
    pub(crate) fn retain_lifetime(&mut self, current: ProcessKey) {
        self.retain(|key, _| {
            if key.process != current.process || *key == current {
                CacheRetention::Keep
            } else {
                CacheRetention::Evict
            }
        });
    }
    pub(crate) fn evict_process(&mut self, process: ProcessId) {
        self.retain(|key, _| {
            if key.process != process {
                CacheRetention::Keep
            } else {
                CacheRetention::Evict
            }
        });
    }
}
#[derive(Debug)]
pub(crate) struct Snapshot {
    pub at: quartz_domain::capture::Timestamp,
    pub read_at: quartz_domain::capture::Timestamp,
    pub event_time_ms: Option<u32>,
    pub source: quartz_domain::capture::ObservationSource,
    pub kind: EventKind,
    pub window: WindowHandle,
    pub title: Title,
    pub desktop: Desktop,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Desktop {
    Observable,
    Unknown,
}

impl Desktop {
    pub(crate) fn compare_names(input: &[u16], current: &[u16]) -> Self {
        if !input.is_empty() && input == current {
            Self::Observable
        } else {
            Self::Unknown
        }
    }
}

/// Resolves a main-thread snapshot on the capture thread.
pub(crate) trait ForegroundResolve {
    fn resolve(&mut self, snapshot: Snapshot) -> Resolution;
}

pub(crate) enum Resolution {
    Event(crate::core::app::ObservedEvent),
    Review,
}

/// The returned resolver may own thread-local resources such as a COM apartment.
pub(crate) type CreateForeground = Box<
    dyn FnOnce() -> Result<Box<dyn ForegroundResolve>, Box<dyn std::error::Error + Send + Sync>>
        + Send,
>;

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_domain::capture::{AppIdentity, TitleStatus};
    const TEST_READ_TICK: u32 = 1_000;
    #[test]
    fn filters_only_foreground_window_title_and_window_destroys() {
        let foreground = WindowHandle::new(1);
        for kind in [
            EventKind::Title,
            EventKind::Destroy,
            EventKind::Foreground,
            EventKind::Review,
        ] {
            let event = NativeEvent {
                kind,
                window: foreground,
                object: WINDOW_OBJECT,
                child: SELF_CHILD,
            };
            assert_eq!(event.accepts(foreground), EventDecision::Accept);
            if matches!(kind, EventKind::Title | EventKind::Destroy) {
                assert_eq!(
                    NativeEvent { child: 1, ..event }.accepts(foreground),
                    EventDecision::Ignore
                );
                assert_eq!(
                    NativeEvent {
                        object: -4,
                        ..event
                    }
                    .accepts(foreground),
                    EventDecision::Ignore
                );
            }
        }
        assert_eq!(
            NativeEvent {
                kind: EventKind::Title,
                window: WindowHandle::new(2),
                object: WINDOW_OBJECT,
                child: SELF_CHILD
            }
            .accepts(foreground),
            EventDecision::Ignore
        );
    }
    fn current(handle: u64, text: &str) -> CurrentForeground {
        CurrentForeground {
            desktop: Desktop::Observable,
            window: WindowHandle::new(handle),
            title: Title::readable(text),
            affinity: DisplayAffinity::None,
        }
    }
    #[test]
    fn late_switcher_event_after_target_is_absorbed_without_observation_or_count() {
        let mut reads = ForegroundReads::default();
        let target = current(7, "target");
        assert_eq!(
            reads.observe(ReadSample::Trigger, &target),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: None
            }
        );
        let late = NativeEvent {
            kind: EventKind::Foreground,
            window: WindowHandle::new(9),
            object: WINDOW_OBJECT,
            child: SELF_CHILD,
        };
        assert_eq!(late.accepts(target.window), EventDecision::Accept);
        assert_eq!(
            reads.observe(ReadSample::Trigger, &target),
            ReadOutcome {
                decision: ReadDecision::Unchanged,
                untriggered: None
            }
        );
        assert_eq!(
            reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK
                },
                &target
            ),
            ReadOutcome {
                decision: ReadDecision::Unchanged,
                untriggered: None
            }
        );
    }
    #[test]
    fn duplicate_triggers_and_fallback_reads_do_not_emit_observations() {
        let mut reads = ForegroundReads::default();
        let state = current(1, "same");
        let _ = reads.observe(ReadSample::Trigger, &state);
        for cause in [
            ReadSample::Trigger,
            ReadSample::Trigger,
            ReadSample::Fallback {
                read_tick: TEST_READ_TICK,
            },
        ] {
            assert_eq!(
                reads.observe(cause, &state),
                ReadOutcome {
                    decision: ReadDecision::Unchanged,
                    untriggered: None
                }
            );
        }
    }
    #[test]
    fn out_of_order_title_event_for_nonforeground_window_is_filtered() {
        let state = current(7, "target");
        let event = NativeEvent {
            kind: EventKind::Title,
            window: WindowHandle::new(3),
            object: WINDOW_OBJECT,
            child: SELF_CHILD,
        };
        assert_eq!(event.accepts(state.window), EventDecision::Ignore);
    }
    #[test]
    fn current_window_retirement_rebuilds_baseline_even_when_state_matches() {
        let mut reads = ForegroundReads::default();
        let state = current(7, "same");
        let _ = reads.observe(ReadSample::Trigger, &state);
        reads.retire(WindowHandle::new(9));
        assert_eq!(
            reads.observe(ReadSample::Trigger, &state),
            ReadOutcome {
                decision: ReadDecision::Unchanged,
                untriggered: None
            }
        );
        reads.retire(state.window);
        assert_eq!(
            reads.observe(ReadSample::Trigger, &state),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: None
            }
        );
    }
    struct PendingFixture {
        reads: ForegroundReads,
        target: CurrentForeground,
        change: CoverageChange,
    }
    impl PendingFixture {
        fn new(read_tick: u32) -> Self {
            let mut reads = ForegroundReads::default();
            let previous = current(1, "previous");
            let target = current(2, "target");
            let _ = reads.observe(ReadSample::Trigger, &previous);
            // The changed observation is available immediately; the count is deferred.
            assert_eq!(
                reads.observe(ReadSample::Fallback { read_tick }, &target),
                ReadOutcome {
                    decision: ReadDecision::Changed,
                    untriggered: None
                }
            );
            Self {
                reads,
                change: CoverageChange {
                    previous_window: Some(previous.window),
                    reviewed_window: Some(target.window),
                },
                target,
            }
        }
    }
    #[test]
    fn in_flight_native_event_cancels_pending_count_before_unchanged_read() {
        for event_time in [TEST_READ_TICK - 20, TEST_READ_TICK] {
            let mut fixture = PendingFixture::new(TEST_READ_TICK);
            assert!(fixture.reads.native_event(event_time).is_none());
            assert_eq!(
                fixture.reads.observe(ReadSample::Trigger, &fixture.target),
                ReadOutcome {
                    decision: ReadDecision::Unchanged,
                    untriggered: None
                }
            );
            assert!(
                fixture
                    .reads
                    .observe(
                        ReadSample::Fallback {
                            read_tick: TEST_READ_TICK + 60_000
                        },
                        &fixture.target
                    )
                    .untriggered
                    .is_none()
            );
        }
    }
    #[test]
    fn first_native_event_after_fallback_tick_counts_pending_change_once() {
        let mut fixture = PendingFixture::new(TEST_READ_TICK);
        assert_eq!(
            fixture.reads.native_event(TEST_READ_TICK + 1),
            Some(fixture.change)
        );
        assert!(fixture.reads.native_event(TEST_READ_TICK + 2).is_none());
        assert_eq!(
            fixture.reads.observe(ReadSample::Trigger, &fixture.target),
            ReadOutcome {
                decision: ReadDecision::Unchanged,
                untriggered: None
            }
        );
    }
    #[test]
    fn pending_native_event_ordering_handles_tick_wrap_and_half_range() {
        let mut before_wrap = PendingFixture::new(3);
        assert!(before_wrap.reads.native_event(u32::MAX - 2).is_none());
        let mut after_wrap = PendingFixture::new(u32::MAX - 2);
        assert_eq!(after_wrap.reads.native_event(3), Some(after_wrap.change));
        let mut half_range = PendingFixture::new(TEST_READ_TICK);
        assert_eq!(
            half_range
                .reads
                .native_event(TEST_READ_TICK.wrapping_add(NATIVE_TICK_HALF_RANGE)),
            Some(half_range.change)
        );
    }
    #[test]
    fn requested_reads_and_baseline_invalidation_leave_pending_change_untouched() {
        let mut fixture = PendingFixture::new(TEST_READ_TICK);
        assert!(
            fixture
                .reads
                .observe(ReadSample::Trigger, &fixture.target)
                .untriggered
                .is_none()
        );
        let requested = current(3, "requested");
        assert!(
            fixture
                .reads
                .observe(ReadSample::Trigger, &requested)
                .untriggered
                .is_none()
        );
        fixture.reads.invalidate();
        assert_eq!(
            fixture.reads.observe(ReadSample::Trigger, &requested),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: None
            }
        );
        assert_eq!(
            fixture.reads.native_event(TEST_READ_TICK + 1),
            Some(fixture.change)
        );
    }
    #[test]
    fn filtered_title_event_still_resolves_pending_change() {
        for event_time in [TEST_READ_TICK, TEST_READ_TICK + 1] {
            let mut fixture = PendingFixture::new(TEST_READ_TICK);
            let event = NativeEvent {
                kind: EventKind::Title,
                window: WindowHandle::new(99),
                object: WINDOW_OBJECT,
                child: SELF_CHILD,
            };
            let resolved = fixture.reads.native_event(event_time);
            assert_eq!(event.accepts(fixture.target.window), EventDecision::Ignore);
            let expected = if event_time == TEST_READ_TICK {
                None
            } else {
                Some(fixture.change)
            };
            assert_eq!(resolved, expected);
            assert!(
                fixture
                    .reads
                    .observe(
                        ReadSample::Fallback {
                            read_tick: TEST_READ_TICK + 60_000
                        },
                        &fixture.target
                    )
                    .untriggered
                    .is_none()
            );
        }
    }
    #[test]
    fn next_fallback_settles_old_change_before_pending_its_new_change() {
        let mut fixture = PendingFixture::new(TEST_READ_TICK);
        let next = current(3, "next");
        assert_eq!(
            fixture.reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK + 60_000
                },
                &next
            ),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: Some(fixture.change)
            }
        );
        assert_eq!(
            fixture.reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK + 120_000
                },
                &next
            ),
            ReadOutcome {
                decision: ReadDecision::Unchanged,
                untriggered: Some(CoverageChange {
                    previous_window: Some(fixture.target.window),
                    reviewed_window: Some(next.window),
                })
            }
        );
    }
    #[test]
    fn fallback_change_stays_pending_until_next_fallback_with_window_evidence() {
        let mut fixture = PendingFixture::new(TEST_READ_TICK);
        assert_eq!(
            fixture.reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK + 60_000
                },
                &fixture.target
            ),
            ReadOutcome {
                decision: ReadDecision::Unchanged,
                untriggered: Some(fixture.change)
            }
        );
        assert_eq!(
            fixture.reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK + 120_000
                },
                &fixture.target
            ),
            ReadOutcome {
                decision: ReadDecision::Unchanged,
                untriggered: None
            }
        );
    }

    #[test]
    fn fallback_after_received_trigger_never_counts() {
        let mut reads = ForegroundReads::default();
        let _ = reads.observe(ReadSample::Trigger, &current(1, "previous"));
        reads.trigger();
        assert_eq!(
            reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK
                },
                &current(2, "target")
            ),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: None
            }
        );
    }
    #[test]
    fn desktop_switch_in_and_out_is_triggered_and_never_counts() {
        let mut reads = ForegroundReads::default();
        let previous = current(1, "previous");
        let _ = reads.observe(ReadSample::Trigger, &previous);
        let unknown = CurrentForeground {
            desktop: Desktop::Unknown,
            window: WindowHandle::new(0),
            title: Title::unreadable(),
            affinity: DisplayAffinity::Unavailable,
        };
        for state in [&unknown, &previous] {
            assert_eq!(
                reads.observe(ReadSample::Trigger, state),
                ReadOutcome {
                    decision: ReadDecision::Changed,
                    untriggered: None
                }
            );
        }
    }
    #[test]
    fn initial_and_invalidated_baselines_never_count_even_on_fallback() {
        let mut reads = ForegroundReads::default();
        let state = current(1, "previous");
        assert_eq!(
            reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK
                },
                &state
            ),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: None
            }
        );
        for cause in [
            ReadSample::Trigger,
            ReadSample::Fallback {
                read_tick: TEST_READ_TICK,
            },
        ] {
            reads.invalidate();
            assert_eq!(
                reads.observe(cause, &state),
                ReadOutcome {
                    decision: ReadDecision::Changed,
                    untriggered: None
                }
            );
        }
    }
    #[test]
    fn rebind_mismatch_reads_current_state_instead_of_requested_window() {
        let mut reads = ForegroundReads::default();
        let previous = current(1, "previous");
        let target = current(2, "target");
        let _ = reads.observe(ReadSample::Trigger, &previous);
        let requested = HookTarget::Process {
            window: previous.window,
            process: ProcessId::new(7),
        };
        assert_eq!(
            target.accepts_rebind(requested, ProcessId::new(8)),
            RebindDecision::ReadCurrent
        );
        assert_eq!(
            reads.observe(ReadSample::Trigger, &target),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: None
            }
        );
        let binding = HookTarget::Process {
            window: target.window,
            process: ProcessId::new(8),
        };
        assert_eq!(
            target.accepts_rebind(binding, ProcessId::new(7)),
            RebindDecision::ReadCurrent
        );
        assert_eq!(
            target.accepts_rebind(binding, ProcessId::new(8)),
            RebindDecision::Accept
        );
        assert_eq!(
            target.accepts_rebind(HookTarget::Unbind, ProcessId::new(8)),
            RebindDecision::ReadCurrent
        );
    }
    #[test]
    fn failed_trigger_read_preserves_evidence_until_next_completed_read() {
        let mut reads = ForegroundReads::default();
        let _ = reads.observe(ReadSample::Trigger, &current(1, "previous"));
        reads.failed_read(ReadCause::Trigger);
        assert_eq!(
            reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK
                },
                &current(2, "target")
            ),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: None
            }
        );
        reads.failed_read(ReadCause::Fallback);
        let next = current(3, "next");
        assert_eq!(
            reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK + 60_000
                },
                &next
            ),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: None
            }
        );
        assert!(
            reads
                .observe(
                    ReadSample::Fallback {
                        read_tick: TEST_READ_TICK + 120_000
                    },
                    &next
                )
                .untriggered
                .is_some()
        );
    }

    #[test]
    fn delivery_failure_invalidates_baseline_and_success_preserves_it() {
        let mut reads = ForegroundReads::default();
        let _ = reads.observe(ReadSample::Trigger, &current(1, "previous"));
        reads
            .accept_delivery(Ok::<(), std::io::Error>(()))
            .expect("delivered");
        let target = current(2, "target");
        let _ = reads.observe(
            ReadSample::Fallback {
                read_tick: TEST_READ_TICK,
            },
            &target,
        );
        reads
            .accept_delivery(Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)))
            .expect_err("disconnected delivery");
        // Invalidation rebuilds the baseline while preserving the older pending diagnostic.
        assert_eq!(
            reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK + 60_000
                },
                &target
            ),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: Some(CoverageChange {
                    previous_window: Some(WindowHandle::new(1)),
                    reviewed_window: Some(target.window),
                })
            }
        );
        assert!(reads.native_event(TEST_READ_TICK + 60_001).is_none());
    }

    #[test]
    fn display_affinity_changes_emit_and_count_only_untriggered_fallbacks() {
        let mut reads = ForegroundReads::default();
        let mut state = current(1, "same");
        let _ = reads.observe(ReadSample::Trigger, &state);
        state.affinity = DisplayAffinity::Exclude;
        assert_eq!(
            reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK
                },
                &state
            ),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: None
            }
        );
        assert_eq!(
            reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK + 60_000
                },
                &state
            ),
            ReadOutcome {
                decision: ReadDecision::Unchanged,
                untriggered: Some(CoverageChange {
                    previous_window: Some(state.window),
                    reviewed_window: Some(state.window),
                })
            }
        );
        state.affinity = DisplayAffinity::Unavailable;
        assert_eq!(
            reads.observe(ReadSample::Trigger, &state),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: None
            }
        );
        state.affinity = DisplayAffinity::None;
        reads.trigger();
        assert_eq!(
            reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK + 120_000
                },
                &state
            ),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: None
            }
        );
        reads.invalidate();
        state.affinity = DisplayAffinity::Monitor;
        assert_eq!(
            reads.observe(
                ReadSample::Fallback {
                    read_tick: TEST_READ_TICK + 180_000
                },
                &state
            ),
            ReadOutcome {
                decision: ReadDecision::Changed,
                untriggered: None
            }
        );
    }

    #[test]
    fn baseline_compares_title_status_as_well_as_text() {
        let mut reads = ForegroundReads::default();
        let mut state = current(1, "same");
        let _ = reads.observe(ReadSample::Trigger, &state);
        for title in [
            Title::truncated("same"),
            Title::unreadable(),
            Title::readable(""),
        ] {
            state.title = title;
            assert_eq!(
                reads.observe(ReadSample::Trigger, &state),
                ReadOutcome {
                    decision: ReadDecision::Changed,
                    untriggered: None
                }
            );
        }
    }
    #[test]
    fn titles_preserve_failure_empty_and_surrogate_boundaries() {
        assert_eq!(
            TitleRead::Failed.snapshot(&[]).status(),
            TitleStatus::Unreadable
        );
        assert_eq!(
            TitleRead::Copied(0).snapshot(&[]).status(),
            TitleStatus::Empty
        );
        assert_eq!(
            TitleRead::Copied(2).snapshot(&[65]).status(),
            TitleStatus::Unreadable
        );
        assert_eq!(TitleRead::Copied(1).snapshot(&[65]).text(), Some("A"));
        assert_eq!(
            TitleRead::Copied(1).snapshot(&[0xD800]).status(),
            TitleStatus::Unreadable
        );
        assert_eq!(
            TitleRead::Copied(2).snapshot(&[0xD83D, 0xDE00]).text(),
            Some("😀")
        );
        let mut buffer = vec![65; TITLE_BUFFER_UNITS];
        buffer[limits::TITLE_UTF16_UNITS - 1] = 0xD800;
        let title = TitleRead::Copied(limits::TITLE_UTF16_UNITS).snapshot(&buffer);
        assert_eq!(title.status(), TitleStatus::Truncated);
        assert_eq!(
            title.text().expect("text").len(),
            limits::TITLE_UTF16_UNITS - 1
        );
    }
    #[test]
    fn prefers_aumid_and_normalizes_executable_identity() {
        assert_eq!(
            AppIdentity::from_process("C:/Apps/TEST.EXE", None),
            AppIdentity::Exe("c:\\apps\\test.exe".into())
        );
        assert_eq!(
            AppIdentity::from_process("C:/Apps/TEST.EXE", Some("Package!App")),
            AppIdentity::Aumid("Package!App".into())
        );
    }
    #[test]
    fn selects_visible_foreign_core_window() {
        let host = ProcessId::new(1);
        let mut children = vec![ChildWindow {
            window: WindowHandle::new(2),
            process: host,
            class: "Windows.UI.Core.CoreWindow".into(),
            visibility: Visibility::Visible,
        }];
        assert!(ChildWindow::content(&children, host).is_none());
        children.push(ChildWindow {
            window: WindowHandle::new(3),
            process: ProcessId::new(2),
            class: "Windows.UI.Core.CoreWindow".into(),
            visibility: Visibility::Hidden,
        });
        assert!(ChildWindow::content(&children, host).is_none());
        children[1].visibility = Visibility::Visible;
        assert_eq!(
            ChildWindow::content(&children, host)
                .expect("content")
                .window,
            WindowHandle::new(3)
        );
    }
    #[test]
    fn selects_content_from_settings_frame_topology() {
        let host = ProcessId::new(1);
        let content = WindowHandle::new(2);
        let children = [
            ChildWindow {
                window: WindowHandle::new(1),
                process: host,
                class: "ApplicationFrameTitleBarWindow".into(),
                visibility: Visibility::Visible,
            },
            ChildWindow {
                window: content,
                process: ProcessId::new(2),
                class: "Windows.UI.Core.CoreWindow".into(),
                visibility: Visibility::Visible,
            },
            ChildWindow {
                window: WindowHandle::new(3),
                process: host,
                class: "ApplicationFrameTitleBarWindow".into(),
                visibility: Visibility::Hidden,
            },
            ChildWindow {
                window: WindowHandle::new(4),
                process: host,
                class: "ApplicationFrameInputSinkWindow".into(),
                visibility: Visibility::Visible,
            },
        ];
        assert_eq!(
            ChildWindow::content(&children, host)
                .expect("Settings content among frame controls")
                .window,
            content
        );
    }
    #[test]
    fn maps_affinity_without_treating_failure_as_absence() {
        assert_eq!(
            DisplayAffinity::from_raw(Some(0)).signal(),
            SystemSignal::Absent
        );
        assert_eq!(
            DisplayAffinity::from_raw(Some(1)).signal(),
            SystemSignal::Present
        );
        assert_eq!(
            DisplayAffinity::from_raw(Some(0x11)).signal(),
            SystemSignal::Present
        );
        assert_eq!(
            DisplayAffinity::from_raw(Some(0xff)).signal(),
            SystemSignal::Unknown
        );
        assert_eq!(
            DisplayAffinity::from_raw(None).signal(),
            SystemSignal::Unknown
        );
        for affinity in [DisplayAffinity::Monitor, DisplayAffinity::Exclude] {
            assert_eq!(affinity.signal(), SystemSignal::Present);
        }
        assert_eq!(DisplayAffinity::Unavailable.signal(), SystemSignal::Unknown);
    }
    #[test]
    fn desktop_queries_fail_closed_and_require_matching_names() {
        let default: Vec<_> = "Default".encode_utf16().collect();
        let secure: Vec<_> = "Winlogon".encode_utf16().collect();
        assert_eq!(
            Desktop::compare_names(&default, &default),
            Desktop::Observable
        );
        assert_eq!(Desktop::compare_names(&secure, &default), Desktop::Unknown);
        assert_eq!(Desktop::compare_names(&[], &default), Desktop::Unknown);
        assert_eq!(Desktop::compare_names(&default, &[]), Desktop::Unknown);
    }
    #[test]
    fn chooses_display_language_then_english_then_first_translation() {
        let english = Translation {
            language: ENGLISH_US,
            codepage: 1200,
        };
        let chinese = Translation {
            language: 0x0804,
            codepage: 1200,
        };
        assert_eq!(
            Translation::select(&[english, chinese], &[chinese.language]),
            Some(chinese)
        );
        assert_eq!(
            Translation::select(&[chinese, english], &[0x0411]),
            Some(english)
        );
        assert_eq!(Translation::select(&[chinese], &[]), Some(chinese));
        assert_eq!(Translation::select(&[], &[]), None);
    }
    #[test]
    fn cache_is_bounded_lru_and_retires_exited_or_reused_processes() {
        let mut cache = BoundedCache::new(PROCESS_CACHE_CAPACITY);
        for created in 0..u64::try_from(PROCESS_CACHE_CAPACITY).expect("capacity") {
            cache.insert(
                ProcessKey {
                    process: ProcessId::new(1),
                    created,
                },
                created,
            );
        }
        let first = ProcessKey {
            process: ProcessId::new(1),
            created: 0,
        };
        assert_eq!(cache.find(&first), Some(&0));
        cache.insert(
            ProcessKey {
                process: ProcessId::new(2),
                created: 200,
            },
            200,
        );
        assert!(
            cache
                .find(&ProcessKey {
                    process: ProcessId::new(1),
                    created: 1
                })
                .is_none()
        );
        cache.retain_lifetime(ProcessKey {
            process: ProcessId::new(1),
            created: 0,
        });
        assert_eq!(cache.find(&first), Some(&0));
        cache.evict_process(ProcessId::new(1));
        assert!(cache.find(&first).is_none());
        assert_eq!(cache.entries.len(), 1);
        let names: BoundedCache<String, String> = BoundedCache::new(DISPLAY_CACHE_CAPACITY);
        assert!(names.entries.is_empty());
    }
}
