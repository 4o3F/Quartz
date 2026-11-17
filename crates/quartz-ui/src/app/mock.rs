//! Static mock data for one plausible workday until the protocol mapping exists.

use std::time::Duration;

use super::model::{
    AppExplanation, AppId, AppInfo, AppWeek, BuiltinCategory, CalendarDate, Category,
    CategoryColor, CategoryId, CategoryName, ClassificationSource, Day, Glyph, PaletteSlot,
    Segment, SegmentId, StateBlock, StateKind, Tendency, TimeOfDay, TimeSpan, Weekday,
};

const DEV: CategoryId = CategoryId(1);
const COMM: CategoryId = CategoryId(2);
const BROWSE: CategoryId = CategoryId(3);
const FUN: CategoryId = CategoryId(4);
const NONE: CategoryId = CategoryId(5);
const LEARN: CategoryId = CategoryId(6);

const CODE: AppId = AppId(1);
const EDGE: AppId = AppId(2);
const FEISHU: AppId = AppId(3);
const TERM: AppId = AppId(4);
const WECHAT: AppId = AppId(5);

/// A foreground segment description for the mock table.
struct Row {
    app: AppId,
    category: CategoryId,
    from: (u32, u32),
    to: (u32, u32),
    title: &'static str,
    domain: Option<&'static str>,
    document: Option<&'static str>,
    rule: &'static str,
    merged: u32,
}

impl Row {
    const fn web(mut self, domain: &'static str) -> Self {
        self.domain = Some(domain);
        self.rule = domain;
        self
    }

    const fn doc(mut self, document: &'static str) -> Self {
        self.document = Some(document);
        self
    }

    const fn merged(mut self, count: u32) -> Self {
        self.merged = count;
        self
    }
}

fn rows() -> Vec<Row> {
    vec![
        Row::new(FEISHU, COMM, (8, 33), (8, 55), "日报 — 飞书").doc("日报"),
        Row::new(CODE, DEV, (8, 55), (9, 15), "main.rs — quartz")
            .doc("main.rs")
            .merged(4),
        Row::new(EDGE, DEV, (9, 15), (9, 20), "gpui — Rust").web("docs.rs"),
        Row::new(CODE, DEV, (9, 20), (10, 5), "timeline.rs — quartz")
            .doc("timeline.rs")
            .merged(7),
        Row::new(TERM, DEV, (10, 5), (10, 10), "cargo build"),
        Row::new(CODE, DEV, (10, 10), (10, 40), "app.rs — quartz")
            .doc("app.rs")
            .merged(5),
        Row::new(WECHAT, COMM, (10, 40), (10, 50), "产品群"),
        Row::new(EDGE, BROWSE, (10, 50), (11, 0), "热门问题 — 知乎").web("zhihu.com"),
        Row::new(CODE, DEV, (11, 0), (11, 45), "main.rs — quartz")
            .doc("main.rs")
            .merged(6),
        Row::new(FEISHU, COMM, (11, 45), (11, 50), "站会纪要").doc("站会纪要"),
        Row::new(CODE, DEV, (11, 50), (12, 0), "07-ui.md — Quartz").doc("07-ui.md"),
        Row::new(FEISHU, COMM, (13, 20), (13, 35), "站会").doc("站会纪要"),
        Row::new(EDGE, BROWSE, (13, 35), (13, 40), "Hacker News").web("news.ycombinator.com"),
        Row::new(EDGE, NONE, (13, 40), (13, 55), "内部系统").web("intranet.example"),
        Row::new(EDGE, BROWSE, (13, 55), (14, 0), "Rust 博客").web("blog.rust-lang.org"),
        Row::new(CODE, DEV, (14, 0), (14, 50), "timeline.rs — quartz")
            .doc("timeline.rs")
            .merged(8),
        Row::new(EDGE, DEV, (14, 50), (14, 55), "Pull requests · 4o3F/Quartz").web("github.com"),
        Row::new(CODE, DEV, (14, 55), (15, 5), "app.rs — quartz").doc("app.rs"),
        Row::new(EDGE, FUN, (15, 5), (15, 12), "哔哩哔哩").web("bilibili.com"),
        Row::new(EDGE, FUN, (15, 32), (15, 50), "哔哩哔哩").web("bilibili.com"),
        Row::new(WECHAT, COMM, (15, 50), (15, 55), "产品群"),
        Row::new(EDGE, BROWSE, (15, 55), (16, 5), "Rust 博客").web("blog.rust-lang.org"),
        Row::new(EDGE, FUN, (16, 5), (16, 20), "YouTube").web("youtube.com"),
        Row::new(WECHAT, COMM, (16, 20), (16, 35), "产品群"),
        Row::new(
            CODE,
            DEV,
            (16, 35),
            (17, 10),
            "main.rs — quartz — Visual Studio Code",
        )
        .doc("main.rs")
        .merged(9),
        Row::new(TERM, DEV, (17, 10), (17, 15), "cargo test"),
        Row::new(FEISHU, COMM, (18, 20), (18, 45), "评审 — 飞书").doc("评审"),
    ]
}

fn minutes<const N: usize>(values: [u64; N]) -> [Duration; N] {
    values.map(|value| Duration::from_secs(value * 60))
}

impl Category {
    fn sample(
        id: CategoryId,
        name: CategoryName,
        color: CategoryColor,
        tendency: Tendency,
    ) -> Self {
        Category {
            id,
            name,
            color,
            tendency,
        }
    }
}

impl AppInfo {
    fn sample(id: AppId, name: &str, short: &str, exe: &str, glyph: Glyph) -> Self {
        AppInfo {
            id,
            name: name.to_owned(),
            short_name: short.to_owned(),
            exe: exe.to_owned(),
            glyph,
        }
    }
}

impl Row {
    const fn new(
        app: AppId,
        category: CategoryId,
        from: (u32, u32),
        to: (u32, u32),
        title: &'static str,
    ) -> Self {
        Row {
            app,
            category,
            from,
            to,
            title,
            domain: None,
            document: None,
            rule: "",
            merged: 1,
        }
    }
}

impl TimeSpan {
    fn from_hours_minutes(from: (u32, u32), to: (u32, u32)) -> Self {
        TimeSpan::new(TimeOfDay::hm(from.0, from.1), TimeOfDay::hm(to.0, to.1))
    }
}

impl StateBlock {
    fn sample(
        from: (u32, u32),
        to: (u32, u32),
        kind: StateKind,
        category: Option<CategoryId>,
        pending: AwayConfirmation,
    ) -> Self {
        StateBlock {
            span: TimeSpan::from_hours_minutes(from, to),
            kind,
            category,
            pending_away: pending == AwayConfirmation::Pending,
            clock_anomaly: false,
        }
    }
}

impl AppExplanation {
    fn sample(
        app: AppId,
        rule_name: &str,
        priority: u32,
        field: &str,
        expected: &str,
        actual: &str,
    ) -> Self {
        AppExplanation {
            app,
            rule_set: "builtin.classification".to_owned(),
            rule_name: rule_name.to_owned(),
            priority,
            field: field.to_owned(),
            operator: "equals".to_owned(),
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        }
    }
}

impl Day {
    /// One workday on Monday, November 16.
    #[must_use]
    pub fn sample() -> Self {
        let segments = rows()
            .into_iter()
            .zip(1_u32..)
            .map(|(row, id)| Segment {
                id: SegmentId(id),
                span: TimeSpan::from_hours_minutes(row.from, row.to),
                app: row.app,
                category: row.category,
                title: row.title.to_owned(),
                domain: row.domain.map(str::to_owned),
                document: row.document.map(str::to_owned),
                source: if row.category == NONE {
                    ClassificationSource::Unmatched
                } else {
                    ClassificationSource::Rule {
                        condition: if row.rule.is_empty() {
                            row.app.sample_executable()
                        } else {
                            row.rule.to_owned()
                        },
                    }
                },
                merged_count: row.merged,
            })
            .collect();
        Day {
            date: CalendarDate {
                month: 11,
                day: 16,
                weekday: Weekday::Mon,
            },
            categories: vec![
                Category::sample(
                    DEV,
                    CategoryName::Builtin(BuiltinCategory::Development),
                    CategoryColor::Palette(PaletteSlot::Cat8),
                    Tendency::Focus,
                ),
                Category::sample(
                    LEARN,
                    CategoryName::Builtin(BuiltinCategory::Learning),
                    CategoryColor::Palette(PaletteSlot::Cat1),
                    Tendency::Focus,
                ),
                Category::sample(
                    COMM,
                    CategoryName::Builtin(BuiltinCategory::Communication),
                    CategoryColor::Palette(PaletteSlot::Cat3),
                    Tendency::Neutral,
                ),
                Category::sample(
                    BROWSE,
                    CategoryName::Custom("浏览".to_owned()),
                    CategoryColor::Palette(PaletteSlot::Cat2),
                    Tendency::Neutral,
                ),
                Category::sample(
                    FUN,
                    CategoryName::Builtin(BuiltinCategory::Entertainment),
                    CategoryColor::Palette(PaletteSlot::Cat13),
                    Tendency::Distraction,
                ),
                Category::sample(
                    NONE,
                    CategoryName::Builtin(BuiltinCategory::Uncategorized),
                    CategoryColor::Neutral,
                    Tendency::Neutral,
                ),
            ],
            apps: vec![
                AppInfo::sample(
                    CODE,
                    "Visual Studio Code",
                    "VS Code",
                    "code.exe",
                    Glyph::Editor,
                ),
                AppInfo::sample(
                    EDGE,
                    "Microsoft Edge",
                    "Microsoft Edge",
                    "msedge.exe",
                    Glyph::Browser,
                ),
                AppInfo::sample(FEISHU, "飞书", "飞书", "Feishu.exe", Glyph::Messenger),
                AppInfo::sample(
                    TERM,
                    "Windows Terminal",
                    "终端",
                    "WindowsTerminal.exe",
                    Glyph::Terminal,
                ),
                AppInfo::sample(WECHAT, "微信", "微信", "WeChat.exe", Glyph::Chat),
            ],
            segments,
            states: vec![
                StateBlock::sample(
                    (12, 0),
                    (13, 20),
                    StateKind::Locked,
                    None,
                    AwayConfirmation::Confirmed,
                ),
                StateBlock::sample(
                    (15, 12),
                    (15, 32),
                    StateKind::Idle,
                    Some(DEV),
                    AwayConfirmation::Pending,
                ),
                StateBlock::sample(
                    (17, 15),
                    (18, 20),
                    StateKind::Excluded,
                    None,
                    AwayConfirmation::Confirmed,
                ),
            ],
            week_totals: vec![
                AppWeek {
                    app: CODE,
                    previous_days: minutes([200, 230, 165, 220, 25, 0]),
                },
                AppWeek {
                    app: EDGE,
                    previous_days: minutes([95, 80, 120, 70, 40, 20]),
                },
                AppWeek {
                    app: FEISHU,
                    previous_days: minutes([60, 75, 50, 66, 0, 0]),
                },
                AppWeek {
                    app: TERM,
                    previous_days: minutes([30, 25, 20, 35, 5, 0]),
                },
                AppWeek {
                    app: WECHAT,
                    previous_days: minutes([20, 18, 25, 12, 30, 15]),
                },
            ],
            explanations: vec![
                AppExplanation::sample(
                    CODE,
                    "VS Code 算开发",
                    50,
                    "app.exe_name",
                    "code.exe",
                    "code.exe",
                ),
                AppExplanation::sample(
                    EDGE,
                    "github.com 算开发",
                    60,
                    "web.domain",
                    "github.com",
                    "github.com",
                ),
                AppExplanation::sample(
                    FEISHU,
                    "飞书算沟通",
                    50,
                    "app.exe_name",
                    "Feishu.exe",
                    "Feishu.exe",
                ),
                AppExplanation::sample(
                    TERM,
                    "终端算开发",
                    50,
                    "app.exe_name",
                    "WindowsTerminal.exe",
                    "WindowsTerminal.exe",
                ),
                AppExplanation::sample(
                    WECHAT,
                    "微信算沟通",
                    50,
                    "app.exe_name",
                    "WeChat.exe",
                    "WeChat.exe",
                ),
            ],
        }
    }
}

impl AppId {
    fn sample_executable(self) -> String {
        match self {
            CODE => "code.exe",
            EDGE => "msedge.exe",
            FEISHU => "Feishu.exe",
            TERM => "WindowsTerminal.exe",
            WECHAT => "WeChat.exe",
            _ => "",
        }
        .to_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AwayConfirmation {
    Confirmed,
    Pending,
}
