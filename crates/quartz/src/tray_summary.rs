//! Cached tray aggregates and localized presentation without database access.

use quartz_i18n::{Language, MessageKey};
use quartz_protocol::{
    ApplicationRank, BuiltinCategory, CategoryDuration, CategoryRef, DayMetrics, ForegroundState,
    QueryDayResponse,
};

/// Duration units shared by the tray's compact displays.
pub(crate) const MINUTE_MS: i64 = 60 * 1000;
const MINUTES_PER_HOUR: i64 = 60;
/// 07 §1.3 keeps the four leading categories in the compact menu.
const TOP_CATEGORY_COUNT: usize = 4;
/// Windows reserves one UTF-16 unit in szTip for the terminator (07 §1.2).
const TOOLTIP_UNITS: usize = 127;
const PERCENT: f64 = 100.0;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TraySummary {
    pub metrics: DayMetrics,
    pub categories: Vec<CategoryDuration>,
    pub recent: Option<ApplicationRank>,
}
impl TraySummary {
    pub(crate) fn from_day(day: QueryDayResponse) -> Option<Self> {
        let metrics = day.metrics?;
        let mut categories: Vec<CategoryDuration> = Vec::new();
        for hour in day.hours? {
            for entry in hour.categories {
                if let Some(existing) = categories
                    .iter_mut()
                    .find(|row| row.category == entry.category)
                {
                    existing.duration_ms += entry.duration_ms;
                } else {
                    categories.push(entry);
                }
            }
        }
        categories.retain(|row| row.duration_ms > 0);
        // Stable ordering preserves the query's category ordering when durations tie.
        categories.sort_by_key(|row| std::cmp::Reverse(row.duration_ms));
        Some(Self {
            metrics,
            categories,
            recent: None,
        })
    }
    pub(crate) fn today_text(&self, language: Language) -> String {
        MessageKey::new("tray.today").in_language(language).replace(
            "%{duration}",
            &TrayText::duration(self.metrics.active_ms, language),
        )
    }
    pub(crate) fn categories_text(&self, language: Language) -> String {
        if self.categories.is_empty() {
            return MessageKey::new("tray.no_activity")
                .in_language(language)
                .into_owned();
        }
        self.categories
            .iter()
            .take(TOP_CATEGORY_COUNT)
            .map(|row| {
                MessageKey::new("today.legend_item")
                    .in_language(language)
                    .replace("%{name}", &TrayText::category(&row.category, language))
                    .replace(
                        "%{duration}",
                        &TrayText::duration(row.duration_ms, language),
                    )
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
    pub(crate) fn recent_text(&self, language: Language) -> String {
        let value = self
            .recent
            .as_ref()
            .map(|row| {
                MessageKey::new("today.legend_item")
                    .in_language(language)
                    .replace("%{name}", &TrayText::menu_value(&row.app.display_name))
                    .replace("%{duration}", &TrayText::duration(row.active_ms, language))
            })
            .unwrap_or_else(|| {
                MessageKey::new("tray.no_activity")
                    .in_language(language)
                    .into_owned()
            });
        MessageKey::new("tray.recent")
            .in_language(language)
            .replace("%{activity}", &value)
    }
    pub(crate) fn tooltip(
        &self,
        foreground: Option<&ForegroundState>,
        now_ms: i64,
        language: Language,
    ) -> String {
        let mut text = MessageKey::new("tray.summary")
            .in_language(language)
            .replace(
                "%{duration}",
                &TrayText::duration(self.metrics.active_ms, language),
            )
            .replace(
                "%{focus}",
                &format!("{:.0}", self.metrics.focus_ratio * PERCENT),
            )
            .replace(
                "%{distraction}",
                &format!("{:.0}", self.metrics.distract_ratio * PERCENT),
            );
        if let Some(view) = foreground
            && let Some(app) = &view.app
        {
            let identity = match app {
                quartz_protocol::AppIdentity::Exe(value) => {
                    let name = value
                        .rsplit(['\\', '/'])
                        .next()
                        .expect("path has a final component");
                    name.strip_suffix(".exe").unwrap_or(name)
                }
                quartz_protocol::AppIdentity::Aumid(value)
                | quartz_protocol::AppIdentity::Unresolved(value) => value,
            };
            let current = match &view.title {
                Some(title) => {
                    format!("{} · {}", TrayText::plain(identity), TrayText::plain(title))
                }
                None => TrayText::plain(identity),
            };
            let line = MessageKey::new("tray.current")
                .in_language(language)
                .replace("%{current}", &current)
                .replace(
                    "%{minutes}",
                    &(now_ms.saturating_sub(view.interval_start_utc_ms).max(0) / MINUTE_MS)
                        .to_string(),
                );
            text.push('\n');
            text.push_str(&line);
        }
        TrayText::truncate(&text)
    }
}

pub(crate) struct TrayText;
impl TrayText {
    pub(crate) fn duration(milliseconds: i64, language: Language) -> String {
        let minutes = milliseconds.max(0) / MINUTE_MS;
        let hours = minutes / MINUTES_PER_HOUR;
        let minutes = minutes % MINUTES_PER_HOUR;
        let key = if hours == 0 {
            "tray.duration_m"
        } else if minutes == 0 {
            "tray.duration_h"
        } else {
            "tray.duration_hm"
        };
        MessageKey::new(key)
            .in_language(language)
            .replace("%{hours}", &hours.to_string())
            .replace("%{minutes}", &minutes.to_string())
    }
    fn category(category: &CategoryRef, language: Language) -> String {
        let key = match category {
            CategoryRef::Unclassified => "category.builtin.uncategorized",
            CategoryRef::Classified { category } => {
                if let Some(name) = &category.name {
                    return Self::menu_value(name);
                }
                match category.builtin_key {
                    Some(BuiltinCategory::Development) => "category.builtin.development",
                    Some(BuiltinCategory::Learning) => "category.builtin.learning",
                    Some(BuiltinCategory::Communication) => "category.builtin.communication",
                    Some(BuiltinCategory::Entertainment) => "category.builtin.entertainment",
                    Some(BuiltinCategory::System) => "category.builtin.system",
                    None => "category.builtin.uncategorized",
                }
            }
        };
        MessageKey::new(key).in_language(language).into_owned()
    }
    fn plain(value: &str) -> String {
        value
            .chars()
            .filter(|character| !character.is_control())
            .collect()
    }
    fn menu_value(value: &str) -> String {
        Self::plain(value).replace('&', "&&")
    }
    pub(crate) fn truncate(value: &str) -> String {
        if value.encode_utf16().count() <= TOOLTIP_UNITS {
            return value.to_owned();
        }
        let mut text = String::new();
        let mut units = 0;
        for character in value.chars() {
            if units + character.len_utf16() >= TOOLTIP_UNITS {
                break;
            }
            units += character.len_utf16();
            text.push(character);
        }
        text.push('…');
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn summary() -> TraySummary {
        TraySummary {
            metrics: DayMetrics {
                active_ms: (5 * 60 + 12) * MINUTE_MS,
                focus_ratio: 0.62,
                distract_ratio: 0.18,
                unclassified_ms: 0,
                longest_focus_ms: 0,
                switches_per_hour: 0.0,
            },
            categories: Vec::new(),
            recent: None,
        }
    }
    #[test]
    fn tooltip_localizes_summary_foreground_elapsed_and_hidden_fields() {
        let summary = summary();
        let mut foreground = ForegroundState {
            app: Some(quartz_protocol::AppIdentity::Exe("C:\\Code.exe".into())),
            title: Some("quartz".into()),
            interval_start_utc_ms: 0,
        };
        for language in [Language::English, Language::SimplifiedChinese] {
            let text = summary.tooltip(Some(&foreground), 23 * MINUTE_MS, language);
            let expected = match language {
                Language::English => {
                    "Quartz · Today 5h12m · Focus 62% · Distraction 18%\nCurrent: Code · quartz · 23 min"
                }
                Language::SimplifiedChinese => {
                    "Quartz · 今日 5h12m · 专注 62% · 分心 18%\n当前：Code · quartz 已 23 分钟"
                }
                #[cfg(debug_assertions)]
                Language::PseudoEnglish => unreachable!("production catalogs"),
            };
            assert_eq!(text, expected);
            crate::tray_tooltip::TrayTooltipText::try_from(text.as_str())
                .expect("fits shell buffer");
        }
        foreground.title = None;
        let hidden = summary.tooltip(Some(&foreground), MINUTE_MS, Language::English);
        assert!(hidden.contains("Code"));
        assert!(!hidden.contains("quartz"));
        foreground.app = None;
        foreground.title = Some("must remain hidden".into());
        assert_eq!(
            summary.tooltip(Some(&foreground), MINUTE_MS, Language::English),
            summary.tooltip(None, MINUTE_MS, Language::English)
        );
    }
    #[test]
    fn long_tooltips_truncate_at_complete_utf16_characters_in_both_languages() {
        let foreground = ForegroundState {
            app: Some(quartz_protocol::AppIdentity::Unresolved("Editor".into())),
            title: Some(format!("\0\n{}", "😀中文".repeat(100))),
            interval_start_utc_ms: 0,
        };
        for language in [Language::English, Language::SimplifiedChinese] {
            let text = summary().tooltip(Some(&foreground), 0, language);
            assert!(text.ends_with('…'));
            assert!(text.encode_utf16().count() <= TOOLTIP_UNITS);
            assert_eq!(text.matches('\n').count(), 1);
            assert!(!text.contains('\0'));
            crate::tray_tooltip::TrayTooltipText::try_from(text.as_str())
                .expect("truncated tooltip fits");
        }
        assert_eq!(
            TrayText::truncate(&"a".repeat(TOOLTIP_UNITS)),
            "a".repeat(TOOLTIP_UNITS)
        );
        assert_eq!(
            TrayText::truncate(&"a".repeat(TOOLTIP_UNITS + 1)),
            format!("{}…", "a".repeat(TOOLTIP_UNITS - 1))
        );
    }
    #[test]
    fn maximum_formatted_tooltips_fit_shell_buffer_in_every_language() {
        use quartz_domain::capture::limits;
        let mut maximum = summary();
        maximum.metrics.active_ms = i64::MAX;
        maximum.metrics.focus_ratio = 1.0;
        maximum.metrics.distract_ratio = 1.0;
        let maximum_minutes = i64::MAX / MINUTE_MS;
        for &language in Language::ALL {
            let duration = TrayText::duration(i64::MAX, language);
            assert!(duration.contains(&(maximum_minutes / MINUTES_PER_HOUR).to_string()));
            let summary = maximum.tooltip(None, i64::MAX, language);
            assert!(summary.contains(&duration));
            assert_eq!(summary.matches("100%").count(), 2);
            crate::tray_tooltip::TrayTooltipText::try_from(summary.as_str())
                .expect("maximum summary fits");
            let short = ForegroundState {
                app: Some(quartz_protocol::AppIdentity::Unresolved("App".into())),
                title: None,
                interval_start_utc_ms: i64::MIN,
            };
            let elapsed = maximum.tooltip(Some(&short), i64::MAX, language);
            let current = MessageKey::new("tray.current")
                .in_language(language)
                .replace("%{current}", "App")
                .replace("%{minutes}", &maximum_minutes.to_string());
            assert!(
                current.contains(&maximum_minutes.to_string()),
                "maximum elapsed minutes for {language}"
            );
            let unbounded = format!("{summary}\n{current}");
            if unbounded.encode_utf16().count() <= TOOLTIP_UNITS {
                assert_eq!(elapsed, unbounded);
            } else {
                assert!(elapsed.ends_with('…'));
                assert!(unbounded.starts_with(elapsed.trim_end_matches('…')));
            }
            crate::tray_tooltip::TrayTooltipText::try_from(elapsed.as_str())
                .expect("maximum elapsed time fits");
            for character in ['界', '😀'] {
                let content = character
                    .to_string()
                    .repeat(limits::TITLE_UTF16_UNITS / character.len_utf16());
                assert_eq!(content.encode_utf16().count(), limits::TITLE_UTF16_UNITS);
                let foreground = ForegroundState {
                    app: Some(quartz_protocol::AppIdentity::Unresolved(content.clone())),
                    title: Some(content),
                    interval_start_utc_ms: i64::MIN,
                };
                let text = maximum.tooltip(Some(&foreground), i64::MAX, language);
                assert!(text.ends_with('…'), "long formatted content for {language}");
                let units = text.encode_utf16().count();
                assert!((TOOLTIP_UNITS - char::MAX.len_utf16()..=TOOLTIP_UNITS).contains(&units));
                crate::tray_tooltip::TrayTooltipText::try_from(text.as_str())
                    .expect("formatted tooltip fits");
            }
        }
    }
}
