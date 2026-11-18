//! Field removal before captured values enter write descriptions or effects.

use super::{TitleStatus, WindowFacts};
use crate::{
    rules::{
        Action, ExplanationId, Field, FieldSnapshot, PrivacyAction, WebAction,
        registry::RuleSnapshot,
    },
    text::NfcText,
};

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
/// Foreground browser context supplied by a later browser shell.
#[derive(Debug, Clone, Default)]
pub enum BrowserContext {
    /// This application is not a browser.
    #[default]
    NotBrowser,
    /// Extension is unavailable; dependent hidden fields are discarded immediately.
    Offline,
    /// Context has not arrived; the core holds outputs for at most two seconds.
    Pending,
    /// A correlated context snapshot; only web fields are used.
    Ready(FieldSnapshot),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
impl WindowFacts {
    pub(super) fn capture(
        &self,
        context: &PrivacyContext,
        snapshot: &RuleSnapshot,
    ) -> CapturedFields {
        let fields = self.rule_fields(context);
        let evaluated = snapshot.privacy().evaluate(&fields);
        let mut policy = AppliedPrivacy::from_evaluation(evaluated, snapshot);
        policy.apply_recognizers(context);
        policy.apply_session(context.mode);
        policy.apply_missing_web(context, snapshot);
        policy.capture(&self.title)
    }
    fn rule_fields(&self, context: &PrivacyContext) -> FieldSnapshot {
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
        if let BrowserContext::Ready(web) = &context.browser {
            fields.domain = web.domain.clone();
            for field in [Field::TabTitle, Field::BrowserProfile] {
                if let Some(value) = web.text.get(&field) {
                    fields.text.insert(field, value.clone());
                }
            }
        }
        fields
    }
}
#[derive(Debug)]
struct AppliedPrivacy {
    action: PrivacyAction,
    explanation: Option<PrivacyExplanation>,
}
impl AppliedPrivacy {
    fn from_evaluation(evaluated: crate::rules::Evaluation, snapshot: &RuleSnapshot) -> Self {
        let action = evaluated.privacy;
        let source = if action.app == Action::Drop {
            evaluated.privacy_sources.app
        } else {
            evaluated.privacy_sources.window_title
        };
        let explanation = source.map(|rule_id| {
            let label =
                snapshot
                    .privacy()
                    .privacy_label(&rule_id.parse::<ExplanationId>().expect(
                        "compiled explanation identifiers have valid targets and revisions",
                    ))
                    .expect("selected explanation belongs to this snapshot");
            PrivacyExplanation {
                rule_id,
                label: label
                    .key
                    .map(PrivacyLabel::Key)
                    .or_else(|| label.text.map(PrivacyLabel::Text)),
            }
        });
        Self {
            action,
            explanation,
        }
    }
    fn restrict(&mut self, restriction: PrivacyAction, id: &str, label: &str) {
        let merged = self.action.merge(restriction);
        if merged.app > self.action.app || merged.window_title > self.action.window_title {
            self.explanation = Some(PrivacyExplanation {
                rule_id: id.into(),
                label: Some(PrivacyLabel::Key(label.into())),
            });
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
                "builtin.capture_excluded@1",
                "privacy.label.capture_excluded",
            );
        }
        if context.private_window == SystemSignal::Present {
            self.restrict(
                PrivacyAction {
                    window_title: Action::Drop,
                    web: WebAction::None,
                    ..PrivacyAction::default()
                },
                "builtin.browser_private@1",
                "privacy.label.private_window",
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
                "session.privacy_mode@1",
                "privacy.label.private_window",
            ),
            PrivacyMode::NoTitles => self.restrict(
                PrivacyAction {
                    window_title: Action::Drop,
                    web: WebAction::DomainOnly,
                    ..PrivacyAction::default()
                },
                "session.no_titles@1",
                "privacy.label.title_hidden",
            ),
        }
    }
    fn apply_missing_web(&mut self, context: &PrivacyContext, snapshot: &RuleSnapshot) {
        if snapshot.privacy().requires_web_privacy()
            && matches!(
                context.browser,
                BrowserContext::Offline | BrowserContext::Pending
            )
        {
            self.restrict(
                PrivacyAction {
                    window_title: Action::Drop,
                    ..PrivacyAction::default()
                },
                "builtin.web_context_unknown@1",
                "privacy.label.title_hidden",
            );
        }
    }
    fn capture(self, title: &super::Title) -> CapturedFields {
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
                self.explanation
            },
            web: self.action.web,
        }
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
