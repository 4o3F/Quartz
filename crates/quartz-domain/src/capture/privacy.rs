//! Field removal before captured values enter write descriptions or effects.

use super::{
    BrowserAudible, BrowserStreamKey, BrowserStreamRef, BrowserTab, OpenWebInterval, RowId,
    Timestamp, TitleStatus, WebLevel, WindowFacts,
    browser::{INTERNAL_BROWSER_DOMAIN, LOCAL_FILE_BROWSER_DOMAIN},
};
use crate::{
    domain_name::DomainName,
    rules::{
        Action, ExplanationId, Field, FieldSnapshot, PrivacyAction, WebAction,
        registry::RuleSnapshot,
    },
    text::NfcText,
};
use std::fmt;

const CAPTURE_EXCLUDED_RULE_ID: &str = "builtin.capture_excluded@1";
const PRIVACY_LABEL_CAPTURE_EXCLUDED: &str = "privacy.label.capture_excluded";
const BROWSER_PRIVATE_RULE_ID: &str = "builtin.browser_private@1";
const PRIVACY_LABEL_PRIVATE_WINDOW: &str = "privacy.label.private_window";
const SESSION_PRIVACY_MODE_RULE_ID: &str = "session.privacy_mode@1";
const SESSION_NO_TITLES_RULE_ID: &str = "session.no_titles@1";
const PRIVACY_LABEL_TITLE_HIDDEN: &str = "privacy.label.title_hidden";
const WEB_CONTEXT_UNKNOWN_RULE_ID: &str = "builtin.web_context_unknown@1";
const WEB_UNLINKED_RULE_ID: &str = "builtin.web_unlinked@1";

/// An explicit system-signal result, including query failure or unavailable support.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SystemSignal {
    #[default]
    /// The shell could not determine this system signal.
    Unknown,
    /// The shell confirmed that the signal is absent.
    Absent,
    /// The shell confirmed that the signal is present.
    Present,
}
/// Browser classification and extension availability for privacy evaluation.
#[derive(Debug, Clone, Default)]
pub enum BrowserContext {
    /// This application is not a browser.
    #[default]
    NotBrowser,
    /// Extension is unavailable; dependent hidden fields are discarded immediately.
    Offline,
}
/// Session and recognizer inputs captured with one observation.
#[derive(Debug, Clone, Default)]
pub struct PrivacyContext {
    /// Display affinity recognizer result, with Unknown distinct from Absent.
    pub capture_excluded: SystemSignal,
    /// Additional private-window signal resolved by the shell.
    pub private_window: SystemSignal,
    /// User session privacy mode.
    pub mode: PrivacyMode,
    /// Browser context used by web-dependent privacy rules.
    pub browser: BrowserContext,
}
/// User-selected field retention mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PrivacyMode {
    #[default]
    /// Use the configured rule-driven retention policy.
    Normal,
    /// Drop foreground titles and all correlated web content.
    Private,
    /// Drop titles and retain web domains at most.
    NoTitles,
}
/// Foreground-field retention summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CaptureLevel {
    /// Retain all allowed foreground fields.
    Full,
    /// Retain application identity while dropping its title.
    TitleHidden,
    /// Retain only time boundaries and privacy explanation.
    Excluded,
}
/// Historical explanation saved on a downgraded row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacyExplanation {
    /// Rule explanation identifier, including revision.
    pub rule_id: String,
    /// Builtin localization key or user text snapshot.
    pub label: Option<PrivacyLabel>,
}
/// Historical user label or stable builtin localization key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivacyLabel {
    /// A stable builtin localization key.
    Key(String),
    /// A historical user-provided label snapshot.
    Text(String),
}
/// Allowed foreground fields after evaluating one immutable rule snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedFields {
    /// Summary for the foreground row.
    pub level: CaptureLevel,
    /// Capped title, absent after removal.
    pub title: Option<String>,
    /// Read or privacy status.
    pub title_status: TitleStatus,
    /// Historical downgrade explanation.
    pub privacy: Option<PrivacyExplanation>,
    /// Merged web action for the later browser integration.
    pub web: WebAction,
}
/// Core-resolved browser stream context for one foreground window evaluation.
#[derive(Debug)]
pub(super) enum ResolvedBrowser<'a> {
    NotBrowser,
    Offline,
    Unlinked,
    Linked {
        stream: BrowserStreamRef,
        window_id: Option<i64>,
        tab: &'a BrowserTab,
    },
}

/// Sanitized web fields retained for one linked foreground interval piece.
///
/// Debug formatting deliberately redacts domain and tab title text.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct CapturedWeb {
    pub stream: BrowserStreamKey,
    pub domain: Option<String>,
    pub tab_title: Option<String>,
    pub tab_title_status: TitleStatus,
    pub tab_id: i64,
    pub browser_window_id: Option<i64>,
    pub audible: BrowserAudible,
    pub web_level: WebLevel,
    pub privacy: Option<PrivacyExplanation>,
}

impl fmt::Debug for CapturedWeb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapturedWeb")
            .field("stream", &self.stream)
            .field("domain_len", &self.domain.as_ref().map(String::len))
            .field("tab_title_len", &self.tab_title.as_ref().map(String::len))
            .field("tab_title_status", &self.tab_title_status)
            .field("tab_id", &self.tab_id)
            .field("browser_window_id", &self.browser_window_id)
            .field("audible", &self.audible)
            .field("web_level", &self.web_level)
            .field("privacy", &self.privacy)
            .finish()
    }
}

impl CapturedWeb {
    pub(super) fn to_open(&self, id: RowId, interval: RowId, start: Timestamp) -> OpenWebInterval {
        OpenWebInterval {
            id,
            interval,
            stream: self.stream,
            start,
            domain: self.domain.clone(),
            tab_title: self.tab_title.clone(),
            tab_title_status: self.tab_title_status,
            tab_id: self.tab_id,
            browser_window_id: self.browser_window_id,
            audible: self.audible,
            web_level: self.web_level,
            privacy: self.privacy.clone(),
        }
    }

    pub(super) fn restrict_to(mut self, floor: &CapturedFields) -> Option<Self> {
        if floor.level == CaptureLevel::Excluded || floor.web == WebAction::None {
            return None;
        }
        if floor.title_status == TitleStatus::Redacted || floor.web == WebAction::DomainOnly {
            self.web_level = WebLevel::DomainOnly;
            self.tab_title = None;
            self.tab_title_status = TitleStatus::Redacted;
            if self.privacy.is_none() {
                self.privacy = floor.privacy.clone();
            }
        }
        Some(self)
    }
}

/// Combined foreground and web privacy evaluation result.
#[derive(Debug)]
pub(super) struct CaptureEvaluation {
    /// Effective fields emitted for the foreground row.
    pub fields: CapturedFields,
    /// Sticky floor excluding transient unlinked browser hiding.
    pub sticky_floor: CapturedFields,
    /// Linked web fields, absent when unlinked, offline, excluded, or web-none.
    pub web: Option<CapturedWeb>,
}

impl WindowFacts {
    pub(super) fn capture(
        &self,
        context: &PrivacyContext,
        browser: ResolvedBrowser<'_>,
        snapshot: &RuleSnapshot,
    ) -> CaptureEvaluation {
        let fields = self.rule_fields(&browser);
        let evaluated = snapshot.privacy().evaluate(&fields);
        let mut policy = AppliedPrivacy::from_evaluation(evaluated, snapshot);
        policy.apply_recognizers(context);
        policy.apply_session(context.mode);
        match browser {
            ResolvedBrowser::NotBrowser => {
                let sticky_floor = policy.capture_fields(&self.title);
                CaptureEvaluation {
                    fields: sticky_floor.clone(),
                    sticky_floor,
                    web: None,
                }
            }
            ResolvedBrowser::Offline => {
                policy.apply_offline_web(snapshot);
                let sticky_floor = policy.capture_fields(&self.title);
                CaptureEvaluation {
                    fields: sticky_floor.clone(),
                    sticky_floor,
                    web: None,
                }
            }
            ResolvedBrowser::Unlinked => {
                let sticky_floor = policy.capture_fields(&self.title);
                policy.apply_unlinked_web();
                let fields = policy.capture_fields(&self.title);
                CaptureEvaluation {
                    fields,
                    sticky_floor,
                    web: None,
                }
            }
            ResolvedBrowser::Linked {
                stream,
                window_id,
                tab,
            } => {
                let sticky_floor = policy.capture_fields(&self.title);
                let web = policy.capture_web(stream, window_id, tab);
                CaptureEvaluation {
                    fields: sticky_floor.clone(),
                    sticky_floor,
                    web,
                }
            }
        }
    }
    fn rule_fields(&self, browser: &ResolvedBrowser<'_>) -> FieldSnapshot {
        let mut fields = FieldSnapshot::default();
        for (field, value) in [
            (Field::IdentityKey, Some(self.app.key())),
            (Field::ExeName, self.exe_name.clone()),
            (Field::DisplayName, Some(self.display_name.clone())),
            (Field::WindowClass, Some(self.class.clone())),
            (Field::RawTitle, self.title.text().map(str::to_owned)),
        ] {
            if let Some(value) = value {
                fields.text.insert(field, NfcText::new(&value));
            }
        }
        if let ResolvedBrowser::Linked { tab, .. } = browser {
            if let Some(tab_title) = tab.title.text() {
                fields.text.insert(Field::TabTitle, NfcText::new(tab_title));
            }
            if let Some(raw_domain) = tab.domain.as_deref()
                && !is_special_browser_domain(raw_domain)
                && let Ok(domain) = DomainName::parse(raw_domain)
            {
                fields.domain = Some(domain);
            }
        }
        fields
    }
}

fn is_special_browser_domain(raw: &str) -> bool {
    raw == INTERNAL_BROWSER_DOMAIN || raw == LOCAL_FILE_BROWSER_DOMAIN
}

fn normalize_web_domain(raw: &str) -> String {
    if is_special_browser_domain(raw) {
        raw.to_owned()
    } else if let Ok(domain) = DomainName::parse(raw) {
        domain.as_ascii().to_owned()
    } else {
        raw.to_owned()
    }
}

fn explanation_from_snapshot(rule_id: String, snapshot: &RuleSnapshot) -> PrivacyExplanation {
    let label = snapshot
        .privacy()
        .privacy_label(
            &rule_id
                .parse::<ExplanationId>()
                .expect("compiled explanation identifiers have valid targets and revisions"),
        )
        .expect("selected explanation belongs to this snapshot");
    PrivacyExplanation {
        rule_id,
        label: label
            .key
            .map(PrivacyLabel::Key)
            .or_else(|| label.text.map(PrivacyLabel::Text)),
    }
}

#[derive(Debug)]
struct AppliedPrivacy {
    action: PrivacyAction,
    explanation: Option<PrivacyExplanation>,
    web_explanation: Option<PrivacyExplanation>,
}
impl AppliedPrivacy {
    fn from_evaluation(evaluated: crate::rules::Evaluation, snapshot: &RuleSnapshot) -> Self {
        let action = evaluated.privacy;
        let source = if action.app == Action::Drop {
            evaluated.privacy_sources.app
        } else {
            evaluated.privacy_sources.window_title
        };
        let explanation = source.map(|rule_id| explanation_from_snapshot(rule_id, snapshot));
        let web_explanation = evaluated
            .privacy_sources
            .web
            .map(|rule_id| explanation_from_snapshot(rule_id, snapshot));
        Self {
            action,
            explanation,
            web_explanation,
        }
    }
    fn restrict(&mut self, restriction: PrivacyAction, id: &str, label: &str) {
        let merged = self.action.merge(restriction);
        let build = || PrivacyExplanation {
            rule_id: id.into(),
            label: Some(PrivacyLabel::Key(label.into())),
        };
        if merged.app > self.action.app || merged.window_title > self.action.window_title {
            self.explanation = Some(build());
        }
        if merged.web > self.action.web {
            self.web_explanation = Some(build());
        }
        self.action = merged;
    }
    fn apply_recognizers(&mut self, context: &PrivacyContext) {
        if context.capture_excluded == SystemSignal::Present {
            self.restrict(
                PrivacyAction {
                    window_title: Action::Drop,
                    ..PrivacyAction::default()
                },
                CAPTURE_EXCLUDED_RULE_ID,
                PRIVACY_LABEL_CAPTURE_EXCLUDED,
            );
        }
        if context.private_window == SystemSignal::Present {
            self.restrict(
                PrivacyAction {
                    window_title: Action::Drop,
                    web: WebAction::None,
                    ..PrivacyAction::default()
                },
                BROWSER_PRIVATE_RULE_ID,
                PRIVACY_LABEL_PRIVATE_WINDOW,
            );
        }
    }
    fn apply_session(&mut self, mode: PrivacyMode) {
        match mode {
            PrivacyMode::Normal => {}
            PrivacyMode::Private => self.restrict(
                PrivacyAction {
                    window_title: Action::Drop,
                    web: WebAction::None,
                    ..PrivacyAction::default()
                },
                SESSION_PRIVACY_MODE_RULE_ID,
                PRIVACY_LABEL_PRIVATE_WINDOW,
            ),
            PrivacyMode::NoTitles => self.restrict(
                PrivacyAction {
                    window_title: Action::Drop,
                    web: WebAction::DomainOnly,
                    ..PrivacyAction::default()
                },
                SESSION_NO_TITLES_RULE_ID,
                PRIVACY_LABEL_TITLE_HIDDEN,
            ),
        }
    }
    fn apply_offline_web(&mut self, snapshot: &RuleSnapshot) {
        if snapshot.privacy().requires_web_privacy() {
            self.restrict(
                PrivacyAction {
                    window_title: Action::Drop,
                    ..PrivacyAction::default()
                },
                WEB_CONTEXT_UNKNOWN_RULE_ID,
                PRIVACY_LABEL_TITLE_HIDDEN,
            );
        }
    }
    fn apply_unlinked_web(&mut self) {
        self.restrict(
            PrivacyAction {
                window_title: Action::Drop,
                ..PrivacyAction::default()
            },
            WEB_UNLINKED_RULE_ID,
            PRIVACY_LABEL_TITLE_HIDDEN,
        );
    }
    fn capture_fields(&self, title: &super::Title) -> CapturedFields {
        let level = if self.action.app == Action::Drop {
            CaptureLevel::Excluded
        } else if self.action.window_title == Action::Drop {
            CaptureLevel::TitleHidden
        } else {
            CaptureLevel::Full
        };
        CapturedFields {
            level,
            title: if self.action.window_title == Action::Keep {
                title.text().map(str::to_owned)
            } else {
                None
            },
            title_status: if self.action.window_title == Action::Drop {
                TitleStatus::Redacted
            } else {
                title.status()
            },
            privacy: if level == CaptureLevel::Full {
                None
            } else {
                self.explanation.clone()
            },
            web: self.action.web,
        }
    }
    fn capture_web(
        self,
        stream: BrowserStreamRef,
        window_id: Option<i64>,
        tab: &BrowserTab,
    ) -> Option<CapturedWeb> {
        if self.action.app == Action::Drop || self.action.web == WebAction::None {
            return None;
        }
        let drop_title =
            self.action.window_title == Action::Drop || self.action.web == WebAction::DomainOnly;
        let (web_level, tab_title, tab_title_status, privacy) = if drop_title {
            (
                WebLevel::DomainOnly,
                None,
                TitleStatus::Redacted,
                self.web_explanation.or(self.explanation),
            )
        } else {
            (
                WebLevel::Full,
                tab.title.text().map(str::to_owned),
                tab.title.status(),
                None,
            )
        };
        let domain = tab
            .domain
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(normalize_web_domain);
        Some(CapturedWeb {
            stream: stream.id,
            domain,
            tab_title,
            tab_title_status,
            tab_id: tab.id,
            browser_window_id: window_id,
            audible: tab.audible,
            web_level,
            privacy,
        })
    }
}

impl CapturedFields {
    pub(super) fn restrict_to(&mut self, previous: &Self) {
        if previous.level > self.level {
            self.level = previous.level;
            self.privacy = previous.privacy.clone();
        }
        if previous.title_status == TitleStatus::Redacted {
            self.title = None;
            self.title_status = TitleStatus::Redacted;
        }
        self.web = self.web.max(previous.web);
    }
}
