//! Platform-independent foreground filtering and metadata selection.

use quartz_domain::capture::{
    AppIdentity, ForegroundGeneration, ProcessId, SystemSignal, Title, WindowHandle, limits,
};
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
    Stale,
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
#[derive(Debug)]
pub(crate) struct ForegroundTarget {
    window: WindowHandle,
    generation: ForegroundGeneration,
}
impl ForegroundTarget {
    pub(crate) fn new() -> Self {
        Self {
            window: WindowHandle::new(0),
            generation: ForegroundGeneration::new(0),
        }
    }
    pub(crate) fn window(&self) -> WindowHandle {
        self.window
    }
    pub(crate) fn generation(&self) -> ForegroundGeneration {
        self.generation
    }
    pub(crate) fn accepts_rebind(&self, generation: ForegroundGeneration) -> RebindDecision {
        if self.generation == generation {
            RebindDecision::Accept
        } else {
            RebindDecision::Stale
        }
    }
    pub(crate) fn observe(&mut self, event: NativeEvent) {
        if event.kind == EventKind::Foreground || self.window != event.window {
            self.generation = ForegroundGeneration::new(
                self.generation
                    .get()
                    .checked_add(1)
                    .expect("foreground generation exhausted"),
            );
            self.window = event.window;
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum TitleRead {
    Copied(usize),
    Failed,
}
pub(crate) fn title_snapshot(buffer: &[u16], read: TitleRead) -> Title {
    let TitleRead::Copied(count) = read else {
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
            .is_some_and(|unit| (0xD800..=0xDBFF).contains(unit))
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

pub(crate) fn application_identity(executable: &str, aumid: Option<&str>) -> AppIdentity {
    match aumid {
        Some(value) => AppIdentity::Aumid(value.to_owned()),
        None => AppIdentity::Exe(executable.replace('/', "\\").to_lowercase()),
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
pub(crate) fn content_window(children: &[ChildWindow], host: ProcessId) -> Option<&ChildWindow> {
    children.iter().find(|child| {
        child.process != host
            && child.visibility == Visibility::Visible
            && child.class == "Windows.UI.Core.CoreWindow"
    })
}
#[derive(Debug, Clone, Copy)]
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
pub(crate) fn version_translation(
    available: &[Translation],
    preferred: &[u16],
) -> Option<Translation> {
    preferred
        .iter()
        .chain(std::iter::once(&ENGLISH_US))
        .find_map(|language| available.iter().find(|entry| entry.language == *language))
        .or_else(|| available.first())
        .copied()
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
#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct Snapshot {
    pub at: quartz_domain::capture::Timestamp,
    pub read_at: quartz_domain::capture::Timestamp,
    pub event_time_ms: u32,
    pub generation: quartz_domain::capture::ForegroundGeneration,
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

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_domain::capture::TitleStatus;
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
    #[test]
    fn reviews_preserve_generation_and_stale_rebinds_are_rejected() {
        let mut target = ForegroundTarget::new();
        let mut event = NativeEvent {
            kind: EventKind::Review,
            window: WindowHandle::new(1),
            object: WINDOW_OBJECT,
            child: SELF_CHILD,
        };
        target.observe(event);
        let first = target.generation();
        assert_eq!(target.window(), event.window);
        target.observe(event);
        assert_eq!(target.accepts_rebind(first), RebindDecision::Accept);
        event.kind = EventKind::Foreground;
        target.observe(event);
        assert_eq!(target.accepts_rebind(first), RebindDecision::Stale);
        assert_eq!(
            target.accepts_rebind(target.generation()),
            RebindDecision::Accept
        );
    }
    #[test]
    fn titles_preserve_failure_empty_and_surrogate_boundaries() {
        assert_eq!(
            title_snapshot(&[], TitleRead::Failed).status(),
            TitleStatus::Unreadable
        );
        assert_eq!(
            title_snapshot(&[], TitleRead::Copied(0)).status(),
            TitleStatus::Empty
        );
        assert_eq!(
            title_snapshot(&[65], TitleRead::Copied(2)).status(),
            TitleStatus::Unreadable
        );
        assert_eq!(
            title_snapshot(&[65], TitleRead::Copied(1)).text(),
            Some("A")
        );
        assert_eq!(
            title_snapshot(&[0xD800], TitleRead::Copied(1)).status(),
            TitleStatus::Unreadable
        );
        assert_eq!(
            title_snapshot(&[0xD83D, 0xDE00], TitleRead::Copied(2)).text(),
            Some("😀")
        );
        let mut buffer = vec![65; TITLE_BUFFER_UNITS];
        buffer[limits::TITLE_UTF16_UNITS - 1] = 0xD800;
        let title = title_snapshot(&buffer, TitleRead::Copied(limits::TITLE_UTF16_UNITS));
        assert_eq!(title.status(), TitleStatus::Truncated);
        assert_eq!(
            title.text().expect("text").len(),
            limits::TITLE_UTF16_UNITS - 1
        );
    }
    #[test]
    fn prefers_aumid_and_normalizes_executable_identity() {
        assert_eq!(
            application_identity("C:/Apps/TEST.EXE", None),
            AppIdentity::Exe("c:\\apps\\test.exe".into())
        );
        assert_eq!(
            application_identity("C:/Apps/TEST.EXE", Some("Package!App")),
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
        assert!(content_window(&children, host).is_none());
        children.push(ChildWindow {
            window: WindowHandle::new(3),
            process: ProcessId::new(2),
            class: "Windows.UI.Core.CoreWindow".into(),
            visibility: Visibility::Hidden,
        });
        assert!(content_window(&children, host).is_none());
        children[1].visibility = Visibility::Visible;
        assert_eq!(
            content_window(&children, host).expect("content").window,
            WindowHandle::new(3)
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
            version_translation(&[english, chinese], &[chinese.language]),
            Some(chinese)
        );
        assert_eq!(
            version_translation(&[chinese, english], &[0x0411]),
            Some(english)
        );
        assert_eq!(version_translation(&[chinese], &[]), Some(chinese));
        assert_eq!(version_translation(&[], &[]), None);
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
