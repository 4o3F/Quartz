//! Mock data for one plausible workday, for the preview window and screenshots until the protocol
//! mapping exists. It goes through the same [`Day::new`] validation real data will.

use std::{collections::BTreeMap, num::NonZeroU32, ops::Range, time::Duration};

use crate::model::{
    AppId, AppInfo, AwayLabel, BuiltinCategory, Category, CategoryColor, CategoryId,
    ClassificationSource, ClockAnomaly, Day, DayParts, EARLIER_DAYS, EarlierDays, Glyph,
    PaletteSlot, Rule, RuleCondition, RuleField, RuleId, RuleOperator, Segment, SegmentId,
    StateBlock, StateKind, Tendency, TimeOfDay, TimeSpan,
};

/// The sample day and the segments the preview states open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SampleDay {
    pub(crate) day: Day,
    /// The interval the details preview selects.
    pub(crate) details_segment: SegmentId,
    /// The interval the hover preview shows a tooltip for.
    pub(crate) hover_segment: SegmentId,
}

const DEVELOPMENT: CategoryId = CategoryId::new(1);
const COMMUNICATION: CategoryId = CategoryId::new(2);
const BROWSING: CategoryId = CategoryId::new(3);
const ENTERTAINMENT: CategoryId = CategoryId::new(4);
const UNCATEGORIZED: CategoryId = CategoryId::new(5);
const LEARNING: CategoryId = CategoryId::new(6);
const SYSTEM: CategoryId = CategoryId::new(7);

const VS_CODE: AppId = AppId::new(1);
const EDGE: AppId = AppId::new(2);
const FEISHU: AppId = AppId::new(3);
const TERMINAL: AppId = AppId::new(4);
const WECHAT: AppId = AppId::new(5);

const RULE_SET: &str = "builtin.classification";
const EXE_RULE_PRIORITY: u32 = 50;
const SITE_RULE_PRIORITY: u32 = 60;

/// A rule that classifies an application by its executable.
struct ExeRule {
    id: RuleId,
    name: &'static str,
    category: CategoryId,
}

/// One application with everything the sample knows about it, so each fact is written once.
struct SampleApp {
    id: AppId,
    name: &'static str,
    short_name: &'static str,
    exe: &'static str,
    glyph: Glyph,
    earlier_minutes: [u64; EARLIER_DAYS],
    /// `None` for applications classified by what they show, such as browsers.
    rule: Option<ExeRule>,
}

const APPS: [SampleApp; 5] = [
    SampleApp {
        id: VS_CODE,
        name: "Visual Studio Code",
        short_name: "VS Code",
        exe: "code.exe",
        glyph: Glyph::Editor,
        earlier_minutes: [200, 230, 165, 220, 25, 0],
        rule: Some(ExeRule {
            id: RuleId::new(1),
            name: "VS Code 算开发",
            category: DEVELOPMENT,
        }),
    },
    SampleApp {
        id: EDGE,
        name: "Microsoft Edge",
        short_name: "Microsoft Edge",
        exe: "msedge.exe",
        glyph: Glyph::Browser,
        earlier_minutes: [95, 80, 120, 70, 40, 20],
        rule: None,
    },
    SampleApp {
        id: FEISHU,
        name: "飞书",
        short_name: "飞书",
        exe: "Feishu.exe",
        glyph: Glyph::Messenger,
        earlier_minutes: [60, 75, 50, 66, 0, 0],
        rule: Some(ExeRule {
            id: RuleId::new(2),
            name: "飞书算沟通",
            category: COMMUNICATION,
        }),
    },
    SampleApp {
        id: TERMINAL,
        name: "Windows Terminal",
        short_name: "终端",
        exe: "WindowsTerminal.exe",
        glyph: Glyph::Terminal,
        earlier_minutes: [30, 25, 20, 35, 5, 0],
        rule: Some(ExeRule {
            id: RuleId::new(3),
            name: "终端算开发",
            category: DEVELOPMENT,
        }),
    },
    SampleApp {
        id: WECHAT,
        name: "微信",
        short_name: "微信",
        exe: "WeChat.exe",
        glyph: Glyph::Chat,
        earlier_minutes: [20, 18, 25, 12, 30, 15],
        rule: Some(ExeRule {
            id: RuleId::new(4),
            name: "微信算沟通",
            category: COMMUNICATION,
        }),
    },
];

/// A rule that classifies browser time by domain.
struct SiteRule {
    id: RuleId,
    domain: &'static str,
    name: &'static str,
    category: CategoryId,
}

const SITES: [SiteRule; 7] = [
    SiteRule {
        id: RuleId::new(11),
        domain: "docs.rs",
        name: "docs.rs 算开发",
        category: DEVELOPMENT,
    },
    SiteRule {
        id: RuleId::new(12),
        domain: "github.com",
        name: "github.com 算开发",
        category: DEVELOPMENT,
    },
    SiteRule {
        id: RuleId::new(13),
        domain: "zhihu.com",
        name: "知乎算浏览",
        category: BROWSING,
    },
    SiteRule {
        id: RuleId::new(14),
        domain: "news.ycombinator.com",
        name: "Hacker News 算浏览",
        category: BROWSING,
    },
    SiteRule {
        id: RuleId::new(15),
        domain: "blog.rust-lang.org",
        name: "Rust 博客算浏览",
        category: BROWSING,
    },
    SiteRule {
        id: RuleId::new(16),
        domain: "bilibili.com",
        name: "哔哩哔哩算娱乐",
        category: ENTERTAINMENT,
    },
    SiteRule {
        id: RuleId::new(17),
        domain: "youtube.com",
        name: "YouTube 算娱乐",
        category: ENTERTAINMENT,
    },
];

/// How a row got its category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowSource {
    /// The application's executable rule.
    Exe,
    /// The site rule of the row's domain.
    Domain,
    /// No rule matched.
    Unmatched,
}

/// Which preview state opens a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewRole {
    Details,
    Hover,
}

/// A foreground segment description for the mock table.
struct Row {
    app: AppId,
    span: TimeSpan,
    title: &'static str,
    domain: Option<&'static str>,
    document: Option<&'static str>,
    source: RowSource,
    merged: u32,
    role: Option<PreviewRole>,
}

const fn hm(hours: u32, minutes: u32) -> TimeOfDay {
    TimeOfDay::hm(hours, minutes)
}

impl Row {
    /// An application classified by its executable.
    const fn app(app: AppId, time: Range<TimeOfDay>, title: &'static str) -> Self {
        Self {
            app,
            span: TimeSpan::new(time.start, time.end),
            title,
            domain: None,
            document: None,
            source: RowSource::Exe,
            merged: 1,
            role: None,
        }
    }

    /// A browser tab classified by its domain.
    const fn site(domain: &'static str, time: Range<TimeOfDay>, title: &'static str) -> Self {
        Self {
            domain: Some(domain),
            source: RowSource::Domain,
            ..Self::app(EDGE, time, title)
        }
    }

    const fn unmatched(self) -> Self {
        Self {
            source: RowSource::Unmatched,
            ..self
        }
    }

    const fn doc(self, document: &'static str) -> Self {
        Self {
            document: Some(document),
            ..self
        }
    }

    const fn merged(self, count: u32) -> Self {
        Self {
            merged: count,
            ..self
        }
    }

    const fn opens(self, role: PreviewRole) -> Self {
        Self {
            role: Some(role),
            ..self
        }
    }

    fn segment(&self, id: SegmentId) -> Segment {
        let (category, source) = match self.source {
            RowSource::Exe => {
                let app = sample_app(self.app);
                let rule = app
                    .rule
                    .as_ref()
                    .expect("rows classified by executable belong to an application with a rule");
                (
                    rule.category,
                    ClassificationSource::Rule {
                        rule: rule.id,
                        actual: app.exe.to_owned(),
                    },
                )
            }
            RowSource::Domain => {
                let domain = self.domain.expect("site rows have a domain");
                let site = SITES
                    .iter()
                    .find(|site| site.domain == domain)
                    .expect("every sample domain has a site rule");
                (
                    site.category,
                    ClassificationSource::Rule {
                        rule: site.id,
                        actual: domain.to_owned(),
                    },
                )
            }
            RowSource::Unmatched => (UNCATEGORIZED, ClassificationSource::Unmatched),
        };
        Segment {
            id,
            span: self.span,
            app: self.app,
            category,
            title: self.title.to_owned(),
            domain: self.domain.map(str::to_owned),
            document: self.document.map(str::to_owned),
            source,
            merged: NonZeroU32::new(self.merged).expect("merged counts are positive"),
        }
    }
}

fn sample_app(id: AppId) -> &'static SampleApp {
    APPS.iter()
        .find(|app| app.id == id)
        .expect("rows name sample applications")
}

fn rows() -> [Row; 27] {
    [
        Row::app(FEISHU, hm(8, 33)..hm(8, 55), "日报 — 飞书").doc("日报"),
        Row::app(VS_CODE, hm(8, 55)..hm(9, 15), "main.rs — quartz")
            .doc("main.rs")
            .merged(4),
        Row::site("docs.rs", hm(9, 15)..hm(9, 20), "gpui — Rust"),
        Row::app(VS_CODE, hm(9, 20)..hm(10, 5), "timeline.rs — quartz")
            .doc("timeline.rs")
            .merged(7),
        Row::app(TERMINAL, hm(10, 5)..hm(10, 10), "cargo build"),
        Row::app(VS_CODE, hm(10, 10)..hm(10, 40), "app.rs — quartz")
            .doc("app.rs")
            .merged(5),
        Row::app(WECHAT, hm(10, 40)..hm(10, 50), "产品群"),
        Row::site("zhihu.com", hm(10, 50)..hm(11, 0), "热门问题 — 知乎"),
        Row::app(VS_CODE, hm(11, 0)..hm(11, 45), "main.rs — quartz")
            .doc("main.rs")
            .merged(6),
        Row::app(FEISHU, hm(11, 45)..hm(11, 50), "站会纪要").doc("站会纪要"),
        Row::app(VS_CODE, hm(11, 50)..hm(12, 0), "07-ui.md — Quartz").doc("07-ui.md"),
        Row::app(FEISHU, hm(13, 20)..hm(13, 35), "站会").doc("站会纪要"),
        Row::site(
            "news.ycombinator.com",
            hm(13, 35)..hm(13, 40),
            "Hacker News",
        ),
        Row::site("intranet.example", hm(13, 40)..hm(13, 55), "内部系统").unmatched(),
        Row::site("blog.rust-lang.org", hm(13, 55)..hm(14, 0), "Rust 博客"),
        Row::app(VS_CODE, hm(14, 0)..hm(14, 50), "timeline.rs — quartz")
            .doc("timeline.rs")
            .merged(8),
        Row::site(
            "github.com",
            hm(14, 50)..hm(14, 55),
            "Pull requests · 4o3F/Quartz",
        )
        .opens(PreviewRole::Hover),
        Row::app(VS_CODE, hm(14, 55)..hm(15, 5), "app.rs — quartz").doc("app.rs"),
        Row::site("bilibili.com", hm(15, 5)..hm(15, 12), "哔哩哔哩"),
        Row::site("bilibili.com", hm(15, 32)..hm(15, 50), "哔哩哔哩"),
        Row::app(WECHAT, hm(15, 50)..hm(15, 55), "产品群"),
        Row::site("blog.rust-lang.org", hm(15, 55)..hm(16, 5), "Rust 博客"),
        Row::site("youtube.com", hm(16, 5)..hm(16, 20), "YouTube"),
        Row::app(WECHAT, hm(16, 20)..hm(16, 35), "产品群"),
        Row::app(
            VS_CODE,
            hm(16, 35)..hm(17, 10),
            "main.rs — quartz — Visual Studio Code",
        )
        .doc("main.rs")
        .merged(9)
        .opens(PreviewRole::Details),
        Row::app(TERMINAL, hm(17, 10)..hm(17, 15), "cargo test"),
        Row::app(FEISHU, hm(18, 20)..hm(18, 45), "评审 — 飞书").doc("评审"),
    ]
}

/// Built-in categories in list order, with the user category "浏览" in the first free slot.
fn categories() -> Vec<Category> {
    let mut categories = vec![
        Category::builtin(DEVELOPMENT, BuiltinCategory::Development),
        Category::builtin(LEARNING, BuiltinCategory::Learning),
        Category::builtin(COMMUNICATION, BuiltinCategory::Communication),
        Category::builtin(ENTERTAINMENT, BuiltinCategory::Entertainment),
        Category::builtin(SYSTEM, BuiltinCategory::System),
        Category::builtin(UNCATEGORIZED, BuiltinCategory::Uncategorized),
    ];
    let used: Vec<CategoryColor> = categories.iter().map(|category| category.color).collect();
    let slot = PaletteSlot::first_unused(&used).expect("built-in categories leave free slots");
    let browsing = Category::custom(
        BROWSING,
        "浏览".to_owned(),
        CategoryColor::Palette(slot),
        Tendency::Neutral,
    );
    categories.insert(3, browsing);
    categories
}

fn rules() -> Vec<Rule> {
    let exe_rules = APPS.iter().filter_map(|app| {
        app.rule.as_ref().map(|rule| Rule {
            id: rule.id,
            rule_set: RULE_SET.to_owned(),
            name: rule.name.to_owned(),
            priority: EXE_RULE_PRIORITY,
            condition: RuleCondition {
                field: RuleField::AppExeName,
                operator: RuleOperator::Equals,
                value: app.exe.to_owned(),
            },
        })
    });
    let site_rules = SITES.iter().map(|site| Rule {
        id: site.id,
        rule_set: RULE_SET.to_owned(),
        name: site.name.to_owned(),
        priority: SITE_RULE_PRIORITY,
        condition: RuleCondition {
            field: RuleField::WebDomain,
            operator: RuleOperator::Equals,
            value: site.domain.to_owned(),
        },
    });
    exe_rules.chain(site_rules).collect()
}

fn states() -> Vec<StateBlock> {
    let state = |time: Range<TimeOfDay>, kind| StateBlock {
        span: TimeSpan::new(time.start, time.end),
        kind,
        clock: ClockAnomaly::None,
    };
    vec![
        state(hm(12, 0)..hm(13, 20), StateKind::Locked),
        state(
            hm(15, 12)..hm(15, 32),
            StateKind::Idle {
                preceding: Some(DEVELOPMENT),
                away: AwayLabel::Pending,
            },
        ),
        state(hm(17, 15)..hm(18, 20), StateKind::Excluded),
    ]
}

impl SampleDay {
    /// One workday on Monday, November 16, 2026.
    ///
    /// # Panics
    /// Panics when the sample tables contradict each other, which the tests catch.
    #[must_use]
    pub(crate) fn new() -> Self {
        let mut details_segment = None;
        let mut hover_segment = None;
        let segments = rows()
            .iter()
            .zip(1..)
            .map(|(row, id)| {
                let id = SegmentId::new(id);
                match row.role {
                    Some(PreviewRole::Details) => details_segment = Some(id),
                    Some(PreviewRole::Hover) => hover_segment = Some(id),
                    None => {}
                }
                row.segment(id)
            })
            .collect();
        let parts = DayParts {
            date: jiff::civil::date(2026, 11, 16),
            categories: categories(),
            apps: APPS
                .iter()
                .map(|app| AppInfo {
                    id: app.id,
                    name: app.name.to_owned(),
                    short_name: app.short_name.to_owned(),
                    exe: app.exe.to_owned(),
                    glyph: app.glyph,
                })
                .collect(),
            segments,
            states: states(),
            history: APPS
                .iter()
                .map(|app| {
                    let minutes = app
                        .earlier_minutes
                        .map(|minutes| Duration::from_secs(minutes * 60));
                    (app.id, EarlierDays(minutes))
                })
                .collect::<BTreeMap<_, _>>(),
            rules: rules(),
        };
        Self {
            day: Day::new(parts).expect("the sample day is consistent"),
            details_segment: details_segment.expect("a row opens the details preview"),
            hover_segment: hover_segment.expect("a row opens the hover preview"),
        }
    }
}

impl Default for SampleDay {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_day_is_valid_and_names_its_preview_segments() {
        let sample = SampleDay::new();
        let details = sample
            .day
            .segment(sample.details_segment)
            .expect("details segment exists");
        assert_eq!(details.span.start(), hm(16, 35));
        let hover = sample
            .day
            .segment(sample.hover_segment)
            .expect("hover segment exists");
        assert_eq!(hover.domain.as_deref(), Some("github.com"));
    }

    #[test]
    fn browsing_takes_the_first_free_palette_slot() {
        let sample = SampleDay::new();
        let browsing = sample.day.category(BROWSING);
        assert_eq!(
            browsing.color,
            CategoryColor::Palette(PaletteSlot::numbered(2))
        );
    }
}
