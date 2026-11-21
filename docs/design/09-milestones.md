# 09 里程碑

每个里程碑都能单独验收。实现由 Codex 分阶段完成，每次外部调用不超过 40 分钟。每个里程碑结束时：

- 跑资源回归（08 第 2.3 节）；
- 有界面改动时先截图给用户确认；
- 提交前交给用户审查，用户确认后才提交。

## M0 骨架与资源基线

**交付**

- Cargo workspace：`quartz`、`quartz-host`、`quartz-domain`、`quartz-protocol`、`quartz-i18n`、`quartz-ui`；`quartz-i18n` 带 zh-CN、en 翻译骨架与键完整性测试（10 第 2 节）；Windows 构建脚本；本项目的 `.cargo/config.toml` 关闭 kache wrapper。
- 入口模式识别（01 第 2 节）：clap 参数解析与单元测试，覆盖核心、界面、用户子命令及未知参数报错。
- 核心：消息循环、托盘（打开、退出）、跨会话数据目录锁文件与会话内互斥量。
- `quartz ui` 打开一个空白的 GPUI 窗口，关窗后进程退出。
- `deno task measure --Exe ./target/release/quartz.exe`，通过 Deno FFI 采集 Windows 资源基线。

**验收**

- 核心 Private Bytes、线程数、加载模块都有基线记录；没有加载 `nv*.dll`。
- 界面开关 20 次后，核心没有增量，界面进程都已退出。
- 这是"同一 exe 静态导入 D3D"的第一次实测。结果只作记录，不阻塞后续里程碑（用户决策 5）。

## M1 采集与存储

**交付**

- 前台和标题 hook、窗口与应用身份解析、空闲检测、会话和电源通知、时钟段（02）。
- schema v1（diesel 迁移）、`store` 访问层、批量写入、确认点、崩溃恢复（03 第 2–4 节）；迁移启动流程、`VACUUM INTO` 备份、`restore-backup` 命令，以及 schema.rs 漂移检查与 `migrations.lock` 校验（03 第 8 节）。
- 隐私策略框架、字段动作、`builtin.browser_private`、`builtin.credential_ui`、`builtin.password_manager` 与用户规则求值（06）。M1 验证用户在用的 Edge、Chrome、Firefox 无痕模式与语言样本；标题无法识别的浏览器记为已知缺口，M5 起以扩展关联为准（06 第 3.4 节）。规则集基础设施在 M1 完成：`quartz-rules/1` 解析与校验、规则集树解析（rule / override / disable、冲突检测）、版本存储、生效规则哈希、内置规则集 `builtin.privacy` 及其升级流程（03 第 8.5 节）。用户隐私规则先通过规则文件编辑，`quartz rules check` 在 M1 提供；界面在 M4 提供。
- `quartz dump --date <YYYY-MM-DD> [--format text|json]`：M1 经数据目录直接只读打开生产数据库，不取得核心实例锁，在一个 WAL 快照中读取本地日历日相交的运行、时钟段、前台与无前台事实、可用状态、采集缺口及限时策略。默认文本每行一条，按开始 UTC 毫秒、类型和主键排序，包含本地起止时间、UTC 毫秒、时长、结束原因及已存储字段；开放行显示 `open (confirmed …)`。字符串使用 JSON 转义；`--format json` 每行一个对象，字段包含 `kind`、`id`、`start_utc_ms`、`end_utc_ms`、`confirmed_utc_ms`、`duration_ms`、`end_reason` 和相应事实字段，以及 `start_local`、`end_local`、`confirmed_local`。端点保留完整存储范围，标题仅在数据库有对应文本时出现。
- 规则匹配的文本比较（NFC、NFKC 与大小写折叠、IDN 转换）及多语言测试用例（10 第 4.1、7 节）；内置识别规则按语言列出变体，验证 zh-CN 与 en 浏览器样本（10 第 4.2 节）。
- `quartz gen-data --db <新文件> [--profile light|typical|heavy] [--seed N] [--days N]`：默认一年，复现 03 第 4.6 节的三档前台与标题参数，包含高频标题和长标题。网页行待采集写入器支持后生成，当前输出跳过说明及目标切片数。`quartz bench-query --db <模拟库> [--runs N]`：默认每种范围 100 次，M1 先跑前台、可用状态和缺口的事实范围读取。
- capture_gaps、deletion_barriers、统一确认点与数据库写入故障处理。

**验收**

- 02 第 6 节的全部场景。
- 时长守恒检查：前台与无前台事实，加停止原因的时间并集共同覆盖运行时间，无重叠；过载范围以 capture_gaps 记录，idle 独立作为掩码检查。按运行与时钟段顺序验证。
- 核心资源满足 08 第 1.1 节，静态导入专项偏差按该节处理。
- 数据库写入失败、队列满、恢复后缺口边界正确；崩溃恢复同时关闭全部开放记录与旧时钟段。
- 迁移测试：样本库迁移到最新、schema 一致性、“数据库更新、程序更旧”拒绝启动、迁移中途失败后可从备份恢复（03 第 8.4 节）。
- 记录 diesel 引入后的 release 体积与增量编译时间。

## M2 IPC 与托盘

**交付**

- 命名管道服务：分帧、握手、DACL、客户端身份检查、角色权限（04）。
- `query.day`、`query.timeline`、`query.pending_away`、`core.status`、订阅 `state`。
- 最小查询管线：裁剪、保留空闲掩码、可用状态、规则分类与人工覆盖语义，暂不做文档提取。交付默认分类与基于已有事实字段的预置规则。
- 标注存储、UUID 去重、同类重叠覆盖裁剪、dismissed_away；供托盘快速标注使用。 M2-D2 提供创建、最终值更新、删除及其原子事务、字段校验和日版本更新；四种覆盖类型裁剪同类旧标注，note 保留重叠。M2-D3 在同一只读快照内读取标注，按边界切片应用分类覆盖、有效活动覆盖与离线活动，接入日指标、小时堆叠、时间轴和版本化 `query.pending_away`。托盘入口在 M2-E 实现。
- 数据库写连接与只读快照连接、版本化分页；bench-query 接入最小查询管线。
- 托盘：通过 `quartz-i18n` 本地化（zh-CN、en），语言设置切换后立即生效；提示文字、摘要菜单、暂停、保持计时、隐私模式、空闲结束后的快速标注。
- 生产数据的命令行查询改为通过管道；gen-data 与 bench-query 保持独立模拟库的离线测试入口。

**验收**

- 04 第 9 节中分帧、UI/CLI 权限、慢客户端、UUID 重试与分页版本测试；host 专属测试在 M5 验收。
- 停止读取的慢客户端不影响采集。
- 托盘摘要与 `quartz dump` 查询结果一致。

## M3 界面 v1

**交付**

- Fluent 2 主题文件与 gpui-component 令牌映射、Mica 窗口与回退、系统强调色与高对比度、Fluent 图标集、对比度检查测试（11）。
- 界面通过管道连接核心，断线后重连。
- 今天页：日期栏、状态行、时间轴（分类主轨、应用泳道、悬停、点击、缩放、平移）、按小时堆叠柱、排行。
- 详情侧栏（只读）。M3 的应用泳道先按窗口实例与原始标题显示，M4 接入文档提取身份。
- 悬停预热、`STARTF_FORCEONFEEDBACK`、"正在打开…"提示。
- 修复 M0-B2 记录的 GPUI Windows 帧调度 / `VSyncProvider` 空闲 CPU 偏差，达到 30 秒内 ≤ 20 ms 的原预算（08 第 1.3 节）。

**验收**

- 截图评审通过，每次同时提供 zh-CN、en 与伪语言 en-XA 三套截图（10 第 6、7 节）；运行中切换语言后界面与组件文字立即变化。
- 界面资源满足 08 第 1.3 节；静止时 CPU 达标。
- 预热实测：用户点击到首帧的 P50；复核默认开启、电池时关闭。验证预热握手前点击、TTL 与显式打开竞争、首帧确认与核心重启后的 UI 重新登记。

## M4 分类、提取、标注

**交付**

- 查询管线完整实现，带三层缓存（03 第 5 节）。
- 在 M2 分类基础上补齐文档提取内置规则与依赖提取字段的分类规则。
- 分类与规则页：规则编辑与命中预览、未分类队列、提取规则、隐私规则。
- 在 M2 标注存储与覆盖语义基础上完成编辑体验：待标注离开、时间轴拖选批量标注、右键“归类为…”。
- 隐私规则应用到历史数据。
- 规则集树页、生效视图、版本 diff 与回退、三方对比、片段解释（`rule_sets.*`、`rules.explain`、`rules_text.*`、`quartz rules preview/diff/explain/tree`），“复制情况说明”；规则格式文档中的字段表、操作符表与错误码表由源码生成或由测试校验一致。

**验收**

- 同一份事实在今天页、时间轴、排行、规则预览中得到相同的分类和时长。
- 接受规则版本变化使全部日缓存失效；标注与事实修改按日期失效。应用显示名、profile 标签与分类元数据变化递增 metadata_version，日缓存、托盘与 day 订阅同步失效。
- 一年模拟数据的查询性能满足 08 第 1.4 节。

## M5 浏览器

**交付**

- Chrome/Edge 扩展、Firefox 扩展（05 第 1 节）。
- `quartz-host.exe` 的 Native Messaging 转发与注册（05 第 2 节），设置页的浏览器分组。
- 核心的上下文关联、求交、回执与去重（05 第 3 节）。
- 浏览器隐私以扩展为准：有已连接 stream 时未关联窗口隐藏标题、关联后切段恢复，未接入时由 `builtin.browser_private` 标题模式兜底；扩充样本库与关联验证（06 第 3.2–3.4 节）。
- browser.policy、purge 确认、gap 与连续 ack，扩展 alarm 重连、开放网页行恢复。
- 将关联后的开放网页行加入 capture 的统一确认点目标与崩溃恢复输入；与前台、可用状态、运行和时钟段在同一事务中结算。

**验收**

- 05 第 4 节的全部场景。
- 多窗口、多 profile 时的 `linked` 覆盖率有实测数据。
- 无痕窗口的标题不出现在数据库、日志、推送和托盘中。

## M6 完善与发布

**交付**

- 本周页、设置页其余部分。
- 自启、保留期清理、按范围删除、压缩数据库、备份。
- 完善 M1 的 `gen-data` 与 `bench-query` 工具；依赖审计（确认没有网络请求代码）。
- 安装说明：解压即用，在设置里启用自启和浏览器扩展。

**验收**

- 08 的全部指标，并附 8 小时长时间运行的结果。

### 持续集成与发布

推送到 `main`、提交拉取请求或手动触发时，GitHub Actions 在 `windows-latest` 上先使用 Deno 2.9.8 运行 `deno task check`，检查 `scripts/` 的格式、lint 和 TypeScript 类型；依赖精确版本写在 `deno.json`，由 `deno.lock` 锁定并缓存。随后使用 `rust-toolchain.toml` 固定的工具链，先运行 Rust 格式检查、`cargo deny check` 和 `deno task check-schema`，再运行 Clippy（警告视为错误）与工作区测试；构建与测试同时校验 `migrations.lock`。测试不开窗，CI 不运行 GUI 预览及资源、体积测量。检查通过后构建 release 版 `quartz.exe` 与 `quartz-host.exe`，由 `deno task package collect --output <目录>` 收集二进制与列出二者 SHA256 的 `SHA256SUMS`，上传为 workflow artifact（`quartz-<commit>-x86_64-pc-windows-msvc`，保留 14 天）。Diesel CLI 固定为 2.4.0，使用 `sqlite-bundled` 功能安装并缓存。依赖更新由 Renovate（仓库根目录 `renovate.json`）发起 PR：Cargo 依赖按 `bump` 更新 `Cargo.toml` 与 `Cargo.lock`，Actions 以完整版本号引用；需要同步升级的依赖放在同一分组（Diesel 与 `libsqlite3-sys`、Diesel CLI；ICU4X 与 `idna`；windows-rs；tracing；serde；Deno 运行时与脚本依赖；GitHub Actions），Rust 工具链、Diesel CLI 与 Deno 版本由自定义规则跟踪。minor、patch 与 lockfile 维护通过 GitHub 原生 auto-merge 自动合并：`main` 的 ruleset 要求 `Windows checks` 通过（仓库管理员可绕过，用于直接推送），major 更新与 `gpui-kit` 需人工审核。

推送 `v*` 标签后，发布工作流运行脚本检查，以 `deno task verify-version` 核对标签是否等于 `v` 加工作区版本，再构建 `quartz.exe` 和 `quartz-host.exe`，通过 `deno task package release --output <目录>` 生成 `quartz-<version>-x86_64-pc-windows-msvc.zip`。压缩包内的 `SHA256SUMS` 列出两个可执行文件的 SHA256，同名 `.zip.sha256` 文件记录压缩包的 SHA256。工作流先把压缩包与校验文件上传为 workflow artifact（保留 90 天），再上传这两个附件并创建带自动发布说明的 GitHub Release 草稿，由用户手动发布；当前不进行代码签名。

## M7 自控协议（CTDP 与 RSIP）

前置：全局热键、可交互通知（Windows toast，需注册 AUMID）。

**交付**

- `quartz-protocols` crate：CTDP 链状态机（主链、预约链、二次预约、侦查、层级编号、储君继承）与 RSIP 国策树（每日最多加一个、递归熄灭、国策组容错、冻结、强化、内化进度），全部带单元测试（12 第 1、4 节）。
- 协议事件表、判例表与派生状态缓存（12 第 5 节）。
- 疑似违规监视、国策自动检查与自动建议（12 第 6 节）。
- 托盘分区、热键、通知裁决、专注页、国策树页、每日复盘（12 第 8 节）。

**验收**

- 事件回放得到的链长与国策树状态，与逐步操作时界面显示一致；删除派生缓存后可完全重建。
- 未启用协议时，资源与首版相同（08）；进行中的协议不产生周期唤醒。
- 疑似违规误报率有实测数据（H12）。

## 以后

规则冲突（抢占）分析、7×24 热力图、CSV 与 JSON 导出、Taix 导入、Ctrl+K 搜索、每日回顾、前台应用音频例外、持久日缓存、低功耗显卡选择（切回混合显卡模式后）、macOS 版本。

## 待验证假设

按风险从高到低：

| # | 假设 | 在哪里验证 | 不成立时 |
|---|---|---|---|
| H1 | 同一 exe 里，核心不创建 D3D 设备就不会加载 NVIDIA 驱动，核心资源达标 | M0 | 记录静态导入偏差，不阻塞里程碑；后续评估 `/DELAYLOAD` 或拆 exe |
| H2 | 事件触发当前状态读取，加 60 秒后备复核，通知覆盖缺口 `untriggered_changes` 每小时 ≤ 1；只计 60 秒后备复核在无触发时发现的变化（02 第 2.1 节） | M1-E2b 已测：约 30 分钟真实切换（含虚拟桌面、跨显示器、UAC、锁屏）计数为 0，切换误差 P95 7–12 ms；一小时连续轮待测。[测量记录](../measurements.md) | 定位反复出现的来源，补订阅对应事件（如 `EVENT_SYSTEM_SWITCHEND`）并重新验收；保持 60 秒后备间隔与计数口径 |
| H3 | GPUI 能在不开窗的情况下完成 D3D 初始化，因此悬停预热有效 | M3 | 预热时开一个不可见的窗口；或放弃预热，只保留托盘摘要 |
| H4 | 仅凭 profile、扩展焦点和标题匹配，就能可靠关联浏览器多窗口 | M5 | 保持 ambiguous；如需 UI Automation 等信号，另立原型评估权限、成本与影响 |
| H5 | 用户在用的浏览器无痕窗口可以从类名或标题识别 | M1-E2b 已测：Edge 157、Firefox 160 的 zh-CN 与 en-US 无痕窗口可由标题识别；Chrome 157 无痕标题与普通窗口相同，无法识别，记为已知缺口（未启用保守规则）。[测量记录](../measurements.md)；M5 扩充样本 | M5 起以扩展关联为准，未关联窗口隐藏标题；未接入扩展的浏览器由标题模式兜底并在设置页提示（06 第 3.4 节） |
| H6 | 已决定使用独立 console 子系统 `quartz-host.exe` 承接浏览器 stdio | M0 已由设计解决 | — |
| H7 | 验证常见应用窗口的显示亲和性查询覆盖、错误码与成本 | M1 | 查询失败记 unknown，其他识别器与用户规则继续求值 |
| H8 | 一年数据下，内存缓存就能满足查询性能 | M4 | 加持久日缓存表 |
| H9 | diesel 的泛型代码对 release 体积、编译时间与核心内存的影响可以接受 | M1-B 已测：release +2.036 MiB；Windows debug 一行增量编译 9.028 s；短时 Private Bytes +0.754 MiB、线程数不变。[测量记录](../measurements.md)；10 分钟资源回归待 M1 完成 | 热路径写入改用 `sql_query`，或把 store 拆成独立 crate 缩短增量编译 |
| H10 | ICU4X 规范化与大小写折叠数据加入核心后，核心体积与内存仍满足 08 | M1-A 已测：规范化与折叠 +156 KiB，含 IDN +292.5 KiB；[测量记录](../measurements.md)，常驻内存待 M1 回归 | 改用 `unicode-normalization` 与 `caseless` 等更小的 crate，保持第 10 章的比较语义 |
| H11 | GPUI 能在 Windows 上正确呈现 Mica 窗口与半透明令牌，并通过 DirectWrite 选中 Segoe UI Variable 的光学尺寸实例和 `tnum` 特性 | M3 | Mica 不可用时用 `bg.base` 纯色；字体退回 `Segoe UI Variable` 单一字族，等宽数字用固定宽度容器 |
| H12 | 基于前台事实的疑似违规检测，在宽限期与判例过滤后误报足够少，不打扰专注 | M7 | 默认只在用户主动报告时进入裁决，自动检测改为任务结束后的回顾提示 |

## 待定项

| # | 问题 | 何时决定 |
|---|---|---|
| Q1 | 已决定使用 `jiff`（当前最新发布版 1.0.0） | M1-A，首次使用时间功能时加入依赖 |
| Q2 | 已决定通过 `windows` 直接调用 `Shell_NotifyIconW`，以接收悬停事件并减少依赖。 | M0 |
| Q3 | 复核预热默认开启、电池时关闭 | M3 实测后复核 |
| Q4 | 数据库是否加密 | 以后 |
| Q5 | 内置提取规则覆盖哪些应用 | M4，按用户实际常用的应用列表 |
