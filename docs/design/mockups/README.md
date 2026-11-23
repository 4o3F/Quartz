# Quartz UI mockups

Static HTML mockups (1280x800, zh-CN, no JS apart from a theme switch). Open any page with `?theme=light` or `?theme=dark`. `tokens.css` encodes 11-design-system tokens (header comment maps each variable to its token name); `mockup.css` holds shared component styles using only those variables. Screenshots are in `screenshots/{page}-{light,dark}.png`.

- `today.html`: Today page per 07 §3/§4/§10 with the 11 §5.4 layout (32 px custom title bar, 240 px navigation, date bar, status line with 100% category bar, timeline with category main track + per-app swimlanes, idle hatching, locked/excluded states, a hover tooltip, hourly stacked bars, application ranking, pending-away row).
- `today-details.html`: same page with the 360 px details sidebar (07 §8) for the selected interval (VS Code 16:35-17:10): raw-interval row, 7-day bars, common titles, deciding rule with a compact explain tree, actions. The navigation is collapsed to the 48 px rail.
- `rules.html`: categories and rules page per 07 §6: top pivot, rule-set tree (versions, switches, +/~/- diff badges, warnings), effective view (source and override per rule, stopped/overridden group), explain panel (rule set -> rule -> condition with actual values, effective vs shadowed).
- `rules-set.html` (extra): same page with the rule-set entries view, rule editor with live "past 7 days" preview and samples, import/export and "copy situation description".

## Design questions / ambiguities (where I had to choose)

1. Accent color is runtime (UISettings), not in 11. I used the Windows default blue as sample values (AccentDark1 #005FB8 / AccentLight2 #60CDFF / AccentDark2 #003E92 / AccentLight3 #99EBFF) for `accent.*` only. These four hex values are the only colors not literally in chapter 11.
2. Mica cannot be shown in static HTML. Window base = `bg.base`, main area = `fill.layer` over it (11 §2 fallback look). Real Mica will look slightly different.
3. 11 §3.3 says the dark plot area gets no card fill; I use `bg.base` as plot background in dark (timeline, hourly chart, category bar) and transparent in light. Confirm the light version should keep the card fill.
4. Details sidebar (360) + 240 px nav leaves a 680 px main area, which is too narrow for the status line and timeline. The two mockups with a right panel therefore use the collapsed 48 px rail. Should the nav auto-collapse when a right panel opens, or should the main area scroll/reflow instead?
5. Page top padding uses `space.xxxl` (32) per 11 §5.2; the pending-away row then sits at the bottom edge of 800 px. At 768 px tall windows the page scrolls; confirm that is acceptable.
6. Status line: 07 §3 gives no sizes. I used `type.title` for active time and `type.subtitle` for the others, with `type.body` units. The "未分类" item shows a chevron to hint it is clickable although 07 says "neutral text only". I also added a 100% category bar with legend inside the status card (07 §10 lists "分类占比" but 07 §3 does not place it).
7. "浏览" is not a built-in category (11 §3.3 lists 开发/学习/沟通/娱乐/系统与工具/未分类). I treated it as a user category taking the first unused palette slot (`cat.2`); 开发 = cat.8, 沟通 = cat.3, 娱乐 = cat.13, 学习 = cat.1.
8. Focus / distraction share: tendency of 开发 = focus, 娱乐 = distraction (07 does not define the formula); "每小时切换" is shown as an integer, rounded.
9. Idle intervals are only drawn on the category main track (as the previous category, 40% + hatch). Swimlanes show active segments only. 07 §4.1 does not say whether idle also appears in swimlanes.
10. Main track merges adjacent same-category active segments (07 §4.2 merges per pixel); the swimlane shows per-document segments, and merged same-window titles are not visible at this zoom.
11. Excluded/idle text is hidden when the block is too narrow; only blocks wide enough show "已排除 65 分钟" (11 §3.4).
12. App icons: 11 §7 says to show the exe icon as-is. Brand icons would add off-palette colors, so the mockups use neutral Fluent-style glyphs (code, globe, chat, send, terminal) as placeholders.
13. The 7-day chart in the details sidebar uses the app's category color; 07 §8 does not specify the chart style. "Create privacy rule" and "delete related data" are shown, with delete as a danger-colored icon button.
14. Rules page structure: 07 §6 does not say how categories / rules / unclassified queue / extraction / privacy are laid out. I used one pivot row (分类, 规则, 未分类队列 with a count badge, 文档提取规则, 隐私规则). The rule-set tree is shown for classification only (the other kinds are the secondary tabs).
15. The explain panel is placed in the details-sidebar slot of the rules page (07 §6 says it lives in the details sidebar) and the rule editor uses the same slot in `rules-set.html`. Condition lines show `field operator "value"` in Cascadia Mono, with the actual value below.
16. 07 §6 mentions both "preview 过去 7 天命中 X 段、Y 小时" and a sample list; I show both in the editor (three sample rows, no scrolling or paging).
17. Warning badges (上游已变更, 目标已删除) use `status.caution` / `status.critical` backgrounds. 07 says "no red warnings" for the user's behavior, so I assumed red is acceptable only for data-integrity warnings like deleted targets.
18. Segoe UI Variable Small/Text/Display families are used per 11 §4.1 with `Microsoft YaHei UI` as zh-CN fallback; `tnum` is enabled through `font-variant-numeric`. Browser rendering differs from DirectWrite/GPUI.

## Decisions (recorded in 07-ui.md)

The user accepted these answers to the questions above; 07-ui.md is authoritative where a mockup differs.

- Q4: opening a right-hand panel (details sidebar, explain panel, rule editor; 360 px) collapses the navigation to the 48 px icon rail (07 §2).
- Focus/distraction share = time in categories whose tendency is focus/distraction ÷ active time; tendency defaults to neutral (07 §3).
- Idle and other non-foreground states are drawn only on the category main track; app swimlanes show foreground facts only (07 §4.1).
- Rules page: one tab row — 分类, 规则, 未分类队列 (with count), 文档提取规则, 隐私规则; explain panel and rule editor use the right-hand panel slot (07 §6).
- Plot areas sit directly on `bg.base` (or Mica) in both light and dark, without card fill (07 §10). The light mockups still show a transparent plot area on the card; follow 07.
- Data-integrity warnings may use `status.critical` / `status.caution`; "no red warnings" applies to judging user behaviour (07 §6).

## Error screen (`errors.html`)

Unified startup, connection, and fatal error screen for the 960×640 window (also verified at the 640×480 minimum via `?size=min`). Query parameters: `?state=<key>&theme=light|dark&lang=zh-CN|en[&size=min][&details=open|closed]`. Uses only `tokens.css` variables and shared controls from `mockup.css`.

### Layout

- **Surface & chrome**: Same 32 px custom title bar (`#logo` + Windows caption buttons) directly on the Mica/`bg.base` canvas — no sidebar or `fill.layer` shell.
- **Scrollable viewport**: Below the title bar, the stage keeps a `24 px` (`space.xxl`) margin on all sides and centers short content via `margin: auto`. When an expanded card exceeds the window height (e.g., `migration-failed` at 640×480), the area below the title bar scrolls vertically (`overflow-y: auto`) with a subtle Fluent 2 overlay scroll thumb (`text.tertiary`, 11 §9.2) rather than clipping.
- **Progress state (`connecting`)**: Weightless centered 28 px Fluent progress ring and `text.secondary` label directly on `bg.base`, with no card or actions.
- **Failure states (all 8 errors — one identical structure)**: One centered `560 px` `fill.card` (`stroke.card`, `radius.overlay` 8 px) with no large tinted bands:
  1. **Header + body in the card**: 48×48 status icon tile (28 px Fluent icon) on the left; status badge, left-aligned `type.subtitle` (20/28 Semibold) title, `type.body` (`text.secondary`) explanation, and action bar (primary `accent` button first, standard secondary buttons next) aligned on the title column's vertical axis.
  2. **Collapsible details footer**: Subordinate `fill.card_secondary` band separated by `stroke.divider` in the same footer position for every failure. When expanded, it grows below inside the same card with a monospace diagnostic key-value list (always starting with `error_code`) and — for `migration-failed` — an inset `bg.base` command box with a dedicated copy button where long Windows paths wrap (`overflow-wrap: anywhere`) instead of truncating.

### Recoverable vs. unrecoverable visual rules

Both classes share the exact same card structure; status color is used **only** in the 48×48 icon tile and the 20 px badge (no tinted header bands or full-red areas):

- **Recoverable (`caution`)** — `core-not-running`, `core-not-running-starting`, `core-start-failed`, `connect-failed`:
  - 28 px Fluent warning triangle (`!`) in a `status.caution_bg` tile + `status.caution` badge (`需要操作` / `Action needed`).
  - Action bar leads with the in-window recovery action (`启动核心` / `重试`). In `core-not-running-starting`, the primary button shows an inline 16 px progress ring (`正在启动核心…`) and `重试` is disabled (`fill.control_disabled` / `text.disabled`).
- **Unrecoverable (`critical`)** — `migration-failed`, `newer-database`, `version-mismatch`, `other-session`:
  - 28 px Fluent dismiss circle (`×`) in a `status.critical_bg` tile + `status.critical` badge (`无法继续` / `Cannot continue`). Distinct glyph shapes (`!` triangle vs. `×` circle) keep the two classes distinguishable without relying on color alone.
  - Action bar offers only `复制详细信息` / `Copy details` (primary accent button, with copy icon) and `退出` / `Quit` (standard secondary button).
- **Consistent copy-details placement rule**: Show the compact `复制详细信息` / `Copy details` action in the right side of the details footer header **only when `复制详细信息` is not already in the primary action bar** (so recoverable states have it in the footer header, while unrecoverable states promote it to the primary button without duplicating it below).

### Design questions / choices made

1. **Card on Mica vs. unboxed text on Mica**: Wrapping the failure block in a single `fill.card` with `fill.card_secondary` for the collapsible footer keeps visual hierarchy on the title and actions while leaving the outer canvas on Mica/`bg.base`.
2. **Actions above details**: Placing the action bar directly under the explanation (above the collapsible `详细信息` footer) keeps primary actions at a fixed vertical position when details toggle and ensures they remain immediately visible above the fold even when tall expanded details scroll at 640×480.
3. **Unrecoverable primary button**: Styled `复制详细信息` as the primary accent button (following Fluent's "primary accent first" rule and steering users to grab diagnostics before quitting) and `退出` as a standard secondary button. Confirm whether both should be standard buttons instead, or if `退出` should be primary in states like `other-session`.
4. **Class badge labels**: Used `需要操作` / `Action needed` (recoverable) and `无法继续` / `Cannot continue` (unrecoverable) as calm, non-jargon labels.
5. **Default details expansion**: Expanded by default on `core-start-failed`, `migration-failed`, `newer-database`, and `version-mismatch` (where paths, commands, or versions are immediately useful) and collapsed on `core-not-running`, `core-not-running-starting`, `connect-failed`, and `other-session`.
6. **Restore command wrapping**: Long paths wrap cleanly across lines inside the command box rather than truncating with an ellipsis so the backup filename remains inspectable before copying.
7. **Error code naming**: Used `connect_timeout` (`connect-failed`), `database_too_new` (`newer-database`), and `version_mismatch` (`version-mismatch`) as placeholder diagnostic codes; align with the final `quartz-ipc` / `quartz-core` error codes during implementation.

## Today data states (`today-states.html`)

Covers the Today page states not shown in `today.html` (which depicts the loaded-with-data state). Reuses the `today.html` chrome unchanged (32 px custom title bar, 240 px navigation sidebar, date bar) at 1280×800 and verifies the 640×480 minimum window (`07 §2.3`) via `?size=min`. Query parameters: `?state=<key>&theme=light|dark&lang=zh-CN|en[&size=min][&reason=paused|locked|persistence-failed][&err=busy|internal]`. Uses only `tokens.css` variables and `mockup.css` controls (`.btn.disabled` was promoted to `mockup.css` as a shared control state).

### Layout per state

- **`loading`**: Keeps the date bar interactive and preserves the exact 4-card Today geometry (status line, timeline, hourly chart, ranking — no pending-away row) with static neutral skeleton bars while retaining known section headings, metric labels, and y-axis labels. Timeline controls and segmented toggles appear dimmed/disabled until data arrives.
- **`refreshing`**: Keeps all loaded cards, charts, and metrics at 100% crisp opacity with zero dimming, blanking, or reflow. Reserves the unused right end of the date bar for a quiet `14 px` progress ring and `正在更新…` / `Updating…` caption (`type.caption`, `text.tertiary`), shown only if a re-query takes noticeable time.
- **`empty-today`**: Date bar shows today with `→` and `今天` disabled. Below it, one spacious `fill.card` replaces the 4 empty data cards: a neutral 48×48 clock icon tile (`fill.subtle_hover` / `text.secondary`), `type.subtitle` heading, reassuring `text.secondary` copy, and a quiet recording pill (`status.success` 6 px dot + `正在记录` / `Recording`).
- **`empty-past`**: Date bar remains fully navigable (past date selected, `→` and `今天` enabled). Same calm single `fill.card` structure with a neutral calendar icon tile, factual explanation (core not running or records deleted), and one standard `回到今天` / `Go to today` button.
- **`stopped`**: Date bar shows today (`→` and `今天` disabled); all collected data remains visible below. A full-width Fluent 2 InfoBar sits directly **between the date bar and the status-line card**:
  - Neutral stop reasons (`paused` default, `locked`, etc.): `fill.card` + `stroke.card` with a `16 px` `text.secondary` icon, neutral copy matching `07 §1.2` / `quartz-i18n`, and a compact `恢复` / `Resume` button when paused.
  - Critical stop reason (`persistence-failed`, the sole critical stop state): `status.critical_bg` surface, subtle `status.critical` border, `16 px` `status.critical` circle-cross icon, and clear retry wording.
- **`query-failed`**: Full chrome, navigation sidebar, and date bar remain usable. Directly below the date bar sits one lightweight horizontal `fill.card` (`20 px` inline `status.caution` warning icon, `type.body_strong` heading, concise explanation, muted monospace error code `busy` / `internal`, and a single standard `重试` / `Retry` button) — visibly distinct from the full-window fatal/connection screen in `errors.html`.
- **`date-picker`**: Underlying Today page (past date `11月16日 周一` / `Mon, Nov 16`) stays visible behind an opaque `300 px` Fluent 2 `CalendarDatePicker` flyout (`bg.flyout`, `stroke.surface`, `radius.overlay`, `shadow.28`) anchored 4 px below the date button. Shows month header with prev/next buttons (next month disabled on current month), locale-aware weekday headers (`zh-CN` Monday-first, `en` Sunday-first), selected day (solid `accent.fill` circle), today (`1.5 px` `accent.fill` ring), and future days disabled (`text.disabled`).
- **`today-nav`**: Shows the live date bar with today selected (`→` and `今天` disabled via `.btn.disabled`), paired with a compact comparison card below stacking the today-selected bar against a past-day bar (`11月16日 周一`) above the status line and timeline.
- **Compact minimum window (`size=min`, 640×480)**: Follows `07 §2.1` and `07 §2.3` — sidebar collapses to the `48 px` icon rail (no collapse button), status metrics reflow into a 3×2 grid, hourly and ranking cards stack vertically, `query-failed` wraps its Retry button cleanly onto the text axis, and vertical overflow uses a `text.tertiary` Fluent overlay scroll thumb with zero horizontal scrolling.

### Design questions / choices made

1. **Static skeleton vs. shimmer / pulse (`loading`)**: Chose a **static skeleton** with no shimmer or looping pulse, matching `07 §9` ("没有持续动画") and `11 §1`/`§8` ("静止不重绘 / 数据刷新无动画"). Local SQLite IPC queries normally complete in tens of milliseconds; a static skeleton avoids flicker and preserves zero idle CPU. Confirm whether a slow opacity pulse is wanted if a query ever stalls past ~1 s.
2. **Refreshing indicator placement & debounce (`refreshing`)**: Chose **no indicator at all** for fast refreshes (< ~300 ms) so routine 5 s `day:<date>` pushes and quick timeline zooms never flicker; only when a re-query exceeds ~300 ms should the quiet `正在更新…` / `Updating…` cue appear in the empty right side of the date bar (causing zero layout shift and never dimming data). Confirm the ~300 ms delay threshold vs. omitting an indicator altogether.
3. **Single empty card vs. zeroed 4-card layout (`empty-today`, `empty-past`)**: Replaced the 4 empty data cards with one calm `fill.card` instead of rendering four boxes of `0 分` / `—` placeholders. Confirm whether the status-line card should still be kept above the empty message when `active_ms == 0`.
4. **Stop-reason InfoBar placement & severity (`stopped`)**: Placed the InfoBar **above** the status-line card (between the date bar and status metrics) so live capture state is clearly separated from historical day totals and never crowds the responsive 6-column stat grid. Only `persistence_failed` uses `status.critical` / `status.critical_bg`; `paused` and `locked` stay neutral. Confirm whether `session_unavailable` and `capture_gap` should use neutral InfoBar styling or `status.caution` / `status.caution_bg`.
5. **Inline `query-failed` visual weight vs. `errors.html`**: Kept `query-failed` strictly inline and lightweight (no 48×48 icon tile, no status badge, no collapsible details drawer, no Quit button, standard `Retry` button instead of an accent CTA) so users immediately read it as a transient per-day error while the sidebar and date bar remain usable.
6. **Activity dots on the calendar (`date-picker`) — recommendation**: **Recommend NOT adding activity dots in M3-D1.** Painting dots on calendar open requires an additional month-level range query (`query.range` is not built until M4 and is heavier than a presence check), adds IPC latency, and causes dots to pop in asynchronously after the flyout opens. Revisit in M4 only if a cheap cached `data_day_versions` day-presence bitmap is exposed.
7. **Calendar selected vs. today visual convention (`date-picker`)**: Used a solid `accent.fill` circle for the **selected** day (the day currently displayed on screen) and a `1.5 px` `accent.fill` ring for **today**. WinUI 3 `CalendarView` uses the inverse (solid Today, outlined Selected), whereas `gpui-component` `Calendar` highlights the selected day solidly. Confirm which convention to standardize on during GPUI implementation.
8. **Date bar disabled state (`today-nav`)**: Disabled both `→` and `今天` when viewing today (`fill.control_disabled`, `text.disabled`) rather than hiding `今天`, keeping the date bar geometry completely stable as the user steps across days.



### Decisions (today-states)

The user approved the mockup with these answers:

- Q1: static skeleton, no pulse.
- Q2: the refreshing indicator appears only after a re-query exceeds ~300 ms, at the right end of the date bar.
- Q3: an empty day replaces the four data cards with one empty-state card.
- Q4: only `persistence_failed` is critical; all other stop reasons (including `session_unavailable` and `capture_gap`) use the neutral InfoBar.
- Q6: no activity dots on the calendar in M3.
- Q7: selected day = solid accent circle, today = accent ring.
- Q8: `→` and `今天` are disabled (not hidden) on today.

## Rules page live states (M4 V1)

Live states for the classification rules page (`rules-states.html`), covering M4 H1 / H3 / H4 / H5. Implemented as a single parameterized page (mirroring `today-states.html` and `errors.html`) rather than mutating the original static `rules.html` / `rules-set.html` references, so the historical mockups stay intact while all 12 live states share one token-driven stylesheet, one `zh-CN` / `en` / `en-XA` copy dictionary, and responsive 1280×800 / 640×480 layout rules. All sample data uses desktop app/title/document fields (`app.exe_name`, `title.normalized`, `doc.document`, `doc.project`) with no M5 browser domains, and real `u32` revisions (`v1`–`v4`) and rule counts (`50`).

Query parameters: `?state=<key>&theme=light|dark&lang=zh-CN|en|en-XA[&size=min]`. Uses only `tokens.css` variables and `mockup.css` primitives.

### Layout per state

- **`loading`**: Keeps the 240 px navigation sidebar, page header (Import/Export disabled), and 5-tab pivot (`规则` active, queue badge as a static skeleton pill). Both the rule-set tree card and entries table card render static neutral skeletons (no shimmer/pulse, matching `today-states` Q1) preserving exact tree indentation and table column structure.
- **`empty`**: No user rule sets exist yet. The tree displays only the locked `builtin.classification · v2 · 50 条` root with an inset dashed callout below (`尚无自定义规则集` + primary `+ 新建规则集` CTA). The right card shows builtin rules read-only with an inline lock InfoBar and a `覆盖内置规则…` toolbar action.
- **`load-error`**: Full chrome, 240 px sidebar, and 5-tab pivot stay interactive. Below the pivot sits one lightweight inline `fill.card` (`20 px` `status.caution` warning icon, `type.body_strong` heading, concise explanation, monospace `error_code: busy`, and a standard `重试` / `Retry` button) — visibly lighter than the full-window fatal/connection screen in `errors.html`.
- **`new-set`**: Triggered from `builtin.classification/windows-terminal`. Over a `fill.smoke` scrim, a centered `468 px` Fluent 2 dialog (`bg.flyout`, `stroke.surface`, `radius.overlay`, `shadow.64`) states that `builtin.classification` is read-only and lets the user choose between an existing user child (`user.local.classification`, recommended) or creating a new user child set (locked parent `builtin.classification`, display name, `user.*` key with syntax hint, and immediate-enable toggle switch).
- **`reorder-entries`**: Demonstrates both allowed reorder interactions and their constraints. In the tree card, sibling user sets (`user.local.classification`, `user.project-docs`) show drag grips and an `accent.fill` insertion bar, while the `builtin.classification` root and non-sibling child (`user.exam-prep`) display dashed disabled-drop cues. In `本规则集条目`, an InfoBar explains that dragging only reorders same-priority entries within the set; priority `50` shows an active drop zone and insertion bar, while priority `80` shows a locked, disabled drop target.
- **`reorder-effective`**: Switches to `生效视图` (`Effective view`), removes drag handles, marks `优先级 ↓` as computed sort order, and places a lock InfoBar explaining that effective order is derived from priority, tree depth, and file order (with a shortcut button to `本规则集条目`). The selected row shows a non-draggable cue (`按优先级 80 · 深度 1 固定`).
- **`editor`**: Collapses navigation to the `48 px` icon rail (`07 §2.2`) so the 360 px classification rule editor sits side-by-side at 1280×800. Includes name, target category picker, priority (`0–1000`), match mode (`all` / `any`), same-priority rule order stepper (`#1 of 2` with `↑` / `↓`), condition rows (negation `非` / `NOT` toggle + desktop field + operator + value, with inline negation hint), `+ 添加条件`, and a pinned footer (`保存`, `取消`, `复制情况说明`). The live 7-day preview card displays **both** `条件命中` (`5 小时 04 分 · 29 段`) and `有效贡献` (`3 小时 42 分 · 21 段`, matching Today/rankings), a gap-breakdown line (shadowed / annotated / idle), and the first-50 privacy-safe desktop sample rows with outcome badges (`生效` / `被遮蔽` / `空闲`).
- **`editor-cat-picker`**: Same editor with the target category flyout (`bg.flyout`, `stroke.surface`, `shadow.28`) open, showing only active categories with swatch dots and tendency labels plus a footer note (`已归档分类（2）不提供选择`) confirming archived categories are excluded.
- **`editor-preview-stale`**: User is actively editing a condition value (accent bottom border + caret). Rather than blanking the preview, a `status.caution_bg` bar (`草稿已变更，停止输入后将重新计算预览`) and inline `14 px` progress ring appear above the softened (`opacity: 0.52`) previous preview results.
- **`saving`**: Primary `保存` button displays an inline `16 px` progress ring (`正在保存…`), an InfoBar reports `正在提交 user.local.classification v4…`, and editor inputs, Cancel, header Import/Export, and tree/list controls are disabled to prevent concurrent edits.
- **`stale-version`**: Conflict state after `rule_set_stale (current_version: 4)`. A `status.caution_bg` banner at the top of the editor explains that the rule set changed elsewhere, offers `重新加载并保留草稿` (accent) and `查看变更`, marks the panel header with an `未保存草稿` badge, disables `保存` until reloaded, and preserves every edited field untouched.
- **`validation-error`**: Inline rule validation (`rule_validation_failed`). A `status.critical_bg` summary banner lists the error count; invalid condition cards (`title.normalized` regex `E-RULE-REGEX` and empty `doc.document` value `E-RULE-VALUE`) highlight their inputs with `status.critical` borders and inline diagnostic codes; preview pauses with a clear message and `保存` stays disabled until fixed.
- **Responsive minimum window (`size=min`, 640×480)**: Follows `07 §2.1`–`§2.3`. Navigation collapses to the `48 px` icon rail (no collapse toggle). In main-only views (`reorder-effective`, `load-error`, etc.), the rule-set tree stacks vertically above the entries card with a Fluent overlay scrollbar (`text.tertiary`) and zero horizontal scrolling. When the editor opens (`editor`), it mounts as a **360 px right-aligned overlay** on opaque `bg.base` + `fill.layer` with `shadow.28`, pinned header and footer, scrollable form body, and a light-dismiss scrim over the underlying list.

### Design questions / choices made

1. **Compact (`640×480`) editor placement — 360 px overlay vs. full-pane list replacement**: Followed `07 §2.2` directly by mounting the rule editor as a **360 px right-aligned overlay** over the main area (preserving the `48 px` rail and dimmed underlying list context) while stacking the tree above the entries list below it so `07 §2.3` ("no horizontal scrollbar") holds even in `en-XA`. Unlike read-only details/explain panels that light-dismiss immediately on `Esc` or click-outside, confirm whether an overlay editor with unsaved changes should block accidental light-dismiss (prompting or requiring explicit Save/Cancel).
2. **Stale-version reload flow (`stale-version`)**: On `rule_set_stale`, `保存` disables and a top caution banner offers `重新加载并保留草稿` so the user refreshes the `v4` base version without losing their edited rule fields. Confirm this inline reload-and-keep-draft pattern vs. automatically opening a two-version diff modal.
3. **Dual-duration preview hierarchy (`editor`)**: Displayed `条件命中` and `有效贡献` side-by-side in equal `fill.card` tiles inside the preview card, keeping both values in neutral `text.primary` (distinguished by a `status.success` dot on `有效贡献` per `11 §3.2` so accent color stays reserved for interactive controls), followed by one gap-breakdown line and first-50 samples with per-row outcome badges (`生效` / `被遮蔽` / `空闲`). Confirm this hierarchy and whether the sample list should default to 3–5 visible rows with internal scrolling up to 50.
4. **Debounced stale-preview behavior (`editor-preview-stale`)**: Kept prior preview numbers dimmed (`opacity: 0.52`) beneath a caution notice while typing instead of replacing them with a skeleton. Confirm whether `保存` remains enabled for classification rules during a pending debounced preview (privacy rules still gate on a fresh preview receipt in V3).
5. **Priority-band headers during within-set reorder (`reorder-entries`)**: Grouped `本规则集条目` visually by priority band when reordering so users immediately see where a dragged row can and cannot drop, and added a locked InfoBar in `生效视图` (`reorder-effective`) with a jump button back to `本规则集条目`. Confirm whether priority-band headers in `本规则集条目` should appear only during an active drag or stay visible at all times.
6. **Override-builtin destination dialog (`new-set`)**: Always shows the destination dialog when overriding a builtin rule (pre-selecting `user.local.classification` if it exists, or creating a new user child set inline). Confirm whether subsequent overrides should default straight into `user.local.classification` with a secondary "Override into another set…" action, or always confirm the destination first.
7. **Archived categories footnote in picker (`editor-cat-picker`)**: Omitted archived categories from the target picker and displayed a muted footer note (`已归档分类（2）不提供选择`) explaining the omission. Confirm whether to keep this count hint or hide archived categories silently.
8. **Inline `load-error` parity with Today (`load-error`)**: Matched `today-states.html`'s `query-failed` card (caution icon, explanation, monospace error code, standard `重试` button, sidebar and pivot still usable) rather than introducing a modal or `errors.html` card. Confirm this shared inline error pattern across M4 pages.

### Decisions (user, 2026-11-23)

1. Overlay editor with unsaved changes: light dismiss and `Esc` ask "Discard changes?"; without changes the panel closes directly.
2. Stale version: keep the inline banner (`重新加载并保留草稿` / `查看变更`); no automatic diff dialog.
3. Preview hierarchy as drawn; the sample list shows 3–5 rows and scrolls internally up to 50.
4. Classification rules can be saved while a debounced preview is pending; privacy rules still require a fresh preview receipt.
5. Priority-band headers in `本规则集条目` appear only during an active drag.
6. Overriding a builtin rule writes to `user.local.classification` without a dialog; a secondary "Override into another rule set…" action opens the destination dialog (`new-set`).
7. Keep the archived-category count note in the category picker.
8. Rules-page load failures reuse the Today inline error card pattern.
