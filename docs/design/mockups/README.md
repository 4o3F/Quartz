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

