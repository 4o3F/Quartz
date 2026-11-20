//! Foreground observation history, privacy evaluation and per-instance title activity.

use super::{
    BrowserContext, CaptureLevel, CapturedFields, Foreground, HookTarget, IntervalFact,
    MonoInstant, NoForegroundReason, ObservationSource, PrivacyContext, PrivacyMode, Timestamp,
    TitleActivity, WindowFacts, WindowId, WindowInstance, WindowInstances, limits,
};
use crate::rules::registry::RuleSnapshot;
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug)]
pub(super) struct ForegroundReading {
    pub source: ObservationSource,
    pub foreground: Foreground,
    pub at: Timestamp,
}
#[derive(Debug)]
pub(super) enum Retention {
    Fresh,
    Cached(CapturedFields),
}
#[derive(Debug)]
pub(super) struct EvaluationContext<'a> {
    pub rules: &'a RuleSnapshot,
    pub retention: Retention,
}
impl<'a> EvaluationContext<'a> {
    pub(super) fn fresh(rules: &'a RuleSnapshot) -> Self {
        Self {
            rules,
            retention: Retention::Fresh,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TitleFingerprint {
    NoWindow,
    Window([u8; 32]),
}
#[derive(Debug)]
struct ObservedWindow {
    facts: Box<WindowFacts>,
    privacy: PrivacyContext,
    instance: WindowInstance,
    fields: CapturedFields,
    fingerprint: [u8; 32],
}
#[derive(Debug)]
enum ObservedForeground {
    Window(Box<ObservedWindow>),
    None(NoForegroundReason),
}
#[derive(Debug)]
pub(super) struct EvaluatedForeground {
    observed: ObservedForeground,
    pub at: Timestamp,
    pub source: ObservationSource,
}
impl ForegroundReading {
    pub(super) fn evaluate(
        self,
        windows: &mut WindowInstances,
        context: EvaluationContext<'_>,
    ) -> EvaluatedForeground {
        let observed = match self.foreground {
            Foreground::None(reason) => ObservedForeground::None(reason),
            Foreground::Window {
                mut facts,
                mut privacy,
            } => {
                let fingerprint = facts.title.fingerprint();
                let instance = windows.observe(&facts, self.at);
                let mut fields = facts.capture(&privacy, context.rules);
                if let Retention::Cached(floor) = context.retention {
                    fields.restrict_to(&floor);
                }
                if fields.level != CaptureLevel::Full {
                    facts.title.redact();
                }
                privacy.clean_web(&fields);
                ObservedForeground::Window(Box::new(ObservedWindow {
                    facts,
                    privacy,
                    instance,
                    fields,
                    fingerprint,
                }))
            }
        };
        EvaluatedForeground {
            observed,
            at: self.at,
            source: self.source,
        }
    }
}
impl PrivacyContext {
    fn clean_web(&mut self, fields: &CapturedFields) {
        if fields.web == crate::rules::WebAction::None {
            self.browser = BrowserContext::Offline;
        } else if fields.web == crate::rules::WebAction::DomainOnly
            && let BrowserContext::Ready(web) = &mut self.browser
        {
            web.text.remove(&crate::rules::Field::TabTitle);
        }
    }
}
impl EvaluatedForeground {
    pub(super) fn fact(&self) -> IntervalFact {
        match &self.observed {
            ObservedForeground::None(reason) => IntervalFact::NoForeground(*reason),
            ObservedForeground::Window(row) => IntervalFact::Window {
                instance: (row.fields.level != CaptureLevel::Excluded).then_some(row.instance.id),
                fields: row.fields.clone(),
            },
        }
    }
    pub(super) fn instance(&self) -> Option<&WindowInstance> {
        match &self.observed {
            ObservedForeground::Window(row) => Some(&row.instance),
            ObservedForeground::None(_) => None,
        }
    }
    pub(super) fn allowed_instance(&self) -> Option<&WindowInstance> {
        match &self.observed {
            ObservedForeground::Window(row) if row.fields.level != CaptureLevel::Excluded => {
                Some(&row.instance)
            }
            _ => None,
        }
    }
    pub(super) fn fingerprint(&self) -> TitleFingerprint {
        match &self.observed {
            ObservedForeground::Window(row) => TitleFingerprint::Window(row.fingerprint),
            _ => TitleFingerprint::NoWindow,
        }
    }
}
#[derive(Debug)]
struct CachedWindow {
    facts: Box<WindowFacts>,
    privacy: PrivacyContext,
    instance: WindowId,
    fingerprint: [u8; 32],
}
#[derive(Debug)]
enum LastForeground {
    Unobserved,
    Window(CachedWindow),
    None(NoForegroundReason),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessBinding {
    Unbind,
    Process(super::ProcessId),
}
#[derive(Debug)]
pub(super) struct ForegroundTracker {
    last: LastForeground,
    binding: Option<ProcessBinding>,
    changes: BTreeMap<WindowId, VecDeque<MonoInstant>>,
}
impl ForegroundTracker {
    pub(super) fn new() -> Self {
        Self {
            last: LastForeground::Unobserved,
            binding: None,
            changes: BTreeMap::new(),
        }
    }
    pub(super) fn rebind(&mut self, foreground: &Foreground) -> RebindDecision {
        let target = HookTarget::from_foreground(foreground);
        let binding = match target {
            HookTarget::Process { process, .. } => ProcessBinding::Process(process),
            HookTarget::Unbind => ProcessBinding::Unbind,
        };
        if self.binding != Some(binding) {
            self.binding = Some(binding);
            RebindDecision::Rebind(target)
        } else {
            RebindDecision::Unchanged
        }
    }
    pub(super) fn current(&self) -> Option<Foreground> {
        match &self.last {
            LastForeground::Unobserved => None,
            LastForeground::None(reason) => Some(Foreground::None(*reason)),
            LastForeground::Window(row) => Some(Foreground::Window {
                facts: row.facts.clone(),
                privacy: row.privacy.clone(),
            }),
        }
    }
    pub(super) fn app(&self) -> Option<&super::AppIdentity> {
        match &self.last {
            LastForeground::Window(row) => Some(&row.facts.app),
            _ => None,
        }
    }
    pub(super) fn instance(&self) -> Option<WindowId> {
        match &self.last {
            LastForeground::Window(row) => Some(row.instance),
            _ => None,
        }
    }
    pub(super) fn fingerprint(&self) -> TitleFingerprint {
        match &self.last {
            LastForeground::Window(row) => TitleFingerprint::Window(row.fingerprint),
            _ => TitleFingerprint::NoWindow,
        }
    }
    pub(super) fn restore_fingerprint(&mut self, fingerprint: TitleFingerprint) {
        if let (LastForeground::Window(row), TitleFingerprint::Window(value)) =
            (&mut self.last, fingerprint)
        {
            row.fingerprint = value;
        }
    }
    pub(super) fn forget(&mut self) {
        self.last = LastForeground::Unobserved;
        self.binding = None;
    }
    pub(super) fn expire_privacy(&mut self) {
        if let LastForeground::Window(row) = &mut self.last {
            row.privacy.mode = PrivacyMode::Normal;
        }
    }
    pub(super) fn remember(&mut self, observation: EvaluatedForeground) {
        self.last = match observation.observed {
            ObservedForeground::None(reason) => LastForeground::None(reason),
            ObservedForeground::Window(row) => LastForeground::Window(CachedWindow {
                facts: row.facts,
                privacy: row.privacy,
                instance: row.instance.id,
                fingerprint: row.fingerprint,
            }),
        };
    }
    pub(super) fn title_activity(&mut self, instance: WindowId, at: MonoInstant) -> TitleActivity {
        let changes = self.changes.entry(instance).or_default();
        changes.push_back(at);
        while changes
            .front()
            .is_some_and(|time| time.advance(limits::TITLE_STORM_WINDOW) < at)
        {
            changes.pop_front();
        }
        if changes.len() > limits::TITLE_STORM_COUNT {
            TitleActivity::Storm
        } else {
            TitleActivity::Ordinary
        }
    }
}
#[derive(Debug)]
pub(super) enum RebindDecision {
    Unchanged,
    Rebind(HookTarget),
}
impl HookTarget {
    pub(super) fn from_foreground(foreground: &Foreground) -> Self {
        match foreground {
            Foreground::Window { facts, .. } => Self::Process {
                window: facts.handle,
                process: facts.process,
            },
            Foreground::None(_) => Self::Unbind,
        }
    }
}
