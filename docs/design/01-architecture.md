# 01 总体架构

本文记录 Quartz 的进程与线程模型、职责边界、单实例与界面生命周期、crate 依赖规则和核心工程决策；具体常量、CLI 参数细节与内部装配以代码（`crates/quartz/src/`）为准，资源指标目标见 [08-resources.md](08-resources.md)。

## 1. 进程模型

```text
quartz.exe（windows 子系统，核心与界面共用）
├─ 核心模式（默认，常驻）
│  ├─ 主线程：Win32 消息循环、托盘、WinEvent hook、会话与电源通知
│  ├─ 采集线程：应用身份解析 → 隐私策略 → 计时状态机 → 写入队列
│  ├─ 数据库线程：diesel（SQLite）单写者，查询也在这里执行
│  └─ I/O 线程：tokio current-thread，命名管道服务
└─ 界面模式（`ui` 子命令，默认常驻，可配置）
   └─ GPUI：连接核心 → 无窗口等待或查询与渲染；关窗销毁窗口，按常驻设置保留或退出进程

quartz-host.exe（console 子系统，M5，由浏览器启动，不链接 GPUI）
└─ 浏览器 stdio ⇄ 长度与格式校验 ⇄ 命名管道
```

进程数上限：1 个核心 + 最多 1 个界面进程 + 每个已连接浏览器 profile 1 个 host。

| 模式 | 负责 | 不做 |
|---|---|---|
| 核心 | 采集、隐私策略、存储、分类与查询、托盘、管道服务、拉起界面 | 不初始化 GPU、字体系统或任何 GPUI 类型 |
| 界面 | 展示、交互、规则编辑、标注；所有数据经管道请求核心 | 不打开数据库，不做分类匹配 |
| host | 字节转发、帧长度与 JSON 结构校验、浏览器身份登记 | 不落盘，不缓冲，不拉起核心 |

**单一事实与计算源**：文档提取、分类匹配与聚合只在核心实现一份，避免服务端与前端各算一套导致口径分歧。

## 2. 入口识别

`quartz.exe` 在初始化日志、线程和数据库之前用 `clap` 解析参数（权威定义：`crates/quartz/src/main.rs`）：

| 调用形式 | 模式 | 说明 |
|---|---|---|
| 无子命令（可选 `--autostart`） | 核心 | 启动采集核心；重复启动按单实例规则转发或退出 |
| `ui [--prewarm \| --core-error <code> [--backup-path <路径>...]]` | 界面 | 隐藏子命令，由核心拉起；`--core-error` 展示核心启动致命错误 |
| `dump --date <日> [--format text\|json] [--offline]` | 命令行 | 默认经管道走 `query.facts` 先写临时文件再输出；核心停止时可用 `--offline` 只读直出 |
| `status` | 命令行 | 经管道查询 `core.status` |
| `settings get \| set ...` | 命令行 | 经管道读写语言、区域、常驻与预热设置 |
| `gen-data` / `bench-query` | 命令行 | 在独立模拟库生成数据与运行查询基准（不触碰生产库） |
| `restore-backup <文件> [--yes]` | 命令行 | 核心停止后用迁移备份原子替换生产库（无 `--yes` 仅提示并失败退出） |
| `rules check <文件>...` | 命令行 | 离线校验规则文件及与已安装规则集组成的树（预览与导入在 UI 完成） |
| 参数解析失败 | 报错退出 | 退出码 2，不进入核心，防止异常参数误拉起第二个核心 |

**为什么界面必须使用显式 `ui` 子命令**：核心重启或崩溃恢复期间数据目录锁会短暂空出，不能靠“锁是否被占”推断应启动核心还是界面；显式子命令让进程角色仅由调用意图决定，并自然区分普通打开、预热与错误窗口。

**控制台与早期诊断**：`quartz.exe` 为 `windows` 子系统，默认无控制台；仅在帮助/版本/参数错误及用户命令行子命令时调用 `AttachConsole(ATTACH_PARENT_PROCESS)`，核心和界面均不附加。日志初始化前的启动失败同时写入 stderr 与带 64 KiB 上限的 `logs/bootstrap.log`。`quartz-host.exe` 为独立 `console` 子系统入口，不链接 GPUI。

## 3. 单实例

- **跨会话数据目录锁**：核心启动后在打开数据库与管道前，通过 `LockFileEx` 独占数据目录的 `quartz.lock` 并持有至退出，保证同一用户跨 Windows 会话最多只有一个采集核心。
- **同会话快速转发**：核心另持有 `Local\Quartz.<SID>.core` 互斥量；同会话普通重复启动经管道发 `ui.open` 后退出，`--autostart` 重复启动直接退出。另一会话抢占锁失败时，核心以 `other_session` 启动错误窗口并以非零退出。
- **管道防抢注**：首个管道实例带 `FILE_FLAG_FIRST_PIPE_INSTANCE`。
- **界面单实例**：普通界面持有 `Local\Quartz.<SID>.ui` 互斥量，重复启动转发 `ui.open` 后退出；`--core-error` 错误窗口不占用该互斥量，确保已有界面运行时仍可弹出核心启动错误。

## 4. 界面生命周期

界面默认无窗口常驻（`ui.resident.enabled = true`），保留 GPUI 与 D3D 设备以缩短点击托盘到首帧的延迟；关闭常驻后按需启动，并支持托盘悬停预热。资源与首帧延迟目标（含常驻/预热/冷启动三组 P50 测量）见 [08-resources.md](08-resources.md)。

### 4.1 打开

- 左键点击托盘、托盘菜单“打开/设置”与管道 `ui.open` 共用同一打开路径。
- 核心主线程维护界面状态机：`None` → `Starting { intent: Show | Prewarm | Resident }` → `Registered { process, connection, phase: Starting | Prewarming | Hidden | Visible }` → `Closing { process }`，配独立显示意图 `show_requested`。
- 打开请求在 `None` 时用 `CreateProcessW`（直接执行 `current_exe() ui`，不经 shell）启动界面；在已登记且未可见时发 `ui.show`，已可见时发 `ui.activate`（发送前调用 `AllowSetForegroundWindow`）。
- 仅首个已登记连接受核心路由控制；其 `ui.first_frame` 确认后状态转为 `Visible` 并清除显示请求；连接注销转 `Closing` 并等待进程句柄退出后才回到 `None`（期间的新打开请求在旧进程退出后执行一次）。

### 4.2 无窗口常驻与悬停预热

- **常驻模式**：管道就绪（或运行中开启 `ui.resident.enabled`）时，核心拉起 `ui --prewarm` 并登记为无 TTL 的 `Hidden`；关闭常驻时向已隐藏界面发定向 `ui.shutdown`，可见界面在关窗后自行退出。常驻进程意外退出后按冷却期（60 s）在主线程等待队列中重启，不轮询。
- **悬停预热**：仅当常驻关闭、`ui.prewarm.enabled` 为真、电源满足 `ui.prewarm.on_battery`（经 `GetSystemPowerStatus` 判断）且冷却期已过时，托盘悬停触发 `ui --prewarm`；预热进程握手后受短 TTL（8 s）约束，显式打开优先于 TTL，到期无显示请求即发 `ui.shutdown`。

### 4.3 关闭

- 最后窗口关闭时销毁窗口：有效常驻开启且连接正常时发送 `ui.hidden` 保留无窗口进程；常驻关闭时调用 `cx.quit()` 释放 GPU 设备并退出。
- 已连接的界面在管道断开后直接关闭窗口并退出。用户从托盘“退出 Quartz”时，核心先广播 `ui.shutdown`，再限时排空并关闭管道、结算采集、关闭数据库。

### 4.4 核心不在时的界面

- 界面首次连接失败即展示统一错误界面（07 第 2.5 节），不做后台自动重连。
- 用户点击“启动核心”前，界面先监听内核事件 `Local\Quartz.<SID>.v1.ready`（由核心管道创建监听实例后置位），再直接拉起无参数 `current_exe()`，可取消阻塞等待就绪后连接一次（超时显示可重试错误），全程不轮询管道、不直接读库。

## 5. crate 划分

```text
crates/
├─ quartz          exe 入口；核心装配、平台层、存储、查询、托盘
├─ quartz-host     独立 Native Messaging 主机（M5），不链接 GPUI
├─ quartz-domain   纯逻辑：计时状态机、区间运算、可用状态裁剪、规则匹配、提取、隐私求值
├─ quartz-protocol 消息类型、分帧编解码、版本与错误码
├─ quartz-pipe     共享命名管道客户端、对端进程身份校验、分帧与请求关联
├─ quartz-i18n     翻译资源、语言协商、复数、时长与日期格式化（10）
└─ quartz-ui       GPUI 界面，工作区内仅依赖 quartz-protocol、quartz-pipe、quartz-i18n
```

未来 M7 的自控纯逻辑将放在同级 `quartz-protocols`（见 12 第 4 节）。

**架构边界与依赖规则：**

- **纯领域层**：`quartz-domain` 不依赖 Windows API、SQLite 与 GPUI，时间由外壳传入，可在任意平台值测试。
- **函数式核心 + 命令式外壳（sans-IO）**：状态有界且可常驻内存的组件（采集状态机、规则注册表、截止时间调度器）在 `quartz-domain` 中纯函数式决策，输出领域写入描述与回复；外壳（`core::app` 及各线程循环）执行数据库与平台 I/O，提交成功后再把结果交回核心。需要按需扫描大量数据的查询管线属于外壳，直接在数据库线程只读执行。
- **平台与存储隔离**：Win32 / COM / HWND / HANDLE 封闭在 `quartz/src/platform/windows/`，对外只暴露领域事件与端口 trait；diesel 与 SQL 封闭在 `quartz/src/store/`，只做持久化与类型转换。
- **界面隔离**：`quartz` 不直接渲染窗口或错误文案（托盘菜单/提示经 `TrayState`/`TrayCommand` 隔离）；对 `quartz-ui` 的依赖仅入口单点调用，便于日后独立拆分 `quartz-ui.exe`。

`quartz` crate 内部模块职责：

| 模块 | 职责 |
|---|---|
| `main` | CLI 解析与模式分发 |
| `core::app` | 核心装配、线程启停与统一关闭路径（含 `WM_ENDSESSION` 最终持久化） |
| `core::{foreground, sources}` | 前台事件过滤与系统采样时间转换、会话/空闲处理 |
| `platform::windows::{foreground, sources, tray, process, ipc}` | Win32 前台解析（采集线程 COM）、观测源、托盘、进程信息与命名管道承载 |
| `persistence` | 数据库线程写入队列、合批事务、失败缓冲与采集过载反馈 |
| `store` | schema 迁移、事务写入、只读查询、版本与备份恢复 |
| `rules::registry` | 规则注册表外壳：单写者事务执行与不可变快照发布（第 6.2 节） |
| `query` | 分批只读查询调度、切片聚合、版本摘要与单帧截断 |
| `ipc` | 会话状态机、角色授权、订阅注册表、连接 outbox 与 CLI 客户端 |

## 6. 线程模型

核心常驻 4 个工作线程（具体通道容量与批处理常量以 `crates/quartz/src/` 为准，线程总数预算见 [08-resources.md](08-resources.md)）：

| 线程 | 职责 | 阻塞与隔离规则 |
|---|---|---|
| 主线程 | Win32 消息循环、WinEvent hook、托盘、会话/电源/时钟通知、空闲定时器 | 全部 hook 生命周期在主线程；变化观测才入队，不做磁盘 I/O |
| 采集线程 | 进程身份解析、隐私求值、计时状态机、生成写入命令 | 可调 `OpenProcess` 等慢 API，不访问数据库 |
| 数据库线程 | 持有写连接与只读连接：批量落盘采集与写请求，分批执行只读查询 | 查询按批让出给采集提交；写提交期限与关闭优先于查询 |
| I/O 线程 | tokio current-thread：管道 accept、读写、连接注册表与每连接 outbox | 不访问 SQLite；查询与写请求通过有界通道转数据库线程，满即返回 `busy` |

```text
主线程 ──ObservedEvent──▶ 采集线程 ──WriteCommand──▶ 数据库线程 ◀──Query/Write── I/O 线程
   ▲                          │                              │
   └──── 托盘与 UI 控制命令 ◀──┴─────── StateChanged ─────────┴──▶ I/O 线程（推送）
```

**有界通道与过载不变量：**

- 线程间全部使用有界队列，生产端绝不因下游阻塞而卡死主线程或采集。
- 主线程→采集线程队列满时丢弃新事件并置过载标志；采集线程发现后将当前区间结束于最后可信时间，写入不含应用/标题的 `capture_gaps`，恢复后重读前台。
- 采集线程→数据库线程共用有界命令预算（排队、失败缓冲与提交中前缀合计），满时优先合并可替换的确认点，仍满则记录 `capture_gaps`；数据库写入失败期间托盘示警，恢复后优先落盘缺口。
- 采集线程若收到单调时间倒退的观测，以已接受边界作为生效时间，保留原始证据字段。
- 通知主线程队列对状态/托盘/hook 目标按类合并，致命故障占独立优先槽位并停止新观测。

### 6.1 截止时间调度器

暂停、隐私模式、保持计时、空闲例外及未来 M7 协议计时统一注册到主线程截止时间调度器：

- 只为最近的一个截止时间设置一次性 `SetTimer`，无截止时间时不设定时器、零周期唤醒。
- 限时策略持久存于 `timed_policies`（其余定时截止存 `settings`）；启动时立刻结算已过期项、重排未过期项；系统时钟跳变后重算最近期限；到期事件交由拥有模块产生 `policy_changed` 等边界。

### 6.2 规则注册表的并发模型

规则被采集线程与数据库线程高频并发读取，又会被 UI 修改或内置升级更新，由三种模式协作：

| 模式 | 解决的问题 | 做法 |
|---|---|---|
| Actor（单写者） | 写竞争 | 规则注册表在数据库线程串行处理全部 `RegistryCommand`（`Apply`、`Import`、`Toggle`、`Delete`、`InstallBuiltin`） |
| RCU（读-复制-更新） | 读写竞争 | 生效规则编译为不可变 `Arc<RuleSnapshot>` 存入 `arc-swap`；读者无锁原子加载，单工作单元全程使用同一快照 |
| OCC（乐观并发） | 覆盖丢失 | 修改携带 `base_version`，单写者原子比较，过期返回 `rule_set_stale` |

**sans-IO 执行顺序与不变量：**

```text
RegistryCommand ─▶ RuleRegistry::decide（纯校验、OCC、编译）─▶ Vec<RuleSetWrite>
   ─▶ 外壳单事务落盘（失败直接返回，不改内存）
   ─▶ RuleRegistry::committed ─▶ 新快照、epoch + 1、原子替换、通知采集线程
```

1. **先编译校验、后落盘、再发布**：写库失败绝不污染内存快照；提交后发布前崩溃时，重启以数据库重建快照（数据库为唯一事实来源）。
2. **epoch 与内容哈希分离**：进程内 `epoch` 单调递增，用于采集线程触发 `policy_changed` 边界（`RulesChanged` 通知只作及时唤醒，正确性由 `epoch` 比较保证）；跨查询与版本摘要使用生效规则内容 SHA-256 哈希。
3. **启动容错**：若某个已保存规则集启动编译失败，仅停用该集并在规则集树告警，不阻断其余规则。

## 7. 依赖选型与工程约定

基础 crate：`windows`、`snafu`、`tokio`、`clap`、`tracing`。其余关键选型：

| 领域 | 选型 | 理由与边界 |
|---|---|---|
| Win32 / 托盘 | `windows`、`tray-icon` | 集中在 `platform::windows`，不混用 `windows-sys` |
| 存储与迁移 | `diesel` + `diesel_migrations` + `libsqlite3-sys(bundled)` | 仅限 `store` 模块；启动校验 SQLite ≥ 3.51.3 |
| 界面 | `gpui-kit`（固定版本） | 仅限 `quartz-ui` |
| 规则并发与匹配 | `arc-swap`、`regex`（设 `size_limit`/`dfa_size_limit`）、`idna`、ICU4X | 规则快照 RCU、正则内存护栏、跨语言文本与域名规范化 |
| 日期与本地化 | `jiff`、`rust-i18n` | Windows 系统时区 + 内嵌 IANA 历史数据；翻译资源集中在 `quartz-i18n` |

- **文件日志**：独立低开销写线程非阻塞接收记录，无周期唤醒；`core.log` / `ui.log` 按大小滚动（单文件 2 MiB、保留 3 份），panic hook 仅落固定诊断记录（不写 panic 载荷，不写标题/域名/URL，见 06 第 5 节）。
- **发布配置**：`lto = "fat"`、`codegen-units = 1`、`panic = "abort"`（不生成含敏感内存的转储）、`strip = true`。

### 7.1 错误处理

内部统一使用按失败操作命名的 `snafu` 错误枚举并保留下层 `source`（`anyhow` 仅在 `quartz-ui` 的 GPUI 边界出现）；跨进程只传 `quartz-protocol` 稳定错误码，不解析错误文本；错误 `Display` 同样遵守日志隐私约束（不携带标题、域名、URL）。

### 7.2 tokio 使用范围

tokio 仅用于命名管道与 host stdio 异步 I/O，绝不接管 Win32 消息循环、SQLite 或 GPUI。所有 runtime 必须使用 `Builder::new_current_thread()` 并限制小容量 `max_blocking_threads`（核心 2、host 1、界面 1），避免多线程 runtime 突破 08 的线程预算。

## 8. 自启、数据目录与更新

- **开机自启**：写入 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Quartz`（值 `"<exe 绝对路径>" --autostart`），以注册表为唯一事实源（不存 SQLite）；仅当注册表值存在且与当前可执行文件路径完全一致时视为已开启，启用时覆写旧路径，停用时删除该值（值不存在亦视为成功），可在设置开关。
- **目录分离**：程序目录与数据目录分离；数据目录位于 `%LOCALAPPDATA%\Quartz\`，包含 `quartz.db`、`logs\`、`icons\`、`backups\` 与单实例锁 `quartz.lock`。
- **更新与备份恢复**：从托盘退出后覆盖 exe 再启动（步骤见 [安装说明](../install.md)）。数据库升级、自动迁移前备份与 `quartz restore-backup` 降级恢复见 03 第 7–8 节。
