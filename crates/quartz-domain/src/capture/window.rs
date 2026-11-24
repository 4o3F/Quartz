//! Run-local window identity and bounded title observations.

use super::{Timestamp, UtcInstant, WindowId, ids::IdAllocator, limits};
use std::collections::BTreeMap;

/// Opaque native window value; no platform handle type crosses the domain boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowHandle(u64);
impl WindowHandle {
    /// Constructs an opaque platform value supplied by the shell.
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }
    /// Returns the value for the platform or persistence adapter.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}
/// Platform process identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(u32);
impl ProcessId {
    /// Constructs an opaque platform value supplied by the shell.
    #[must_use]
    pub fn new(value: u32) -> Self {
        Self(value)
    }
    /// Returns the value for the platform or persistence adapter.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}
/// HWND reuse sequence within one capture run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct InstanceGeneration(u64);

impl InstanceGeneration {
    /// Starts the first lifetime observed for a window handle.
    #[must_use]
    pub fn initial() -> Self {
        Self(0)
    }

    /// Returns the reuse sequence for persistence adapters.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// Returns the next lifetime sequence, or `None` when the counter is exhausted.
    #[must_use]
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Application identity already resolved and normalized by the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppIdentity {
    /// Normalized lowercase absolute executable path.
    Exe(String),
    /// Application user model identifier, preferred for packaged applications.
    Aumid(String),
    /// Window class used when process identity cannot be resolved.
    Unresolved(String),
}
impl AppIdentity {
    /// Prefers a packaged application's AUMID, otherwise normalizes its Windows executable path.
    #[must_use]
    pub fn from_process(executable: &str, aumid: Option<&str>) -> Self {
        match aumid {
            Some(value) => Self::Aumid(value.to_owned()),
            None => Self::Exe(executable.replace('/', "\\").to_lowercase()),
        }
    }

    /// Produces the stable application dictionary key.
    pub fn key(&self) -> String {
        match self {
            Self::Exe(value) => format!("exe:{value}"),
            Self::Aumid(value) => format!("aumid:{value}"),
            Self::Unresolved(value) => format!("unresolved:{value}"),
        }
    }
}

/// Title status codes match the existing store ontology; conversion stays in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleStatus {
    /// The capped title was read successfully.
    Ok,
    /// The shell successfully read an empty title.
    Empty,
    /// The shell could not read a title snapshot.
    Unreadable,
    /// The title exceeded the configured UTF-16 unit cap.
    Truncated,
    /// Privacy policy discarded the captured title.
    Redacted,
}

/// A bounded title input. Debug formatting deliberately excludes its text.
#[derive(Clone, PartialEq, Eq)]
pub struct Title {
    text: Option<String>,
    status: TitleStatus,
}
impl std::fmt::Debug for Title {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Title")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}
impl Title {
    /// Caps text before privacy evaluation, without cutting a UTF-16 surrogate pair.
    pub fn readable(text: &str) -> Self {
        let mut units = 0;
        let bounded: String = text
            .chars()
            .take_while(|ch| {
                units += ch.len_utf16();
                units <= limits::TITLE_UTF16_UNITS
            })
            .collect();
        let status = if bounded.len() < text.len() {
            TitleStatus::Truncated
        } else if bounded.is_empty() {
            TitleStatus::Empty
        } else {
            TitleStatus::Ok
        };
        Self {
            text: Some(bounded),
            status,
        }
    }
    /// Marks a snapshot whose native buffer was filled to the title cap.
    #[must_use]
    pub fn truncated(text: &str) -> Self {
        let mut title = Self::readable(text);
        title.status = TitleStatus::Truncated;
        title
    }
    /// Represents a failed title read without substituting a later title.
    pub fn unreadable() -> Self {
        Self {
            text: None,
            status: TitleStatus::Unreadable,
        }
    }
    /// Borrows the capped text used for privacy matching.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
    /// Returns how the shell's read was represented.
    pub fn status(&self) -> TitleStatus {
        self.status
    }
    pub(super) fn fingerprint(&self) -> [u8; 32] {
        use sha2::Digest;
        let mut digest = sha2::Sha256::new();
        digest.update([match self.status {
            TitleStatus::Ok => 0,
            TitleStatus::Empty => 1,
            TitleStatus::Unreadable => 2,
            TitleStatus::Truncated => 3,
            TitleStatus::Redacted => 4,
        }]);
        if let Some(text) = &self.text {
            digest.update(text.as_bytes());
        }
        digest.finalize().into()
    }
    pub(super) fn redact(&mut self) {
        self.text = None;
        self.status = TitleStatus::Redacted;
    }
}

/// Origin of application identity and metadata supplied by an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataSource {
    /// Executable and display name were obtained from a process query.
    Process,
    /// Identity came from a frame property; descriptive metadata remains provisional.
    Frame,
    /// Application metadata could not be resolved.
    Unavailable,
}

/// Whether descriptive application metadata needs a fresh resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataRefresh {
    /// Metadata is final for this observation, including unavailable metadata.
    Settled,
    /// Descriptive application metadata is provisional and may be completed by a fresh resolution.
    Pending,
}
impl MetadataSource {
    /// Returns whether a maintenance read should request fresh descriptive metadata.
    #[must_use]
    pub fn refresh(self) -> MetadataRefresh {
        match self {
            Self::Frame => MetadataRefresh::Pending,
            Self::Process | Self::Unavailable => MetadataRefresh::Settled,
        }
    }
}

/// Identity facts that the shell revalidated after resolving process metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowFacts {
    /// Run-local native window value.
    pub handle: WindowHandle,
    /// Revalidated owning process.
    pub process: ProcessId,
    /// Process creation UTC, absent on query failure.
    pub process_created_utc: Option<UtcInstant>,
    /// Exact class name; changes retire the old instance.
    pub class: String,
    /// Resolved application identity.
    pub app: AppIdentity,
    /// Source of this observation's application metadata.
    pub metadata_source: MetadataSource,
    /// Lowercase executable filename used by rules.
    pub exe_name: Option<String>,
    /// Resolved application display name.
    pub display_name: String,
    /// Immediate, capped title snapshot.
    pub title: Title,
}

/// Persistable instance identity, emitted only after application privacy permits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInstance {
    /// Core-assigned logical key, scoped by run.
    pub id: WindowId,
    /// Revalidated window identity and application metadata; title is excluded.
    pub handle: WindowHandle,
    /// Owning process.
    pub process: ProcessId,
    /// Optional process creation time.
    pub process_created_utc: Option<UtcInstant>,
    /// HWND reuse generation within this run.
    pub generation: InstanceGeneration,
    /// Exact observed class.
    pub class: String,
    /// Application dictionary identity.
    pub app: AppIdentity,
    /// Source of the latest observation, used to decide application dictionary updates.
    pub metadata_source: MetadataSource,
    /// Executable filename.
    pub exe_name: Option<String>,
    /// Application display name.
    pub display_name: String,
    /// First observation of this instance.
    pub first_seen: Timestamp,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstanceLifecycle {
    Live,
    Retired,
}
#[derive(Debug, Clone)]
struct Entry {
    instance: WindowInstance,
    lifecycle: InstanceLifecycle,
    last_seen: super::MonoInstant,
}
#[derive(Debug)]
pub(super) struct ObservedInstance {
    pub instance: WindowInstance,
    pub retired: Option<WindowId>,
}
/// Window identities retained in a bounded run-local cache, including retirement history per HWND.
#[derive(Debug, Clone, Default)]
pub struct WindowInstances {
    entries: BTreeMap<WindowHandle, Entry>,
    ids: IdAllocator,
    generation_floor: InstanceGeneration,
    highest_generation: InstanceGeneration,
}
impl WindowInstances {
    /// Resolves a current instance, incrementing generation after retirement or identity change.
    #[must_use]
    pub fn observe(&mut self, facts: &WindowFacts, at: Timestamp) -> WindowInstance {
        self.observe_with_retirement(facts, at, None).instance
    }
    pub(super) fn observe_with_retirement(
        &mut self,
        facts: &WindowFacts,
        at: Timestamp,
        protected: Option<WindowId>,
    ) -> ObservedInstance {
        let old = self.entries.get(&facts.handle);
        let same = old.is_some_and(|old| {
            old.lifecycle == InstanceLifecycle::Live
                && old.instance.process == facts.process
                && old.instance.process_created_utc == facts.process_created_utc
                && old.instance.class == facts.class
                && old.instance.app == facts.app
        });
        if same {
            let entry = self
                .entries
                .get_mut(&facts.handle)
                .expect("matching entry exists");
            if facts.metadata_source == MetadataSource::Process {
                entry.instance.exe_name = facts.exe_name.clone();
                entry.instance.display_name = facts.display_name.clone();
            }
            entry.instance.metadata_source = facts.metadata_source;
            entry.last_seen = at.mono;
            return ObservedInstance {
                instance: entry.instance.clone(),
                retired: None,
            };
        }
        let previous_generation = old.map(|entry| entry.instance.generation);
        let mut retired = old
            .filter(|entry| entry.lifecycle == InstanceLifecycle::Live)
            .map(|entry| entry.instance.id);
        if old.is_none() && self.entries.len() >= limits::WINDOW_INSTANCE_CAPACITY {
            let (&handle, _) = self
                .entries
                .iter()
                .filter(|(_, entry)| Some(entry.instance.id) != protected)
                .min_by_key(|(_, entry)| entry.last_seen)
                .expect("cache capacity leaves a victim outside the protected foreground");
            self.generation_floor = self
                .highest_generation
                .next()
                .expect("window instance generation exhausted");
            retired = self
                .entries
                .remove(&handle)
                .filter(|entry| entry.lifecycle == InstanceLifecycle::Live)
                .map(|entry| entry.instance.id);
        }
        let generation = match previous_generation {
            Some(previous) => previous
                .next()
                .expect("window instance generation exhausted")
                .max(self.generation_floor),
            None => self.generation_floor,
        };
        self.highest_generation = self.highest_generation.max(generation);
        let instance = WindowInstance {
            id: self.ids.window(),
            handle: facts.handle,
            process: facts.process,
            process_created_utc: facts.process_created_utc,
            generation,
            class: facts.class.clone(),
            app: facts.app.clone(),
            metadata_source: facts.metadata_source,
            exe_name: facts.exe_name.clone(),
            display_name: facts.display_name.clone(),
            first_seen: at,
        };
        self.entries.insert(
            facts.handle,
            Entry {
                instance: instance.clone(),
                lifecycle: InstanceLifecycle::Live,
                last_seen: at.mono,
            },
        );
        ObservedInstance { instance, retired }
    }
    #[cfg(test)]
    pub(super) fn retained_count(&self) -> usize {
        self.entries.len()
    }
    /// Finds the live instance associated with a native destroy notification.
    pub(super) fn live_id(&self, handle: WindowHandle) -> Option<WindowId> {
        self.entries
            .get(&handle)
            .filter(|entry| entry.lifecycle == InstanceLifecycle::Live)
            .map(|entry| entry.instance.id)
    }
    /// Retires only the matching instance, so a delayed destroy cannot retire a reused HWND.
    pub fn retire(&mut self, id: WindowId) {
        for entry in self.entries.values_mut() {
            if entry.instance.id == id {
                entry.lifecycle = InstanceLifecycle::Retired;
                break;
            }
        }
    }
}
