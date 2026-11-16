# 设计系统候选

日期：2026-11-16。目的：为界面（07）选定一个设计系统，约束颜色、字体、间距、圆角、层级、图标和图表配色，避免页面各自发挥。

## 1. 约束条件

### 1.1 技术现状（已查 gpui-kit 0.9.1 源码）

- gpui-component 的主题来自 shadcn/ui 的 neutral 配色（`theme/default-theme.json` 标注 `author: shadcn`）。主题是 JSON 文件，包含 136 个语义颜色令牌（`background`、`primary`、`sidebar`、`list_hover`、`chart.1`–`chart.5` 等），另有 `radius`、`radius_lg`、`font_size`、`mono_font_size`。换设计系统主要是换一套主题 JSON，再调整少量组件尺寸。
- 默认图标为 Lucide（ISC 许可，1,830 个）。替换图标集只需按 `IconName` 的文件名提供 SVG。
- gpui 在 Windows 上支持窗口背景 `Blurred`、`MicaBackdrop`、`MicaAltBackdrop`（`WindowBackgroundAppearance`），Mica 只在 Windows 11 生效。
- 托盘菜单是 Win32 菜单，样式由系统决定，不受设计系统影响。

### 1.2 Quartz 的要求

| # | 要求 | 原因 |
|---|---|---|
| R1 | 分类色至少 10 种，亮暗两种模式下可区分，并考虑色觉差异 | 时间轴主轨与堆叠柱按分类着色（07 第 3 节） |
| R2 | 信息密度高：时间轴、泳道、排行、规则列表 | 今天页一屏展示一天 |
| R3 | 亮暗主题跟随系统 | 07 第 3 节 |
| R4 | 中文排版：字号阶梯、行高适合 CJK；字体回退见 10 第 6.2 节 | 首版 zh-CN 与 en |
| R5 | 动效克制，静止时不重绘 | 08 第 1.3 节：静止 CPU ≤ 20 ms / 30 s |
| R6 | 与 Windows 托盘工具的使用场景协调 | 用户从托盘打开，看完即关 |
| R7 | 许可允许在开源项目中使用令牌与图标 | 开源发布 |
| R8 | 在 gpui-component 上的实现成本低 | 界面由 Codex 实现，M3 有工期 |

## 2. 候选

### A. Fluent 2（Windows 11 原生风格）

- **外观**：Windows 11 设置、Outlook、Teams 的风格。浅灰底、白色卡片、圆角 4/8 px、柔和阴影；窗口用 Mica 透出桌面色调；强调色取 Windows 系统强调色。
- **令牌**：Fluent 2 公开颜色、字体、间距（4 px 网格）、圆角、阴影令牌。字体 Segoe UI Variable（Windows 11 自带，Windows 10 回退 Segoe UI），中文接 Microsoft YaHei UI。
- **图标**：Fluent UI System Icons（MIT），有 Regular / Filled 两种线重。
- **分类色**：Fluent 的共享色（shared colors）有 20 多种色相，但没有作为图表配色做过色觉验证；需要我们挑选并验证 10 种。
- **适合度**：R6 最好，打开后与系统设置、托盘弹出层观感一致。R2 中等，Fluent 默认控件高度 32 px，偏宽松，需要为时间轴与列表定义紧凑密度。
- **成本**：中。主题 JSON 改写；`radius = 4`、`radius_lg = 8`；控件高度与焦点样式需要调整；图标需按 `IconName` 映射约 60 个常用图标；读取系统强调色（WinRT `UISettings`）。
- **风险**：只换颜色不改控件形态时，会显得“像 Fluent 又不像”。需要逐个检查按钮、开关、输入框、标签页的形态。

### B. shadcn/ui + Radix Colors（gpui-component 默认风格）

- **外观**：中性灰阶、细边框、几乎没有阴影，Linear、Vercel 一类工具的风格。
- **令牌**：直接使用 gpui-component 内置主题。分类色使用 Radix Colors（MIT）：每种色相 12 级，按级别规定用途（背景、边框、实色、文字），亮暗两套成对设计。
- **图标**：Lucide（已内置）。
- **分类色**：从 Radix 的 tomato、amber、grass、teal、cyan、blue、indigo、violet、plum、pink、brown、gray 中取第 9 级作为实色，第 3 级作为浅底。Radix 不做色觉验证，相邻色相（blue / indigo）需要拉开。
- **适合度**：R2、R5、R8 最好。R6 一般：外观通用，看不出是 Windows 应用，但也不突兀。
- **成本**：最低。主要工作是定义分类色板和时间轴专用令牌。
- **风险**：风格辨识度低，容易显得像模板。

### C. IBM Carbon（数据密集型）

- **外观**：直角或小圆角、清晰的网格、强对比，适合仪表盘与数据表。
- **令牌**：Carbon 公开颜色、字体（IBM Plex，中文用系统字体回退）、间距、布局网格令牌，Apache-2.0 许可。
- **图标**：Carbon Icons（Apache-2.0）。
- **分类色**：Carbon 数据可视化有固定顺序的 14 色分类色板，专门检查过相邻色对比，并为暗色主题提供对应色板。这是四个候选中最完整的图表配色。
- **适合度**：R1、R2 最好。R6 较弱：企业软件观感，对个人工具偏冷硬。
- **成本**：中。主题 JSON 改写，`radius` 设为 0–2；字号阶梯换 Carbon 的 productive 类型；图标映射。
- **风险**：整体偏严肃；直角风格与 Windows 11 圆角窗口并排时不协调。

### D. GitHub Primer（功能型、可访问性强）

- **外观**：GitHub 的风格。中性偏冷，信息密度中高，圆角 6 px。
- **令牌**：Primer Primitives（MIT）提供颜色、尺寸、字体令牌；有色觉友好主题（protanopia/deuteranopia、tritanopia）与高对比主题。
- **图标**：Octicons（MIT），约 300 个，数量少于 Lucide。
- **分类色**：Primer 的 data visualization 颜色有 13 种色相，并随色觉主题变化。
- **适合度**：R1、R2、R7 好；可以把色觉友好主题直接作为设置项提供。R6 一般。
- **成本**：中。主题 JSON 改写；Octicons 缺少的图标用 Lucide 补，会造成图标风格混用。
- **风险**：用户可能觉得“像 GitHub”。

## 3. 对比

| | A Fluent 2 | B shadcn + Radix | C Carbon | D Primer |
|---|---|---|---|---|
| R1 分类色 | 需自选并验证 | 需自选，部分验证 | 最好，现成 14 色 | 好，带色觉主题 |
| R2 信息密度 | 中，需定义紧凑密度 | 好 | 最好 | 好 |
| R3 亮暗 | 有 | 有 | 有 | 有，另有高对比 |
| R4 中文排版 | 好，系统字体 | 好 | 中，Plex 无简体中文 | 好 |
| R5 动效 | 规范含动效，需裁剪 | 动效少 | 动效少 | 动效少 |
| R6 Windows 协调 | 最好，Mica 与系统强调色 | 一般 | 弱 | 一般 |
| R7 许可 | MIT | MIT / ISC | Apache-2.0 | MIT |
| R8 实现成本 | 中 | 最低 | 中 | 中 |

> **结论（2026-11-16）**：用户选定 A Fluent 2，分类色借用 Carbon 色板。规范见 [11-design-system.md](../design/11-design-system.md)。

## 4. 建议

推荐 **A Fluent 2，分类色借用 C Carbon 的分类色板**：

- Quartz 是从托盘打开的 Windows 工具，Fluent 2 与系统观感一致；gpui 已支持 Mica。
- Fluent 2 缺少验证过的图表配色，这正好由 Carbon 的 14 色分类色板补上（Apache-2.0，可以只借用这一部分）。
- 时间轴与列表另外定义紧凑密度（控件高 28 px、行高 24 px），其余使用 Fluent 标准密度。

如果更看重工期和实现风险，选 **B**：几乎没有改造成本，分类色同样可以借用 Carbon 色板。

无论选哪个，都在 07 中补一节“设计令牌”，包括：颜色语义（含分类色 1–14 的亮暗值）、字号阶梯、间距、圆角、阴影、密度、图标集、动效时长上限。主题 JSON 是唯一来源，组件里不写死颜色值。

## 5. 下一步

选定一到两个候选后，用 ui-spike 的 GPUI 工程把今天页的静态样稿（日期栏、状态行、时间轴、堆叠柱、排行）按候选主题各渲染一版，给出 zh-CN 与 en 的截图对比，再定稿令牌。
