# 11 设计系统

Quartz 界面采用 **Fluent 2**，以 Windows 11 的 WinUI 3 实现为准（用户决策 10）。分类色使用 **Carbon 数据可视化的 14 色分类色板**，弥补 Fluent 缺少经过验证的图表配色。

本章是界面视觉的唯一规范。实现时所有值来自主题 JSON（第 9 节），组件中不写死颜色、字号与尺寸。

来源（2026-11-16 读取）：

- 颜色：microsoft-ui-xaml `controls/dev/CommonStyles/Common_themeresources_any.xaml`（MIT）。
- 字号阶梯、圆角、层级、动效、Mica：Windows 开发文档 Typography、Geometry、Layering and elevation、Motion、Mica。
- 间距、阴影、时长与缓动：`@fluentui/tokens`（MIT）。
- 分类色：carbon-charts `packages/core/scss/_color-palette.scss` 与 `@carbon/colors` 令牌（Apache-2.0）。

## 1. 原则

1. **与 Windows 一致。** 从托盘打开时，外观与 Windows 设置、托盘弹出层一致：Mica 底、系统强调色、Segoe UI Variable、4/8 px 圆角。
2. **数据优先。** 颜色主要留给分类。界面框架只用中性色与强调色，状态色只用于状态。
3. **静止不重绘。** 动效只用于响应操作，没有循环动画（08 第 1.3 节）。
4. **可读与可及。** 文字对比度满足 WCAG AA，图形元素与背景对比度不低于 3:1，支持高对比度模式与“减少动画”。

## 2. 材质与层级

| 层 | 材质 | 令牌 |
|---|---|---|
| 窗口底层 | Mica（`WindowBackgroundAppearance::MicaBackdrop`） | 不可用时用 `bg.base` |
| 内容层 | 半透明填充，透出 Mica | `fill.layer` |
| 卡片 | 半透明填充 + 1 px 边框 | `fill.card`、`stroke.card` |
| 弹出层（菜单、下拉、tooltip、对话框） | 不透明 | `bg.flyout`、`stroke.surface`，阴影见第 6 节 |

- 侧边栏直接放在 Mica 上，不加填充；主区域使用内容层；状态行、排行等分组使用卡片。
- WinUI 的弹出层使用 Acrylic。GPUI 不支持窗口内 Acrylic，弹出层改用不透明的 `bg.flyout`。
- Mica 在以下情况由系统回退为纯色，界面要同样可用：Windows 10、关闭透明效果、节电模式、窗口失去焦点。回退后窗口底色为 `bg.base`。
- 高对比度模式（`SystemParametersInfo(SPI_GETHIGHCONTRAST)`）下不使用 Mica 与半透明令牌，全部改用系统颜色（`GetSysColor`：窗口、窗口文字、高亮、高亮文字、灰色文字、按钮面）。分类色在高对比度下以文字标签和图案区分，不依赖颜色。数据标记使用不透明填充和绘制在区间内部的 1 px 系统窗口文字色描边；分类按色板序号循环分配实心高亮、密集斜线、反斜线、交叉线、横线、竖线和点阵，图例采用相同图案。

实现通过 GPUI 的 `set_background_appearance` 设置 backdrop；Quartz 补充自绘客户区的 `DwmExtendFrameIntoClientArea(-1)` 和亮暗属性。Windows build < 22621 或 DWM 调用失败时使用 `bg.base`。Mica 呈现的实测记录见 [测量记录](../measurements.md)。

## 3. 颜色

### 3.1 中性与状态

WinUI 资源写作 `#AARRGGBB`。下表已换成主题 JSON 使用的 `#RRGGBBAA`（不透明时省略 alpha）。

| Quartz 令牌 | WinUI 来源 | 亮色 | 暗色 | 用途 |
|---|---|---|---|---|
| `text.primary` | `TextFillColorPrimary` | `#000000E4` | `#FFFFFF` | 正文、标题 |
| `text.secondary` | `TextFillColorSecondary` | `#0000009E` | `#FFFFFFC5` | 次要文字、说明 |
| `text.tertiary` | `TextFillColorTertiary` | `#00000072` | `#FFFFFF87` | 占位符、坐标轴标签 |
| `text.disabled` | `TextFillColorDisabled` | `#0000005C` | `#FFFFFF5D` | 禁用文字 |
| `text.on_accent` | `TextOnAccentFillColorPrimary` | `#FFFFFF` | `#000000` | 强调色按钮上的文字 |
| `fill.control` | `ControlFillColorDefault` | `#FFFFFFB3` | `#FFFFFF0F` | 按钮、输入框默认底色 |
| `fill.control_hover` | `ControlFillColorSecondary` | `#F9F9F980` | `#FFFFFF15` | 控件悬停 |
| `fill.control_pressed` | `ControlFillColorTertiary` | `#F9F9F94D` | `#FFFFFF08` | 控件按下 |
| `fill.control_disabled` | `ControlFillColorDisabled` | `#F9F9F94D` | `#FFFFFF0B` | 控件禁用 |
| `fill.subtle_hover` | `SubtleFillColorSecondary` | `#00000009` | `#FFFFFF0F` | 列表行、无边框按钮悬停 |
| `fill.subtle_pressed` | `SubtleFillColorTertiary` | `#00000006` | `#FFFFFF0A` | 列表行、无边框按钮按下 |
| `fill.layer` | `LayerFillColorDefault` | `#FFFFFF80` | `#3A3A3A4C` | Mica 之上的内容层 |
| `fill.card` | `CardBackgroundFillColorDefault` | `#FFFFFFB3` | `#FFFFFF0D` | 卡片 |
| `fill.card_secondary` | `CardBackgroundFillColorSecondary` | `#F6F6F680` | `#FFFFFF08` | 卡片内的次级区域 |
| `fill.smoke` | `SmokeFillColorDefault` | `#0000004D` | `#0000004D` | 对话框遮罩 |
| `bg.base` | `SolidBackgroundFillColorBase` | `#F3F3F3` | `#202020` | Mica 不可用时的窗口底色；暗色图表绘图区 |
| `bg.secondary` | `SolidBackgroundFillColorSecondary` | `#EEEEEE` | `#1C1C1C` | — |
| `bg.tertiary` | `SolidBackgroundFillColorTertiary` | `#F9F9F9` | `#282828` | — |
| `bg.flyout` | `SolidBackgroundFillColorQuarternary` | `#FFFFFF` | `#2C2C2C` | 弹出菜单、下拉、tooltip 底色 |
| `stroke.control` | `ControlStrokeColorDefault` | `#0000000F` | `#FFFFFF12` | 控件边框 |
| `stroke.control_strong` | `ControlStrokeColorSecondary` | `#00000029` | `#FFFFFF18` | 控件底边强调、输入框 |
| `stroke.card` | `CardStrokeColorDefault` | `#0000000F` | `#00000019` | 卡片边框 |
| `stroke.surface` | `SurfaceStrokeColorDefault` | `#75757566` | `#75757566` | 弹出层边框 |
| `stroke.divider` | `DividerStrokeColorDefault` | `#0000000F` | `#FFFFFF15` | 分隔线 |
| `stroke.focus_outer` | `FocusStrokeColorOuter` | `#000000E4` | `#FFFFFF` | 焦点框外圈 2 px |
| `stroke.focus_inner` | `FocusStrokeColorInner` | `#FFFFFFB3` | `#000000B3` | 焦点框内圈 1 px |
| `status.success` | `SystemFillColorSuccess` | `#0F7B0F` | `#6CCB5F` | 成功 |
| `status.caution` | `SystemFillColorCaution` | `#9D5D00` | `#FCE100` | 警告（如写入失败恢复中） |
| `status.critical` | `SystemFillColorCritical` | `#C42B1C` | `#FF99A4` | 错误 |
| `status.neutral` | `SystemFillColorNeutral` | `#00000072` | `#FFFFFF8B` | 中性状态；“未分类”色 |
| `status.success_bg` | `SystemFillColorSuccessBackground` | `#DFF6DD` | `#393D1B` | 成功提示条底色 |
| `status.caution_bg` | `SystemFillColorCautionBackground` | `#FFF4CE` | `#433519` | 警告提示条底色 |
| `status.critical_bg` | `SystemFillColorCriticalBackground` | `#FDE7E9` | `#442726` | 错误提示条底色 |

### 3.2 强调色

强调色取 Windows 系统强调色，界面进程启动时通过 WinRT `UISettings.GetColorValue` 读取 `Accent` 及其 `AccentLight1–3`、`AccentDark1–3` 变体，并监听 `ColorValuesChanged`。

| 令牌 | 亮色 | 暗色 | 用途 |
|---|---|---|---|
| `accent.fill` | AccentDark1 | AccentLight2 | 主按钮、选中的开关、进度条、侧边栏选中指示条 |
| `accent.fill_hover` | `accent.fill` 90% 不透明度 | 同左 | 主按钮悬停 |
| `accent.text` | AccentDark2 | AccentLight3 | 链接、强调文字 |

强调色只用于交互元素与选中状态，不用于数据。系统强调色可能与某个分类色接近，二者用途分开，不会混淆含义。

### 3.3 分类色

分类色使用 Carbon 的 14 色分类色板。色板顺序经过设计，按顺序分配时相邻颜色对比最大；新建分类按顺序取第一个未使用的编号。`categories.color` 保存色板键 `cat.1`–`cat.14` 或 `neutral`（03 第 2.6 节）。

| 键 | 亮色 | 暗色 | 亮色对 `#F3F3F3` | 暗色对 `#202020` | 色块上文字 |
|---|---|---|---|---|---|
| `cat.1` | `#6929C4` | `#8A3FFC` | 7.0 | 3.3 | 白 |
| `cat.2` | `#1192E8` | `#33B1FF` | 3.0 | 6.9 | 黑 |
| `cat.3` | `#005D5D` | `#007D79` | 6.9 | 3.3 | 白 |
| `cat.4` | `#9F1853` | `#FF7EB6` | 6.9 | 6.9 | 白 / 黑 |
| `cat.5` | `#FA4D56` | `#FA4D56` | 3.0 | 4.9 | 黑 |
| `cat.6` | `#520408` | `#FFF1F1` | 13.7 | 14.8 | 白 / 黑 |
| `cat.7` | `#198038` | `#6FDC8C` | 4.5 | 9.5 | 白 / 黑 |
| `cat.8` | `#002D9C` | `#4589FF` | 10.2 | 4.9 | 白 / 黑 |
| `cat.9` | `#EE5296` | `#D02670` | 3.0 | 3.3 | 黑 / 白 |
| `cat.10` | `#B28600` | `#D2A106` | 3.0 | 6.9 | 黑 |
| `cat.11` | `#009D9A` | `#08BDBA` | 3.0 | 7.0 | 黑 |
| `cat.12` | `#012749` | `#BAE6FF` | 13.6 | 12.3 | 白 / 黑 |
| `cat.13` | `#8A3800` | `#BA4E00` | 7.2 | 3.2 | 白 |
| `cat.14` | `#A56EFF` | `#D4BBFF` | 3.0 | 9.6 | 黑 |
| `neutral` | `status.neutral` | `status.neutral` | 3.3 | 5.8 | 按合成色选黑或白 |

- 亮色 `cat.9` 使用 `#EE5296`，相对 Carbon 原值将绿色通道降低一个 sRGB 步长，对 `bg.base` 为 3.0123:1。
- 对比度为 WCAG 相对亮度对比，图形元素要求不低于 3:1。“色块上文字”取白色或黑色中对比较高的一个，均不低于 4.5:1。
- 暗色下 `cat.1`、`cat.3`、`cat.9`、`cat.13` 对卡片底色（约 `#2C2C2C`）只有 2.8:1。因此**图表绘图区在暗色下不叠加卡片填充**，直接放在 `bg.base` 或 Mica 上；相邻色块之间留 1 px 背景色间隙。
- 内置分类的默认颜色：开发 `cat.8`、学习 `cat.1`、沟通 `cat.3`、娱乐 `cat.13`（暖橙，符合“分心用暖色、不用红色”）、系统与工具 `cat.10`、未分类 `neutral`。
- 分类色只表示分类，不表示专注或分心。倾向在状态行用文字表达。
- gpui-component 的 `chart.1`–`chart.5` 映射为 `cat.1`–`cat.5`。

### 3.4 时间轴状态

| 状态 | 表现 |
|---|---|
| 活跃 | 分类色实心块 |
| 空闲（idle） | 分类色 40% 不透明度，叠加 `stroke.control_strong` 斜纹（45°，间距 4 px） |
| 锁屏、睡眠、断开、暂停、核心未运行 | `fill.subtle_hover` 实心，无文字 |
| 无前台、已排除、采集缺口 | `status.neutral` 30% 不透明度，宽度足够时显示文字（如“已排除 25 分钟”） |
| 时钟异常 | 顶部 2 px `status.caution` 标记 |
| 悬停 | 1 px `text.primary` 描边 |
| 选中 / 拖选范围 | `accent.fill` 20% 不透明度覆盖，边缘 1 px `accent.fill` |

## 4. 字体

### 4.1 字族

| 用途 | 字族 |
|---|---|
| 小字（≤ 12 px） | `Segoe UI Variable Small` |
| 正文（14–18 px） | `Segoe UI Variable Text` |
| 标题（≥ 20 px） | `Segoe UI Variable Display` |
| Windows 10 回退 | `Segoe UI` |
| 中文回退 | 按界面语言，见 10 第 6.2 节（zh-CN 为 `Microsoft YaHei UI`） |
| 等宽（规则编辑器正则、路径） | `Cascadia Mono`，回退 `Consolas` |

时长、时间、百分比等会实时变化的数字启用等宽数字（OpenType `tnum`），避免数字跳动时宽度变化。M3 验证 GPUI 能否通过 DirectWrite 选中可变字体的光学尺寸实例与 `tnum` 特性；不能时统一使用 `Segoe UI Variable`。

### 4.2 字号阶梯

与 Windows 字号阶梯一致（单位为有效像素，字号/行高）。不使用 Bold 和斜体，强调用 Semibold。

| 令牌 | 字号/行高 | 字重 | Quartz 用途 |
|---|---|---|---|
| `type.caption` | 12/16 | Regular | 坐标轴标签、时间轴刻度、辅助说明 |
| `type.body` | 14/20 | Regular | 正文、列表、表单 |
| `type.body_strong` | 14/20 | Semibold | 列表中的主名称、卡片标题 |
| `type.body_large` | 18/24 | Regular | 状态行数值的单位 |
| `type.subtitle` | 20/28 | Semibold | 页面区块标题（时间轴、排行） |
| `type.title` | 28/36 | Semibold | 页面标题、状态行主数值（活跃时长） |
| `type.title_large` | 40/52 | Semibold | 不使用 |
| `type.display` | 68/92 | Semibold | 不使用 |

中文按同一阶梯排版。12 px 是中文可读的下限（Windows 排版指南：最小 12 px Regular、14 px Semibold），不使用更小的字号。

## 5. 形状与间距

### 5.1 圆角

| 令牌 | 值 | 用于 |
|---|---|---|
| `radius.control` | 4 px | 按钮、输入框、复选框、列表行背景、tooltip、进度条、滚动条、时间轴色块 |
| `radius.overlay` | 8 px | 窗口、卡片、弹出菜单、下拉、对话框 |
| `radius.none` | 0 | 与其他直边相接的边；窗口最大化或贴靠时 |

对应 gpui-component 的 `radius = 4`、`radius_lg = 8`。

### 5.2 间距

4 px 网格，取 `@fluentui/tokens` 的间距阶梯：

| 令牌 | 值 | 典型用途 |
|---|---|---|
| `space.xxs` | 2 | 图标与紧贴文字 |
| `space.xs` | 4 | 紧凑密度的行内间距 |
| `space.s` | 8 | 控件之间、卡片之间 |
| `space.m` | 12 | 卡片内元素之间 |
| `space.l` | 16 | 卡片内边距 |
| `space.xl` | 20 | — |
| `space.xxl` | 24 | 页面左右边距、区块之间 |
| `space.xxxl` | 32 | 页面顶部到第一个区块 |

### 5.3 密度

| 密度 | 控件高度 | 列表行高 | 用于 |
|---|---|---|---|
| 标准 | 32 px | 40 px | 设置页、对话框、规则编辑器表单 |
| 紧凑 | 24 px | 32 px | 时间轴工具条、排行、规则列表、未分类队列 |

时间轴尺寸：分类主轨 24 px；应用泳道 20 px，泳道间距 4 px；时间刻度区 20 px；色块圆角 2 px（色块很窄时圆角会吃掉面积，此处例外）。

### 5.4 窗口布局

- 侧边栏宽 240 px，可折叠为 48 px 的图标栏；放在 Mica 上，选中项左侧显示 3 px × 16 px 的 `accent.fill` 圆角指示条。
- 主区域左右边距 `space.xxl`，最大内容宽度 1200 px，超出时居中。
- 详情侧栏宽 360 px，使用内容层，左侧 1 px `stroke.divider`。并排显示，或在窗口较窄时作为浮层（第 5.5 节），浮层另加 `shadow.28`。
- 标题栏与 Mica 连成一体（自绘标题栏，高 32 px），系统按钮保持 Windows 样式。

### 5.5 断点 / 窗口尺寸档位

断点取 WinUI `NavigationView` 的标准值，单位为逻辑像素（DIP），与系统缩放和文字大小无关。档位由窗口宽度在每次渲染时推导，没有媒体查询。

| 档位 | 窗口宽度 | 侧边栏 | 详情侧栏 | 小时柱与排行 |
|---|---|---|---|---|
| Compact | < 641 | 48 px 图标栏 | 360 px 浮层 | 堆叠 |
| Medium | 641–1007 | 48 px 图标栏 | 360 px 浮层 | 堆叠 |
| Expanded | ≥ 1008 | 240 px，可折叠 | 并排，主区域不足时为浮层 | 并排 |

- 边界值：640 为 Compact，641 为 Medium，1007 为 Medium，1008 为 Expanded。
- 最小窗口 640×480。
- `ShellSizes::timeline_min_main`：并排详情侧栏后主区域应保留的最小宽度，等于页面左右边距（2 × `space.xxl`）、卡片内边距（2 × `space.l`）、三条 1 px 边框（主区域左边框与卡片两侧）、标签列最大宽度 `LABEL_COLUMN_MAX_WIDTH`（160）、标签列与绘图区之间的 `space.s` 间距与绘图区最小可用宽度 `TIMELINE_MIN_PLOT_WIDTH`（480 px，约可放 6 个有标签的刻度）之和，默认主题下为 731 px。并排后主区域低于它时，先把侧边栏收起为图标栏，仍不够则改为浮层。
- 浮层贴窗口右边缘、位于标题栏之下，内容层加 `shadow.28`，面板之外是透明的轻触关闭遮罩（07 第 2.2 节）。
- 实现：`quartz-ui` 的 `layout` 模块（不含 GPUI，有单元测试）。断点与最小宽度是平台布局常量，不进入主题文件。

### 5.6 内容驱动的宽度

文字不放进写死宽度的列（10 第 6.3 节）。按下表区分哪些尺寸写死、哪些由内容决定：

| 类别 | 规则 | 例子 |
|---|---|---|
| 固定令牌（逻辑像素） | 与文字无关的几何，保持常数 | 间距、圆角、描边与分隔线、图标尺寸、侧边栏 240/48、面板 360、轨道与泳道高度的下限、比例条高度、色块最小宽度（只对块体取整，过窄的区间画成标记，07 第 4.3 节） |
| 名称列 | `flex: 1`，`min-width: 0`，超出时省略号 | 排行的应用名、常见标题、泳道标签中的应用名 |
| 数值与时长列 | 宽度取该列所有格式化值中最宽者，用 GPUI 文本系统按实际字体、字号和等宽数字（`tnum`）测量，再限制在 [最小, 最大] 之间；格子内文字不换行，超出最大宽度时省略号 | 排行时长 `VALUE_RANGE` 32–96；常见标题时长 `TITLE_VALUE_RANGE` 32–96；小时柱纵轴标签 `AXIS_COLUMN_RANGE` 24–72；时间轴标签列 `LABEL_COLUMN_RANGE` 72–160（最大值即 `LABEL_COLUMN_MAX_WIDTH`） |
| 弹出内容 | 只设最小与最大宽度，随内容增长，超出最大宽度时换行或省略号 | tooltip 200–320；日期选择按钮 `DATE_PICKER_MIN_WIDTH` 120 至 `DATE_PICKER_MAX_WIDTH` 240 |
| 等宽列 | 状态行单元格：列数取 6、3、2、1 中最大的使最宽单元格放得下，宽度取自布局边界；单元格内容单行、不换行、省略号，最宽单元格比卡片还宽时取 1 列，行高不变 | 状态行 |
| 按绘图区分配 | 不设单位宽度，由可用宽度均分 | 小时柱、7 天柱 |
| 比例条 | 与名称平分行内剩余宽度，不小于 `PROPORTION_BAR_MIN_WIDTH`（48 px） | 排行与常见标题的比例条 |

- 最大宽度也是截断点：超过时文字以省略号收尾。
- 控件与行的高度只给下限（`min-height`），随文字缩放增长（第 5.7 节）。
- 测量值与缩放因子相关，缩放因子变化时重新测量；测量只在渲染时进行。
- 时间轴绘图区与小时标签行在布局给出边界的同一帧内（prepaint）由 `canvas` 构建元素，几何取自当前边界，没有“上一帧宽度”，窗口缩放、侧边栏切换与文字大小变化后都不滞后；它们只随父元素重绘，不自行请求帧。

### 5.7 文字缩放

读取 Windows“文字大小”（`UISettings.TextScaleFactor`，100%–225%），用 `TextScale` 表示，取值限定在 1.0–2.25，超出范围为错误，不截断。

- 字号阶梯（第 4.2 节）的字号与行高都乘以缩放因子；字重、字族不变。
- 依赖文字的尺寸增长：行高、轨道与坐标轴高度（取令牌值与缩放后行高中较大者）、文字列宽（重新测量）。控件与列表行的高度保持第 5.3 节的值为下限，文字变大时随之增高（与 WinUI 一致）。
- 不随文字缩放：间距、圆角、描边、图标尺寸、侧边栏与面板宽度、窗口断点与最小窗口尺寸。
- 运行中变化：窗口激活时重新读取，因缩放因子变化重新布局，不重启。预览用 `QUARTZ_PREVIEW_TEXT_SCALE` 覆盖。

## 6. 阴影与层级

阴影取 `@fluentui/tokens`。亮色 ambient `rgba(0,0,0,0.12)`、key `rgba(0,0,0,0.14)`；暗色 ambient `rgba(0,0,0,0.24)`、key `rgba(0,0,0,0.28)`。

| 令牌 | 值 | Windows 层级 | 用于 |
|---|---|---|---|
| `shadow.2` | `0 0 2px ambient, 0 1px 2px key` | 控件（2） | 主按钮 |
| `shadow.8` | `0 0 2px ambient, 0 4px 8px key` | 卡片（8） | 悬停中的卡片（静止卡片不加阴影，只用边框） |
| `shadow.16` | `0 0 2px ambient, 0 8px 16px key` | tooltip（16） | tooltip、时间轴悬停卡片 |
| `shadow.28` | `0 0 8px ambient, 0 14px 28px key` | 弹出层（32） | 菜单、下拉、日期选择 |
| `shadow.64` | `0 0 8px ambient, 0 32px 64px key` | 对话框（128） | 对话框 |

阴影只用于浮在内容之上的元素。卡片和列表靠边框与填充区分层次，不靠阴影。

## 7. 图标

- 使用 Fluent UI System Icons（MIT）。正常状态用 Regular，选中状态（侧边栏当前页、开启的开关型按钮）用 Filled。
- 尺寸：标准密度 20 px，紧凑密度 16 px。颜色跟随文字令牌，不单独着色；状态图标使用对应状态色。
- gpui-component 按 `IconName` 的文件名加载 SVG。Quartz 关闭组件自带的 Lucide 图标集，为用到的 `IconName` 提供 Fluent 图标文件；只提供实际用到的图标，构建时检查没有缺失，避免混用两套图标。
- 应用图标来自 exe 或 AUMID（`apps.icon`，04），按原样显示在 16 或 20 px 的方框内。
- 托盘图标是单独的一组 16/20/24/32 px ICO，按系统亮暗切换（07 第 1.1 节）。

## 8. 动效

取 Windows 动效规范中的时长与缓动，并按原则 3 收窄：

| 用途 | 时长 | 缓动 |
|---|---|---|
| 悬停、按下的颜色变化 | 83 ms | 线性 |
| 弹出层、tooltip 出现 | 167 ms | `cubic-bezier(0,0,0,1)`，位移 + 淡入 |
| 弹出层消失 | 167 ms | `cubic-bezier(1,0,1,1)`，淡出 |
| 侧栏展开、详情侧栏滑入 | 250 ms | `cubic-bezier(0,0,0,1)` |
| 页面切换 | 无动画 | — |
| 数据刷新、时间轴缩放平移 | 无动画，直接重绘 | — |

- 最长动画 250 ms。动画结束后不再请求帧。
- 系统“动画效果”关闭时（`SPI_GETCLIENTAREAANIMATION` 为假），所有动画改为瞬时完成。

## 9. 实现

### 9.1 主题文件

- `crates/quartz-ui/themes/fluent.json` 是唯一的令牌来源，格式沿用 gpui-component 主题 JSON，包含 `Fluent Light` 与 `Fluent Dark` 两个主题。第 3–6 节的 Quartz 令牌作为扩展字段写在同一文件，由 `quartz-ui` 读取。
- 主题文件编译进 exe（`include_str!`）。启动时只解析一次。
- 亮暗跟随系统（`Theme::sync_system_appearance`），强调色与高对比度在运行时变化时重新应用，不重启。

### 9.2 gpui-component 令牌映射

| gpui-component | Fluent 令牌 |
|---|---|
| `background` | 透明（Mica）；回退 `bg.base` |
| `foreground` | `text.primary` |
| `muted.foreground` | `text.secondary` |
| `border` | `stroke.card` |
| `input.border` | `stroke.control` |
| `primary` / `primary.hover` / `primary.foreground` | `accent.fill` / `accent.fill_hover` / `text.on_accent` |
| `secondary` / `secondary.hover` / `secondary.active` | `fill.control` / `fill.control_hover` / `fill.control_pressed` |
| `list.hover` / `list.active` | `fill.subtle_hover` / `fill.subtle_pressed` |
| `popover` | `bg.flyout` |
| `sidebar` | 透明（Mica）；回退 `bg.base` |
| `ring` | `stroke.focus_outer` |
| `danger` / `success` / `warning` | `status.critical` / `status.success` / `status.caution` |
| `chart.1`–`chart.5` | `cat.1`–`cat.5` |
| `radius` / `radius_lg` / `font_size` | 4 / 8 / 14 |

组件 JSON 使用 gpui-kit 0.10.0 的字段名，例如 `primary.background`、`primary.hover.background`、`popover.background` 和 `sidebar.background`。`quartz.component_mapping` 在加载时解析为组件字段枚举和令牌枚举，运行时通过类型化访问器应用已解析的颜色；两个主题的 `colors` 显式保存这些字段的回退值。`window.background` 是派生令牌：Mica 时透明，纯色回退时取 `bg.base`。

| gpui-component 补充字段 | Fluent 令牌 |
|---|---|
| `primary.active.background` | `accent.fill` |
| `secondary.foreground`、`popover.foreground`、`sidebar.foreground` | `text.primary` |
| `accent.background`、`sidebar.accent.background`、`muted.background` | `fill.subtle_hover` |
| `accent.foreground`、`sidebar.accent.foreground` | `text.primary` |
| `sidebar.primary.background` / `sidebar.primary.foreground` | `accent.fill` / `text.on_accent` |
| `list.background` | `bg.flyout` |
| `list.active.border` | `accent.fill` |
| `list.highlight.foreground`、`link`、`link.hover`、`link.active` | `accent.text` |
| `title_bar.background` | `window.background` |
| `title_bar.border`、`sidebar.border` | `stroke.divider` |
| `scrollbar.background` | 透明；高对比度时为系统窗口色 |
| `scrollbar.thumb.background` / `scrollbar.thumb.hover.background` | `text.tertiary` / `text.secondary` |
| `selection.background` | `accent.fill` 20% 不透明度（`accent.selection`） |
| `progress.bar.background` | `accent.fill` |
| `danger.hover.background`、`danger.active.background` | `status.critical` |
| `danger.foreground`、`success.foreground`、`warning.foreground` | `text.on_critical`、`text.on_success`、`text.on_caution`；亮色为白，暗色为黑 |
| `overlay` | `fill.smoke` |
| `window.border` | `stroke.surface` |

`Palette` 包含全部颜色字段，强调色、材质和高对比度覆盖直接生成类型化调色板；高对比度以完整结构构造逐一设置字段。系统变化不重新解析 JSON。组件颜色及绘制令牌在一次 `Theme::update` 中同步更新，高对比度下保持不透明。预览高对比度颜色只从 `preview` 功能内包含的独立 JSON 加载。标题栏使用原生 Windows 命中区域和 16 px Fluent 图标，按钮宽 46 px、高 32 px。图标的来源路径、提交与许可证保存在图标目录的 `manifest.json` 和 `LICENSE` 中。

gpui-component 控件形态与 Fluent 不同的地方（开关、复选框、滑块、标签页、焦点框为双层描边），在 M3 逐个检查，必要时在 `quartz-ui` 中包装或自绘。

### 9.3 检查

- 测试读取嵌入主题，检查全部 14 个分类色及 `neutral` 对 `bg.base` 不低于 3:1，以及每个分类色上的黑白自适应文字不低于 4.5:1。`neutral` 绘图色先与 `bg.base` 合成，再选择黑色或白色文字。
- 两个主题分别检查 `text.primary`、`text.secondary`、`accent.text` 对以下九种背景不低于 4.5:1：`bg.base`、其上的 `fill.layer`、层上的 `fill.card`、卡片上的控件默认/悬停/按下填充、底层上的列表悬停/按下填充、`bg.flyout`。透明填充先逐层与背景合成，再计算文字对比度。
- 另外检查 `text.on_accent` 对强调色默认/悬停填充，`status.success` 对 `status.success_bg`，以及 `text.on_critical`、`text.on_success`、`text.on_caution` 对各自状态填充。`text.tertiary` 用于坐标轴和辅助说明，`text.disabled` 用于禁用文字。测试 `embedded_categories_and_body_text_meet_wcag_contrast` 加 `--nocapture` 可输出各主题的实际对比度。
- 截图评审（07 第 11 节、10 第 7 节）覆盖：亮色、暗色、Mica 回退（关闭透明效果）、高对比度，每种都有 zh-CN 与 en。
- 代码检查遍历 `crates/quartz-ui/src` 内全部 Rust 文件，禁止 `rgb(`、`rgba(`、`hsl(`、`hsla(`、`#RGB[A]`、`#RRGGBB[AA]` 与六/八位 `0x` 十六进制颜色。唯一豁免是主题解码模块 `src/app/tokens.rs`，其中含检查器自己的测试样本；系统事件模块与所有视图均受检查。
