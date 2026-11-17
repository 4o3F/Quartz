# Quartz 设计文档

Quartz 是一个 Windows 优先、开源、本地优先的自动时间记录工具。它回答一个问题：**今天的时间去了哪里**。

设计目标按优先级排列：

1. **记录可信**：时长守恒，不把离开、排除、崩溃缺口算成活动；每段结果都能解释来源。
2. **常驻资源占用尽量小**：常驻进程不碰 GPU，界面按需启动，关窗即退出。
3. **隐私可控**：数据只在本机；敏感窗口在入库前按策略降级。
4. **结构可演进**：事实只记一次，分类、提取、统计都可以从事实重建。

## 章节

| 文档 | 内容 |
|---|---|
| [01-architecture.md](01-architecture.md) | 进程模型、入口识别、生命周期、crate 划分、线程模型 |
| [02-capture.md](02-capture.md) | Windows 采集：前台与标题、窗口身份、空闲与可用状态、时钟 |
| [03-data-model.md](03-data-model.md) | 表结构、约束与索引、写入与恢复、查询管线、缓存、保留与删除 |
| [04-ipc.md](04-ipc.md) | 命名管道协议：分帧、握手、角色权限、消息清单、订阅 |
| [05-browser.md](05-browser.md) | 浏览器扩展、Native Messaging host、关联与缓冲 |
| [06-privacy.md](06-privacy.md) | 通用敏感窗口隐私策略：识别器、采集级别、执行点 |
| [07-ui.md](07-ui.md) | 托盘、界面信息架构、时间轴、归类与标注体验 |
| [08-resources.md](08-resources.md) | 资源预算、测量方法与验收场景 |
| [09-milestones.md](09-milestones.md) | 里程碑划分、每阶段验收标准、待验证假设与待定项 |
| [10-i18n.md](10-i18n.md) | 界面语言与区域格式、翻译资源、存储中的语言中立、多语言文本匹配、字体与布局 |
| [11-design-system.md](11-design-system.md) | Fluent 2 设计系统：材质与层级、颜色令牌、分类色、字体、间距与密度、阴影、图标、动效、主题实现 |
| [12-self-regulation.md](12-self-regulation.md) | 扩展：CTDP 与 RSIP 自控协议的映射、模块结构、事件模型、监视与评估，以及首版预留的扩展点 |
| [../rules/rule-format.md](../rules/rule-format.md) | 对外规则格式 `quartz-rules/1`：字段、操作符、规则类型、校验错误码与示例，可直接交给 AI 编写规则 |

## 已定决策

| # | 决策 | 来源 |
|---|---|---|
| D1 | `quartz.exe` 承担常驻核心、按需 GPUI 界面和用户命令行；独立 console 子系统 `quartz-host.exe` 承担浏览器 Native Messaging 转发 | 用户决策，见 01 第 1–2 节 |
| D2 | 界面用 GPUI（gpui-kit 0.10.0），GPU 渲染；软件渲染在大窗口时间轴上只有约 26 fps，不采用 | [界面原型实测](../research/ui-spike-results.md)（历史原型使用 gpui-kit 0.9.1） |
| D3 | 关窗即退出界面进程；用托盘悬停预热、托盘摘要菜单、`STARTF_FORCEONFEEDBACK` 缓解约 4 秒的 dGPU 冷启动 | 用户决策 1 |
| D4 | 核心不在时，浏览器扩展保留有界短缓冲；核心入库时只保留与前台事实相交的部分 | 用户决策 2 |
| D5 | 原始数据只存事实；分类、文档提取、统计在查询时计算，用可重建缓存提速 | 用户决策 3 |
| D6 | 记录窗口标题，包括浏览器；标题是活动身份的一部分，标题变化切出新区间 | 用户决策 4 |
| D7 | 同一 exe 静态导入 D3D 系列 DLL 的常驻开销不阻塞设计；UI 隔离在 `quartz-ui`，日后可改 `/DELAYLOAD` 或拆 exe | 用户决策 5 |
| D8 | 敏感窗口隐私策略做成通用、可扩展的识别器框架，在核心采集层入库前生效，不只针对浏览器无痕 | 用户决策 6 |
| D9 | 不照搬 ActivityWatch 的心跳合并（pulsetime）与跨间隙合并；Quartz 用事件驱动的区间 | 先例调研 |
| D10 | 基础 crate 用 `windows`、`snafu`、`tokio`（只用 current-thread runtime）、`clap`、`tracing`；界面子进程由核心以显式 `ui` 子命令拉起，不按锁状态推断模式 | 用户决策 7，见 01 第 2、7 节 |
| D11 | 数据库访问用 diesel 与 diesel_migrations；迁移只向前，迁移前 `VACUUM INTO` 备份，数据库更新于程序时拒绝启动；内置规则按修订号升级且不覆盖用户修改 | 用户决策 8，见 03 第 4.5、7、8 节 |
| D12 | 数据库体积：固定枚举存整数（`enum_labels` 与 `_v` 视图保证可读）、结束行确认点置 NULL、区间序号不建唯一约束；标题保留 UNIQUE 文本去重 | 用户决策 9，见 03 第 1、2.9、4.6 节 |
| D13 | 国际化：首版 zh-CN 与 en，`rust-i18n` 与 gpui-component 共用语言设置；数据库与协议只存键和编码；规则匹配按 NFKC 与大小写折叠比较，结果与界面语言无关 | 用户反馈，见 10 |
| D14 | 界面设计系统采用 Fluent 2（以 WinUI 3 为准），分类色采用 Carbon 14 色分类色板 | 用户决策 10，见 11 |
| D15 | 为集成 CTDP/RSIP 预留扩展点：逻辑日起点偏移、统一截止时间调度器、共用条件引擎、托盘分区、时间轴叠加轨、IPC 前缀；协议本身在 M7，用户是唯一裁判 | 用户需求，见 12 |
| D16 | Quartz 不包含任何 LLM/Agent 功能。规则通过公开的文本格式 `quartz-rules/1` 编写，用户可自行交给任意 AI 生成；Quartz 只负责严格校验、预览与导入 | 用户决策 11，见规则格式文档与 12 第 8.1 节 |
| D17 | 所有规则以规则集组织并组成树：子规则集只写相对父级的差异（新增、覆盖、停用）；内置规则集为根，用户修改是覆盖层；规则集保留全部版本，支持版本 diff、结果 diff、三方对比与逐条件的解释树；规则版本号取生效规则的内容哈希 | 用户决策 12，见规则格式文档第 1、4 节与 03 第 2.6、8.5 节 |

## 术语

| 术语 | 含义 |
|---|---|
| 事实（fact） | 采集时直接观测到的数据：前台区间、可用状态、浏览器上下文。入库后不改写，只能结束、删除或清理字段 |
| 前台区间 | 某个窗口实例以某个标题处于前台的 `[start, end)` 时间段 |
| 窗口实例 | 一个具体的顶层窗口，在一次采集运行内由 HWND、PID、进程创建时间与窗口代次确定 |
| 可用状态 | 输入空闲、锁屏、睡眠、会话断开、用户暂停等，单独成表，查询时用来裁剪有效时长 |
| 派生数据 | 文档或项目提取结果、分类、日汇总。都可从事实和规则重建 |
| 标注（annotation） | 用户事后对一个时间范围的说明或覆盖，独立于事实保存 |
| 隐私字段动作 | `app: keep/drop`、`window_title: keep/drop`、`web: full/domain_only/none`；逐字段取最严格结果，见 06 第 2 节 |
| 采集级别 | `capture_level` 是前台字段动作的摘要：`full`、`title_hidden`、`excluded`；网页另用 `web_level` |
| 采集缺口 | `capture_gaps` 记录队列过载或数据库写入失败造成的未知范围，不计活动 |
| 核心 / UI / host | 常驻核心进程 / GPUI 界面进程 / Native Messaging 转发进程 |

## 参考资料

以下调研过程材料在 `.ccg/tasks/quartz-architecture-design/research/`：

- `backend-analysis.md`：采集、存储、IPC、进程、浏览器、资源、跨平台、crate 选型
- `frontend-analysis.md`：信息架构、托盘、归类、空闲体验、图表可行性
- `prior-art.md`：ActivityWatch 源码与 ManicTime、Timing、RescueTime、Toggl、WakaTime 的做法
- `synthesis.md`：两份报告的共识、分歧裁决和全部用户决策

界面选型的实测条件与数据见仓库内的 [ui-spike-results.md](../research/ui-spike-results.md)。
