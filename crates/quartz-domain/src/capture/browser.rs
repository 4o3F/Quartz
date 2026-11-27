//! Browser stream inputs, pure window-to-tab linking and retention levels.

use super::{BrowserContext, Title};
use std::{collections::BTreeMap, fmt};

const CHROME_EXE_NAME: &str = "chrome.exe";
const EDGE_EXE_NAME: &str = "msedge.exe";
const FIREFOX_EXE_NAME: &str = "firefox.exe";

const CHROME_TITLE_SUFFIXES: &[&str] = &[" - Google Chrome"];
const EDGE_TITLE_SUFFIXES: &[&str] = &[" - Microsoft\u{200b} Edge", " - Microsoft Edge"];
const FIREFOX_TITLE_SUFFIXES: &[&str] = &[" \u{2014} Mozilla Firefox", " - Mozilla Firefox"];

pub(super) const INTERNAL_BROWSER_DOMAIN: &str = "(internal)";
pub(super) const LOCAL_FILE_BROWSER_DOMAIN: &str = "(local-file)";

const WEB_LEVEL_FULL_CODE: i32 = 1;
const WEB_LEVEL_DOMAIN_ONLY_CODE: i32 = 2;

/// Supported browser families for extension stream linking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BrowserFamily {
    /// Google Chrome.
    Chrome,
    /// Microsoft Edge.
    Edge,
    /// Mozilla Firefox.
    Firefox,
}

impl BrowserFamily {
    /// Returns the lowercase Windows executable filename for this browser family.
    #[must_use]
    pub const fn exe_name(self) -> &'static str {
        match self {
            Self::Chrome => CHROME_EXE_NAME,
            Self::Edge => EDGE_EXE_NAME,
            Self::Firefox => FIREFOX_EXE_NAME,
        }
    }

    /// Maps an executable filename to a supported browser family.
    #[must_use]
    pub fn from_exe_name(exe_name: &str) -> Option<Self> {
        if exe_name.eq_ignore_ascii_case(CHROME_EXE_NAME) {
            Some(Self::Chrome)
        } else if exe_name.eq_ignore_ascii_case(EDGE_EXE_NAME) {
            Some(Self::Edge)
        } else if exe_name.eq_ignore_ascii_case(FIREFOX_EXE_NAME) {
            Some(Self::Firefox)
        } else {
            None
        }
    }

    const fn title_suffixes(self) -> &'static [&'static str] {
        match self {
            Self::Chrome => CHROME_TITLE_SUFFIXES,
            Self::Edge => EDGE_TITLE_SUFFIXES,
            Self::Firefox => FIREFOX_TITLE_SUFFIXES,
        }
    }
}

/// Persisted `browser_streams` row identifier supplied by the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BrowserStreamKey(i64);

impl BrowserStreamKey {
    /// Wraps a `browser_streams` row identifier.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Returns the underlying `browser_streams` row identifier.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

/// Stable stream identity and browser family pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BrowserStreamRef {
    /// Persisted `browser_streams` row identifier.
    pub id: BrowserStreamKey,
    /// Browser family reported by this extension stream.
    pub family: BrowserFamily,
}

/// Whether a browser stream currently reports OS focus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BrowserFocus {
    /// The browser window is focused.
    Focused,
    /// The browser window is not focused.
    #[default]
    Unfocused,
}

/// OS window state reported by a browser stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserWindowState {
    /// Standard restored window.
    Normal,
    /// Maximized window.
    Maximized,
    /// Minimized window, excluded from foreground linking.
    Minimized,
    /// Fullscreen window.
    Fullscreen,
}

/// Whether a browser tab is currently producing audio.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BrowserAudible {
    /// The tab is not producing audio.
    #[default]
    Silent,
    /// The tab is producing audio.
    Audible,
}

/// Active tab metadata reported by a browser stream.
///
/// Debug formatting deliberately redacts domain and title text.
#[derive(Clone, PartialEq, Eq)]
pub struct BrowserTab {
    /// Browser-assigned tab identifier within the stream.
    pub id: i64,
    /// Domain or special marker (`(internal)`, `(local-file)`), when present.
    pub domain: Option<String>,
    /// Capped tab title.
    pub title: Title,
    /// Whether the tab is currently audible.
    pub audible: BrowserAudible,
}

impl fmt::Debug for BrowserTab {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserTab")
            .field("id", &self.id)
            .field("domain_len", &self.domain.as_ref().map(String::len))
            .field("title", &self.title)
            .field("audible", &self.audible)
            .finish()
    }
}

/// Latest state snapshot for one connected browser stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSnapshot {
    /// Focus state reported by the browser extension.
    pub focus: BrowserFocus,
    /// Browser-assigned window identifier, when a window is present.
    pub window_id: Option<i64>,
    /// Window state reported by the browser extension, when present.
    pub window_state: Option<BrowserWindowState>,
    /// Active tab details, when present.
    pub tab: Option<BrowserTab>,
}

/// Result of matching a browser foreground window against connected streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserLink {
    /// No stream is currently connected for this browser family.
    NoStream,
    /// Streams exist for this family, but zero or multiple matched.
    Ambiguous,
    /// Exactly one connected stream matched the foreground window.
    Linked(BrowserStreamRef),
}

/// Retention level for a persisted web interval row (`none` writes no web row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebLevel {
    /// Retain domain and tab title.
    Full,
    /// Retain domain only; tab title is redacted.
    DomainOnly,
}

impl WebLevel {
    /// Returns the schema code for `web_intervals.web_level`.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Full => WEB_LEVEL_FULL_CODE,
            Self::DomainOnly => WEB_LEVEL_DOMAIN_ONLY_CODE,
        }
    }
}

/// Strips one known trailing browser suffix from a foreground window title.
#[must_use]
pub fn strip_browser_title_suffix(family: BrowserFamily, title: &str) -> &str {
    for suffix in family.title_suffixes() {
        if let Some(stripped) = title.strip_suffix(suffix) {
            return stripped;
        }
    }
    title
}

/// Matches a foreground browser window against connected stream snapshots.
///
/// Candidates are streams of `family` that are focused, not minimized, and carry
/// a tab with a non-empty title. A candidate matches when the stripped window
/// title equals the tab title or one is a prefix of the other.
#[must_use]
pub fn link_browser_window<'a>(
    family: BrowserFamily,
    window_title: Option<&str>,
    streams: impl IntoIterator<Item = (BrowserStreamRef, &'a BrowserSnapshot)>,
) -> BrowserLink {
    let stripped = window_title
        .map(|title| strip_browser_title_suffix(family, title))
        .filter(|title| !title.is_empty());
    let mut saw_family_stream = false;
    let mut matched: Option<BrowserStreamRef> = None;
    let mut multiple = false;

    for (stream, snapshot) in streams {
        if stream.family != family {
            continue;
        }
        saw_family_stream = true;
        let Some(window_stripped) = stripped else {
            continue;
        };
        if snapshot.focus != BrowserFocus::Focused {
            continue;
        }
        if snapshot.window_state == Some(BrowserWindowState::Minimized) {
            continue;
        }
        let Some(tab) = &snapshot.tab else {
            continue;
        };
        let Some(tab_title) = tab.title.text().filter(|title| !title.is_empty()) else {
            continue;
        };
        if titles_match(window_stripped, tab_title) {
            if matched.is_some() {
                multiple = true;
            } else {
                matched = Some(stream);
            }
        }
    }

    if !saw_family_stream {
        BrowserLink::NoStream
    } else if multiple {
        BrowserLink::Ambiguous
    } else if let Some(stream) = matched {
        BrowserLink::Linked(stream)
    } else {
        BrowserLink::Ambiguous
    }
}

fn titles_match(window_title: &str, tab_title: &str) -> bool {
    window_title == tab_title
        || window_title.starts_with(tab_title)
        || tab_title.starts_with(window_title)
}

#[derive(Debug, Clone)]
struct ConnectedBrowser {
    stream: BrowserStreamRef,
    snapshot: BrowserSnapshot,
}

/// In-memory latest browser snapshots indexed by stream key.
#[derive(Debug, Default)]
pub(super) struct BrowserStreams {
    streams: BTreeMap<BrowserStreamKey, ConnectedBrowser>,
}

impl BrowserStreams {
    pub(super) fn update(&mut self, stream: BrowserStreamRef, snapshot: BrowserSnapshot) {
        self.streams
            .insert(stream.id, ConnectedBrowser { stream, snapshot });
    }

    pub(super) fn remove(&mut self, id: BrowserStreamKey) {
        self.streams.remove(&id);
    }

    pub(super) fn resolve<'a>(
        &'a self,
        exe_name: Option<&str>,
        window_title: Option<&str>,
        shell_context: &BrowserContext,
    ) -> super::privacy::ResolvedBrowser<'a> {
        let Some(family) = exe_name.and_then(BrowserFamily::from_exe_name) else {
            return match shell_context {
                BrowserContext::NotBrowser => super::privacy::ResolvedBrowser::NotBrowser,
                BrowserContext::Offline => super::privacy::ResolvedBrowser::Offline,
            };
        };
        match link_browser_window(
            family,
            window_title,
            self.streams
                .values()
                .map(|entry| (entry.stream, &entry.snapshot)),
        ) {
            BrowserLink::NoStream => super::privacy::ResolvedBrowser::Offline,
            BrowserLink::Ambiguous => super::privacy::ResolvedBrowser::Unlinked,
            BrowserLink::Linked(stream) => {
                let entry = self
                    .streams
                    .get(&stream.id)
                    .expect("linked stream exists in store");
                let tab = entry
                    .snapshot
                    .tab
                    .as_ref()
                    .expect("linked stream carries an active tab");
                super::privacy::ResolvedBrowser::Linked {
                    stream,
                    window_id: entry.snapshot.window_id,
                    tab,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream_ref(id: i64, family: BrowserFamily) -> BrowserStreamRef {
        BrowserStreamRef {
            id: BrowserStreamKey::new(id),
            family,
        }
    }

    fn snapshot(
        focus: BrowserFocus,
        window_state: Option<BrowserWindowState>,
        title: Option<&str>,
    ) -> BrowserSnapshot {
        BrowserSnapshot {
            focus,
            window_id: Some(10),
            window_state,
            tab: title.map(|value| BrowserTab {
                id: 1,
                domain: Some("example.com".into()),
                title: Title::readable(value),
                audible: BrowserAudible::Silent,
            }),
        }
    }

    #[test]
    fn family_maps_known_executables_round_trip() {
        for family in [
            BrowserFamily::Chrome,
            BrowserFamily::Edge,
            BrowserFamily::Firefox,
        ] {
            assert_eq!(
                BrowserFamily::from_exe_name(family.exe_name()),
                Some(family)
            );
        }
        assert_eq!(
            BrowserFamily::from_exe_name("CHROME.EXE"),
            Some(BrowserFamily::Chrome)
        );
        assert_eq!(BrowserFamily::from_exe_name("notepad.exe"), None);
    }

    #[test]
    fn linking_matches_exact_prefix_both_ways_and_known_suffixes() {
        let chrome = stream_ref(1, BrowserFamily::Chrome);
        let snap = snapshot(
            BrowserFocus::Focused,
            Some(BrowserWindowState::Normal),
            Some("Rust Documentation"),
        );
        assert_eq!(
            link_browser_window(
                BrowserFamily::Chrome,
                Some("Rust Documentation - Google Chrome"),
                [(chrome, &snap)],
            ),
            BrowserLink::Linked(chrome)
        );
        // Chromium profile segment before suffix matches via stripped.starts_with(tab_title).
        assert_eq!(
            link_browser_window(
                BrowserFamily::Chrome,
                Some("Rust Documentation - Work - Google Chrome"),
                [(chrome, &snap)],
            ),
            BrowserLink::Linked(chrome)
        );
        // Truncated window title matches via tab_title.starts_with(stripped).
        assert_eq!(
            link_browser_window(
                BrowserFamily::Chrome,
                Some("Rust Doc - Google Chrome"),
                [(chrome, &snap)],
            ),
            BrowserLink::Linked(chrome)
        );

        let edge = stream_ref(2, BrowserFamily::Edge);
        assert_eq!(
            link_browser_window(
                BrowserFamily::Edge,
                Some("Rust Documentation - Work - Microsoft\u{200b} Edge"),
                [(edge, &snap)],
            ),
            BrowserLink::Linked(edge)
        );
        assert_eq!(
            link_browser_window(
                BrowserFamily::Edge,
                Some("Rust Documentation - Microsoft Edge"),
                [(edge, &snap)],
            ),
            BrowserLink::Linked(edge)
        );

        let firefox = stream_ref(3, BrowserFamily::Firefox);
        assert_eq!(
            link_browser_window(
                BrowserFamily::Firefox,
                Some("Rust Documentation \u{2014} Mozilla Firefox"),
                [(firefox, &snap)],
            ),
            BrowserLink::Linked(firefox)
        );
        assert_eq!(
            link_browser_window(
                BrowserFamily::Firefox,
                Some("Rust Documentation - Mozilla Firefox"),
                [(firefox, &snap)],
            ),
            BrowserLink::Linked(firefox)
        );
    }

    #[test]
    fn linking_rejects_ambiguous_minimized_unfocused_and_family_mismatch() {
        let chrome_one = stream_ref(1, BrowserFamily::Chrome);
        let chrome_two = stream_ref(2, BrowserFamily::Chrome);
        let focused = snapshot(
            BrowserFocus::Focused,
            Some(BrowserWindowState::Normal),
            Some("Inbox"),
        );
        // Two profiles with the same title -> Ambiguous.
        assert_eq!(
            link_browser_window(
                BrowserFamily::Chrome,
                Some("Inbox - Google Chrome"),
                [(chrome_one, &focused), (chrome_two, &focused)],
            ),
            BrowserLink::Ambiguous
        );

        // Minimized or unfocused streams are excluded from candidates.
        let minimized = snapshot(
            BrowserFocus::Focused,
            Some(BrowserWindowState::Minimized),
            Some("Inbox"),
        );
        let unfocused = snapshot(
            BrowserFocus::Unfocused,
            Some(BrowserWindowState::Normal),
            Some("Inbox"),
        );
        assert_eq!(
            link_browser_window(
                BrowserFamily::Chrome,
                Some("Inbox - Google Chrome"),
                [(chrome_one, &minimized)],
            ),
            BrowserLink::Ambiguous
        );
        assert_eq!(
            link_browser_window(
                BrowserFamily::Chrome,
                Some("Inbox - Google Chrome"),
                [(chrome_one, &unfocused)],
            ),
            BrowserLink::Ambiguous
        );
        assert_eq!(
            link_browser_window(
                BrowserFamily::Chrome,
                Some("Inbox - Google Chrome"),
                [(chrome_one, &minimized), (chrome_two, &focused)],
            ),
            BrowserLink::Linked(chrome_two)
        );

        // Stream from another family -> NoStream for Firefox.
        assert_eq!(
            link_browser_window(
                BrowserFamily::Firefox,
                Some("Inbox \u{2014} Mozilla Firefox"),
                [(chrome_one, &focused)],
            ),
            BrowserLink::NoStream
        );

        // Empty window or tab titles never link.
        let empty_tab = snapshot(
            BrowserFocus::Focused,
            Some(BrowserWindowState::Normal),
            Some(""),
        );
        assert_eq!(
            link_browser_window(
                BrowserFamily::Chrome,
                Some("Inbox - Google Chrome"),
                [(chrome_one, &empty_tab)],
            ),
            BrowserLink::Ambiguous
        );
        assert_eq!(
            link_browser_window(
                BrowserFamily::Chrome,
                Some(" - Google Chrome"),
                [(chrome_one, &focused)],
            ),
            BrowserLink::Ambiguous
        );
    }

    #[test]
    fn linking_handles_chinese_and_emoji_titles_and_debug_redacts_secrets() {
        let chrome = stream_ref(1, BrowserFamily::Chrome);
        let title = "项目规划 🚀 讨论";
        let snap = BrowserSnapshot {
            focus: BrowserFocus::Focused,
            window_id: Some(5),
            window_state: Some(BrowserWindowState::Maximized),
            tab: Some(BrowserTab {
                id: 9,
                domain: Some("secret-bank.example".into()),
                title: Title::readable(title),
                audible: BrowserAudible::Audible,
            }),
        };
        assert_eq!(
            link_browser_window(
                BrowserFamily::Chrome,
                Some("项目规划 🚀 讨论 - 个人 - Google Chrome"),
                [(chrome, &snap)],
            ),
            BrowserLink::Linked(chrome)
        );
        let debug = format!("{snap:?}");
        assert!(!debug.contains("secret-bank.example"));
        assert!(!debug.contains("项目规划"));
        assert!(!debug.contains("🚀"));
    }
}
