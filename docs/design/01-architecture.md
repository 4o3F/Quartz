# 01 总体架构

## 1. 进程模型

```text
quartz.exe（windows 子系统，核心与界面共用）
├─ 核心模式（默认，常驻）
│  ├─ 主线程：Win32 消息循环、托盘、WinEvent hook、会话与电源通知
│  ├─ 采集线程：应用身份解析 → 隐私策略 → 计时状态机 → 写入队列
│  ├─ 数据库线程：diesel（SQLite）单写者，查询也在这里执行
│  └─ I/O 线程：tokio current-thread，命名管道服务
└─ 界面模式（`ui` 子命令，按需启动）
   └─ GPUI 窗口：连接核心 → 查询与订阅 → 渲染；最后一个窗口关闭即退出进程

quartz-host.exe（console 子系统，由浏览器启动，不链接 GPUI）
└─ 浏览器 stdio ⇄ 长度与格式校验 ⇄ 命名管道
```

进程数上限为：1 个核心，加最多 1 个界面进程，再加每个已连接的浏览器 profile 一个 host。

各模式的职责边界：

| 模式 | 负责 | 不做 |
|---|---|---|
| 核心 | 采集、隐私策略、存储、分类与查询、托盘、管道服务、拉起界面 | 不初始化 GPU、字体系统或任何 GPUI 类型 |
| 界面 | 展示、交互、规则编辑、标注；所有数据经管道请求核心 | 不打开数据库，不做分类匹配 |
| host | 字节转发、帧长度与 JSON 结构校验、浏览器身份登记 | 不落盘，不缓冲，不拉起核心 |

分类、提取、统计只在核心实现一份。这是为了避开 ActivityWatch 的问题：它的服务端和 WebUI 各实现一套分类匹配，结果不一致。

## 2. 入口识别

`quartz.exe` 用 `Args::try_parse()` 解析自身命令，确定模式后再初始化日志、线程、数据库和平台设施。浏览器通过 Native Messaging manifest 启动独立的 `quartz-host.exe`。

`quartz.exe` 的命令行定义如下：

```rust
#[derive(Parser)]
#[command(name = "quartz", version)]
struct Args {
    /// 由自启项传入，只影响启动提示与延迟初始化
    #[arg(long)]
    autostart: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 界面进程，只由核心拉起
    #[command(hide = true)]
    Ui {
        #[arg(long, conflicts_with = "core_error")]
        prewarm: bool,
        #[arg(long)]
        core_error: Option<CoreErrorCode>,
        #[arg(long, requires = "core_error", required_if_eq("core_error", "migration_failed"))]
        backup_path: Vec<PathBuf>,
    },
    /// 打印某天的区间（经管道以 cli 角色查询核心；M1 可直接只读打开数据库）
    Dump { #[arg(long)] date: String },
    /// 核心状态
    Status,
    /// 向独立模拟库写入一年模拟数据，不触碰生产数据库
    GenData { #[arg(long)] db: PathBuf },
    /// 只读模拟库的查询基准
    BenchQuery { #[arg(long)] db: PathBuf },
    /// 用迁移前备份替换数据库；核心必须已退出
    RestoreBackup { file: PathBuf, #[arg(long)] yes: bool },
    /// 规则文本的校验与预览（quartz-rules/1，见 docs/rules/rule-format.md）
    #[command(subcommand)]
    Rules(RulesCommand),
}

#[derive(Subcommand)]
enum RulesCommand {
    /// 校验规则文件及其与已安装规则集组成的树，按固定格式输出错误（离线，不需要核心）
    Check { files: Vec<PathBuf> },
    /// 以下经管道以 cli 角色请求核心（只读）
    Preview { file: PathBuf, #[arg(long, default_value_t = 7)] days: u32 },
    Diff { file: PathBuf, #[arg(long, default_value_t = 7)] days: u32 },
    Explain { #[arg(long)] at: String },
    Tree { #[arg(long)] kind: Option<String> },
}
```

| 解析结果 | 模式 |
|---|---|
| 无子命令 | 核心 |
| `ui [--prewarm]` | 界面 |
| `dump`、`status`、`gen-data`、`bench-query`、`restore-backup`、`rules check/preview/diff/explain/tree` | 命令行 |
| 解析失败 | 报错退出，不进入核心 |

解析失败时退出码为 2，不启动核心。这样，浏览器或其他调用方传入未预料的参数时，不会误启动第二个核心。`ui` 在帮助中隐藏，它是核心拉起界面时使用的进程间约定。

**为什么界面需要显式子命令。** 界面和核心是同一个 exe，但启动后的行为完全不同。核心拉起界面时必须明确告诉子进程“你是界面”：

1. 不能靠“核心是否已在运行”推断。核心重启、更新或崩溃恢复期间，数据目录锁会短暂空出；按推断启动的界面子进程会拿到锁，变成第二个核心，开始采集。显式子命令让模式只取决于调用方意图，与时序无关。
2. 预热进程（`ui --prewarm`）不开窗口，它的行为本来就需要用参数区分。
3. 用户双击 exe 或从开始菜单启动时不带参数，走核心路径：核心已在运行时，把“打开界面”转发给核心后退出（第 3 节），由核心按第 4 节拉起界面。所以界面进程始终由核心创建，UI 状态机（`starting`、`prewarming`、`visible`）只有核心这一个所有者，不会和用户直接启动的界面实例竞争。

**控制台输出。** `quartz.exe` 使用 `windows` 子系统，启动时没有控制台。`Args::try_parse()` 返回帮助、版本或解析错误时，先调用 `AttachConsole(ATTACH_PARENT_PROCESS)`，再由 clap 输出并选择退出码。解析成功后，仅用户命令行子命令附加父控制台，核心和界面均不附加。没有父控制台时（例如被双击后解析失败），错误写入日志文件。GUI 子系统进程附加控制台后，cmd 不会等它退出，输出可能和下一个提示符交错。

`quartz-host.exe` 使用独立的 console 子系统入口接收浏览器提供的标准句柄，不链接 GPUI。浏览器参数不用于入口路由；Native Messaging manifest 的 `allowed_origins`、`allowed_extensions` 限制可调用的扩展。用户命令行入口仍留在 `quartz.exe`。

## 3. 单实例

- 同一用户只允许一个采集会话。核心在初始化数据库与管道前，用 `LockFileEx` 独占数据目录的 `quartz.lock`，并在整个生命周期持有；锁文件跨 Windows 会话生效。
- `Local\Quartz.<SID>.core` 互斥量只用于会话内快速转发。同会话的普通重复启动通过管道发送 `ui.open` 后退出；`--autostart` 重复启动直接退出。另一会话启动时，核心以 `other_session` 错误码启动 UI 错误窗口并返回失败退出码。
- 首个管道实例使用 `FILE_FLAG_FIRST_PIPE_INSTANCE`，防止其他进程抢先创建同名管道。
- 界面进程用互斥量 `Local\Quartz.<SID>.ui` 保证唯一。重复启动时，通过核心转发"激活窗口"请求后退出。

## 4. 界面生命周期

### 4.1 打开

1. 用户左键点击托盘，或选择菜单里的"打开"，触发打开。
2. 核心维护 UI 状态 `none | starting | prewarming | visible` 和 `show_requested`。打开请求置 `show_requested = true`；已可见时发 `ui.activate`，已预热且完成握手时发 `ui.show`，启动中则等待握手。
3. 状态为 `none` 时，核心用 `CreateProcessW` 以 `current_exe()` 的绝对路径和 `ui` 子命令启动界面，不经过 shell。
   - 设置 `STARTF_FORCEONFEEDBACK`，让鼠标显示"后台运行"指针。
   - 托盘提示改为"正在打开…"。
4. 核心保存子进程句柄并设为 `starting`，防止连续点击重复启动。界面在 `hello` 中登记 `starting`、`prewarming` 或 `visible`；核心在握手完成后重放尚未完成的显示请求。收到 `ui.first_frame` 后设为 `visible` 并清除 `show_requested`。
5. 核心重启后，已有 UI 重连握手即重新登记；核心检测到会话内 UI 互斥量已被持有时，等待已有 UI 重连登记，再处理待完成的打开请求。

### 4.2 悬停预热

鼠标进入托盘图标（`NIN_POPUPOPEN` 或首个 `WM_MOUSEMOVE`）时，核心以 `ui --prewarm` 启动界面进程，规则如下：

- 预热进程会完成 GPUI 初始化和 D3D 设备创建，但不创建窗口。
- `PREWARM_TTL`（默认 8 秒）从预热进程握手完成时起算。到期前未被打开才退出；显式打开请求优先于 TTL，核心的 `show_requested` 在首帧确认前一直保留。
- 用户点击后，核心登记 `show_requested`；预热进程已握手就发送 `ui.show`，否则握手完成后重放。
- 两次预热之间至少间隔 `PREWARM_COOLDOWN`（默认 60 秒），避免鼠标频繁扫过托盘时反复唤醒 dGPU。
- 设置中提供开关，默认开启，电池供电时关闭；M3 实测后复核此默认值。

GPUI 在 `Application::run` 内部、启动回调之前创建 D3D 设备（`connect_initially` → `DirectXDevices::new`），所以"进入 run 但不开窗"就能完成预热中最耗时的部分。这一点在 M3 实测确认。

### 4.3 关闭

- 最后一个窗口关闭时，界面进程调用 `cx.quit()`，整个进程退出，GPU 驱动占用随之释放。
- 核心确认 UI 进程退出后设为 `none`；仅管道断开时保留进程登记并等待重连。
- 用户在托盘选择"退出 Quartz"时，核心先通知界面关闭，再结算当前区间、关闭数据库，最后退出。

### 4.4 核心不在时的界面

界面连接不上管道时，显示"Quartz 核心未运行"，并提供"启动核心"按钮（以无参数方式启动 `quartz.exe`）。界面不自行读取数据库。

## 5. crate 划分

```text
crates/
├─ quartz          exe 入口；核心装配、平台层、存储、查询、托盘
├─ quartz-host     独立 Native Messaging 主机；浏览器 stdio 与核心管道转发，不链接 GPUI
├─ quartz-domain   纯逻辑：计时状态机、区间运算、可用状态裁剪、规则匹配、提取、隐私策略求值
├─ quartz-protocol 消息类型、分帧编解码、版本与错误码
├─ quartz-i18n     翻译资源、语言协商、复数、时长与日期格式化（10）
└─ quartz-ui       GPUI 界面，只依赖 quartz-protocol 与 quartz-i18n

以后：quartz-protocols  自控协议的纯逻辑（12 第 4 节），依赖规则同 quartz-domain
```

依赖规则：

- `quartz-domain` 不依赖 Windows API、SQLite 和 GPUI，可以在任意平台跑单元测试。时间值由外壳读取后作为纯核心的输入传入。
- `quartz-ui` 不依赖 `quartz-domain`，展示所需的一切都来自协议消息；文字翻译与格式化来自 `quartz-i18n`。
- `quartz-i18n` 不依赖 GPUI、Windows API 与 SQLite，核心和界面共用。界面里要显示"命中规则"或"预览命中"，都向核心请求。
- `quartz` 不呈现窗口、对话框或错误文字，只保留 Shell 托盘；启动错误以 `quartz-protocol::CoreErrorCode` 和结构化参数交给 `quartz-ui` 本地化并展示，`quartz-i18n` 的文字查找仅用于托盘菜单和提示。托盘通过 `TrayState` 输入、`TrayCommand` 输出隔离，原生窗口、菜单和 Shell 调用集中在 `platform/windows/tray/`；以后可像 `quartz-host` 一样将该模块移到独立可执行文件。
- `quartz` 对 `quartz-ui` 的依赖只出现在入口里一个函数调用：`quartz_ui::run(args)`。日后要拆成独立的 `quartz-ui.exe`，只需把这一行移到新的 bin crate。
- 平台代码放在 `quartz/src/platform/windows/`，对外只暴露领域事件，不暴露 HWND、HANDLE 或 COM 类型。macOS 实现以后放在同级目录。
- **函数式核心 + 命令式外壳（sans-IO）。** 有状态的业务组件（规则注册表、采集状态机、截止时间调度器）是不做 I/O 的纯核心：输入当前状态与一条命令或事件，输出决定（要执行的写入描述、回复、待发布的结果），不调用数据库、文件、系统 API 或线程原语。外壳（各线程的循环，组合根 `core::app` 负责装配）持有 `store`、平台句柄与通道，执行核心给出的写入，成功后再把结果交回核心。数据库只是外壳的依赖，业务核心不定义也不依赖仓储接口。核心放在 `quartz-domain`，用值做单元测试。
- **适用边界。** 状态能放进内存、规模有限的组件采用上述模式。需要按需读取大量数据的部分（查询管线，03 第 5 节）属于外壳，直接在数据库线程上用只读连接执行。`store` 是唯一包含 diesel 与 SQL 的模块，只做持久化与行、领域类型转换，不含业务判断。

`quartz` crate 内部模块：

| 模块 | 职责 |
|---|---|
| `main` | clap 参数解析与模式分发 |
| `core::app` | 核心装配、线程启动、关闭顺序；`Autostart` 控制重复启动时是否转发打开界面的请求 |
| `core::foreground` | 前台事件过滤、元数据选择与解析接口 |
| `core::sources` | 系统采样的时间转换、会话重试与空闲状态处理 |
| `platform::windows::{foreground, sources, tray, process}` | Win32 前台解析、系统观测源、托盘与进程信息；`sources` 读取空闲、会话与电源状态 |
| `platform::windows::ipc` | 命名管道创建、对端身份验证与连接生命周期；传递 `ipc::Services` |
| `persistence` | 数据库线程的写入队列、批量事务、失败缓冲与采集反馈 |
| `store` | schema 迁移、写入、读取、保留清理；`Store::write` 统一写事务，提交后合并版本变更，`store::versions` 管理版本行、UTC 日期范围和配置键 |
| `rules::registry` | 规则注册表：规则状态的唯一写者，发布不可变规则快照（第 6.2 节） |
| `query` | 查询调度、纯切片与聚合、版本摘要与分页；`query::runner` 持有分批查询任务并归还只读连接 |
| `ipc` | 会话动作、角色权限、订阅和协议客户端；`ipc::Services` 汇集应用服务，`ipc::connection` 在异步读写流上执行会话动作 |

`query` 依赖 `store` 读取事实和版本值，`store` 通过 `quartz-domain` 的事实类型返回数据。`ipc` 依赖 `query` 的查询调度与版本发布接口。`core::app` 将查询端口、版本端口、状态接收端和界面命令端口组装为 `ipc::Services`。`platform::windows::ipc` 创建和验证命名管道，只通过 `ipc` 接口传递服务集合、启动订阅注册表并调用 `ipc::connection`。`core::app::CoreRuntime` 打开存储、取得安装标识、构造运行元数据与状态发布器，并启动采集线程、数据库线程和管道服务。`PlatformPorts` 提供时钟、主线程唤醒与界面命令通知、会话标识、安装标识候选值、核心实例标识、前台解析器工厂和管道工厂，以及语言与区域设置。前台解析器工厂在采集线程调用；Windows 实现持有该线程的 COM apartment，并在解析器释放后结束 COM。平台主线程持有托盘、系统观测源和消息循环，通过运行时取得观测入口、关闭端口和命令接收端。日志初始化由运行时提供，数据目录定位和实例锁仍使用 Windows 环境与系统接口。

用户退出时，`CoreRuntime` 依次发送 `ui.shutdown`、限时发送待处理的管道消息并停止管道、请求采集结算、等待采集与数据库线程退出。启动失败和运行时释放也使用同一条关闭路径。`WM_ENDSESSION` 先通过 `ShutdownPort` 在回调内等待最终持久化，消息循环结束后由运行时停止管道并回收已完成的线程。关闭时读取时钟失败会保留错误，并使用采集线程最后一次观测时间结算。

## 6. 线程模型

| 线程 | 内容 | 阻塞规则 |
|---|---|---|
| 主线程 | 消息循环；WinEvent 回调；托盘；消息窗口接收会话和电源通知，隐藏顶层窗口接收时间变化和结束会话广播；空闲检查定时器（`SetTimer`） | 所有 hook 注册、重绑、解绑均在主线程；触发与低频复核共用当前桌面、`GetForegroundWindow()` 和标题的读取比较路径，仅将变化观测按读取顺序入队（02 第 1 节），不做磁盘 I/O |
| 采集线程 | 进程身份查询、处理标题快照、隐私策略、状态机；发送目标 HWND/PID 的 hook 重绑命令，生成写入命令 | 可以调用 `OpenProcess` 等慢 API；不访问数据库；主线程执行重绑前核对目标仍为当前前台 |
| 数据库线程 | 持有写连接和只读连接；写连接批量写入，只读连接持有查询的 WAL 快照 | 查询分批计算，批间让出给写连接处理写入；同一响应使用同一读事务 |
| I/O 线程 | tokio current-thread；管道 accept、读写、订阅推送 | 不执行 SQLite；查询请求转给数据库线程，用 oneshot 取回结果 |

查询任务持有只读事务，按 `QUERY_BATCH_ROWS`（256）分批读取事实、扫描边界和聚合片段；每批之间先处理待写命令，并在写入截止时间前预留 `COMMIT_HEADROOM`（100 毫秒）执行提交。I/O 线程以非阻塞方式投递到容量为 8 的查询通道，异步等待 oneshot 回复；查询结束或取消后，`query::runner` 归还只读连接。`DatabaseLoop` 直接持有 `Store`，规则外壳按调用借用存储，规则事务的时间由数据库线程的时钟提供。

I/O 与托盘写入通过独立的有界 `WritePort`（`WRITE_CHANNEL_CAPACITY`，8）投递，队列满时返回 `busy`，数据库线程先提交全部待落盘的采集批次，再处理一条写命令，成功提交后发布版本变化。采集提交截止时间和关闭请求优先于写命令，写命令的 SQLite 锁等待限制在 `COMMIT_HEADROOM` 内，执行后返回循环重新检查采集队列。

I/O 线程内用普通 tokio task 和有界 `mpsc` 实现一个连接注册表 actor，以及每个连接独立的 outbox actor。数据库线程的 `VersionPublisher` 保存 UTC 日期版本并计算已订阅逻辑日期的摘要，通过 watch 发布 `{date, versions}`；注册表只保留已订阅日期的摘要。注册表持有连接角色、订阅和发送句柄，扇出只调用非阻塞投递；outbox 负责发送预算、可靠消息顺序和最新状态槽。主线程通过 watch 发布已清洗状态，通过有界邮箱非阻塞投递控制消息；客户端写入只由 outbox 等待，详细规则见 04 第 7 节。

线程之间用有界通道连接：

```text
主线程 ──ObservedEvent──▶ 采集线程 ──WriteCommand──▶ 数据库线程 ◀──Query── I/O 线程
   ▲                          │                              │
   └──── 托盘与 UI 控制命令 ◀──┴─────── StateChanged ─────────┴──▶ I/O 线程（推送）
```

主线程到采集线程的队列容量为 1,024。满时主线程不阻塞，置过载标志并丢弃新事件。采集线程发现标志后，把当前区间结束于最后可信时间，写 `capture_gaps`，清空不再可信的排队事件；恢复后重读前台，缺口到重读成功时结束。

采集线程到数据库线程共用一个按写入命令数计量的 4,096 容量预算；排队批次、失败缓冲和正在提交的事务前缀都计入这个预算。数据库线程提交前固定事务前缀，执行 SQLite 时释放队列锁；采集线程继续合并或丢弃尚未提交的后缀，控制通道仅发送合并后的唤醒。满时先合并可替换的确认点命令；仍无法入队则按同一过载流程记录未知范围。数据库写入失败时托盘显示异常；写入队列作为失败缓冲，最多保留 4,096 条命令或 10 分钟，以先到者为准，超限范围转为 `capture_gaps`。数据库恢复后优先落盘缺口，再恢复采集。缺口不包含标题或应用身份。

会话与电源通知通过显式注册交给 message-only 窗口；`WM_TIMECHANGE`、`WM_QUERYENDSESSION` 和 `WM_ENDSESSION` 由托盘已有的隐藏顶层窗口接收。两个窗口均不设置 `WS_VISIBLE`。跨线程通知使用类型化 `AppMessage` 唤醒主线程。

### 6.1 截止时间调度器

暂停、隐私模式、保持计时、空闲例外等限时状态，以及以后的协议计时（12 第 7 节），都注册到核心的一个截止时间调度器：

- 调度器在主线程，只为最近的一个截止时间设置一次性 `SetTimer`；没有截止时间时不设定时器，不产生周期唤醒。
- 暂停、隐私模式、保持计时、应用豁免和声音例外的状态与 UTC 截止时间保存在 `timed_policies`，注册、取消或到期与对应采集边界在同一事务完成；这些策略不在 `settings` 重复保存截止时间。其他需要持久化的调度截止时间保存在 `settings`。
- 核心启动时读取全部截止时间：已过期的立即按到期处理，其余重新调度。
- 到期回调投递给拥有该状态的模块（采集线程或以后的 selfreg 模块），由它产生 `policy_changed` 等边界。
- 系统时间跳变（02 第 5 节）后重新计算最近截止时间。

### 6.2 规则注册表的并发模型

规则会被采集线程（隐私求值）和数据库线程（分类、提取、查询）同时读取，又会被界面、命令行导入和内置升级修改。并发由三个明确的模式分工处理：

| 模式 | 解决的问题 | 做法 |
|---|---|---|
| Actor（单写者） | 修改之间的竞争 | 规则注册表由数据库线程上的 Actor 循环驱动，是规则状态的唯一写者。所有修改都是 `RegistryCommand`（`Apply`、`Toggle`、`Reorder`、`Delete`、`Revert`、`Import`、`InstallBuiltin`），附回复通道，逐条串行处理，修改可线性化。注册表本身是 sans-IO 纯核心（第 5 节），见下文“处理一条命令” |
| RCU（读-复制-更新） | 读写之间的竞争 | 当前生效规则是不可变的 `RuleSnapshot { epoch, privacy, classification, extraction, hashes }`，以 `Arc` 共享，放在基于 `arc-swap` 的 `SnapshotCell` 中。读者用一次原子加载取得快照，不加锁、不阻塞；写者构造并编译完整的新快照后原子替换，旧快照在最后一个读者释放后回收 |
| 乐观并发控制（OCC） | 基于旧版本的修改覆盖新版本 | 每个修改命令携带它所基于的 `base_version`。注册表与当前版本比较，不一致时返回错误码 `rule_set_stale`，客户端重新加载并显示三方对比；单写者保证“比较并写入”是原子的 |

**读者的快照隔离。** 一个工作单元只使用一个快照：采集线程处理一个事件、数据库线程计算一次查询响应（含全部分页）时，开始时取一次快照，全程使用它，不会混用新旧规则。查询游标绑定的规则输入版本即快照中的哈希，与第 4 节的 `stale_cursor` 机制一致。

**变更通知。** 替换快照后，注册表向采集线程的事件队列推送 `RulesChanged { epoch }`。采集线程记录当前区间求值时所用快照的 epoch；收到通知或在处理事件时发现更大的 epoch，就产生 06 第 4.1 节的 `policy_changed` 边界。通知只用于及时唤醒，正确性由 epoch 比较保证，通知延迟或合并都不会漏掉变更。

**固定顺序与不变量：**

**处理一条命令（sans-IO）。** 注册表核心 `RuleRegistry` 在 `quartz-domain` 中，内存里保存各规则集的当前版本与内容（数百条规则，启动时由外壳从数据库载入）。外壳的循环对每条命令执行：

```text
RegistryCommand ─▶ RuleRegistry::decide(&self, cmd) ─▶ Decision { writes: Vec<RuleSetWrite>, reply }
                         （纯计算：OCC 检查、校验与编译）        │
                                                              ▼
              外壳在一个事务中用 store 执行 writes；失败则只回复错误，不进入下一步
                                                              │ 提交成功
                                                              ▼
              RuleRegistry::committed(outcome) ─▶ 新快照、epoch+1、原子替换、发通知
```

`RuleSetWrite` 是领域层的写入描述枚举（提交新版本、启停、排序、标记删除、安装内置修订），只携带领域类型。核心从不调用数据库；外壳不做业务判断。

1. `decide` 校验并编译新版本（正则限制见 03 第 5.5 节），并按 `base_version` 做 OCC 检查。编译失败或版本过期时不产生写入，只返回错误回复。
2. 外壳在一个事务中执行全部写入（`rule_set_versions` 与 `rule_sets`），提交。
3. 提交成功后外壳调用 `committed`，核心更新内存状态，构建新快照，epoch 加一，原子替换，再发通知。提交失败时不调用 `committed`，内存状态与数据库不会分歧。
4. 提交成功但替换前崩溃时，核心启动时从数据库重建快照；数据库是唯一事实来源，快照只是它的编译视图。
5. epoch 只在进程内单调递增，用于判断发布先后；跨进程与缓存使用内容哈希（03 第 2.7 节），两者职责不同。
6. 启动时若某个已存版本编译失败（例如限制调整后），只停用该规则集并在规则集树上告警，其余规则照常生效。

## 7. 依赖选型

用户指定的基础 crate：`windows`、`snafu`、`tokio`、`clap`、`tracing`。其余按下表选择。

| 用途 | 选择 | 说明 |
|---|---|---|
| Win32 | `windows 0.62`，只开需要的 feature | 不混用 `windows-sys`；平台层集中在 `platform::windows` |
| 命令行 | `clap 4`（`derive`，关闭 `color`、`suggestions` 等非必要 feature） | 用法见第 2 节；浏览器启动参数在 clap 之前预检 |
| 错误 | `snafu` | 见下文“错误处理” |
| 异步 I/O | `tokio`，只开 `rt`、`net`、`io-util`、`io-std`、`sync`、`time`、`macros` | 只用 current-thread runtime，见下文“tokio 使用范围” |
| 日志 | `tracing` + 精简 `tracing-subscriber`（`fmt`、`env-filter`），自写按大小滚动的文件 writer | 日志里不写标题、域名等内容，见 06 |
| 界面 | `gpui-kit 0.10.0`（固定版本，及其依赖的 `gpui-pre`） | 只在 `quartz-ui` 中 |
| 托盘 | `tray-icon 0.26` | 复用核心的消息循环。如果无法拿到悬停事件或需要自绘 HMENU，就改为直接调用 `Shell_NotifyIconW` |
| 数据库 | `diesel 2.4.0`（`sqlite`、`returning_clauses_for_sqlite_3_35`）+ `diesel_migrations 2.4.0`；`libsqlite3-sys` 开 `bundled` | 只在 `store` 模块中，见 03 第 4.5、8 节。启动时检查 SQLite 版本不低于 3.51.3（WAL-reset 缺陷修复版本） |
| 序列化 | `serde` + `serde_json` | 协议与浏览器共用 JSON |
| 规则快照发布 | `arc-swap 1.9.5` | 只在 `rules::registry` 中，实现第 6.2 节的 RCU 快照单元 |
| 日期 | `jiff` | [Windows 系统时区识别与内嵌 IANA 历史时区数据](https://docs.rs/jiff/1.0.0/jiff/_documentation/platform/index.html#windows) |
| 正则 | `regex`，规则编译时设置 `size_limit` 和 `dfa_size_limit` | 防止用户正则耗尽内存 |
| 界面文字 | `rust-i18n`（与 gpui-component 相同） | 只在 `quartz-i18n` 中声明翻译资源，见 10 第 2 节 |
| 文本比较 | ICU4X `icu_normalizer`、`icu_casemap`（编译期数据） | `quartz-domain` 的规则匹配使用；M1 测量体积（09 H10） |
| 区域格式 | ICU4X `icu_plurals`、`icu_decimal`；界面另用 `icu_collator` | 复数、数字格式、按语言排序 |
| 域名 | `idna` | 用户输入的 Unicode 域名转 punycode |

### 7.1 错误处理

错误类型定义在执行该失败操作的模块中，上层只通过 `source` 包装下层错误，不重复定义下层的失败原因。

有明确接收者的操作应实现为拥有或借用该接收者的类型的方法，自由函数仅用于没有接收者的纯计算，关联函数用于构造对象。

- `anyhow` 仅在 `quartz-ui` 的 GPUI 错误边界作为上游错误的 `source` 类型出现，所有内部错误均使用 `snafu`。
- 按失败情形定义 snafu 错误类型，名称采用“动词 + 对象 + Error”；一个操作有多种失败原因时，用该操作命名的一个枚举为每种原因定义变体，并用 context selector 附加上下文（路径、调用名、规则 ID 等）。不定义整个 crate 共用的兜底错误枚举；跨平台端口与前台解析器工厂使用 `Box<dyn Error + Send + Sync>` 保留平台错误，操作错误仍由具体的 snafu 错误类型包装。裸名 `Error` 仅用于模块私有类型。
- Win32 调用的 `windows::core::Error` 作为 `source` 保留，上下文写调用名，例如 `OpenProcessSnafu { pid }`。
- 跨进程只传结构化错误码。`quartz-protocol` 定义错误码枚举，核心把内部 snafu 错误映射为错误码，界面和 host 只按错误码分支，不解析错误消息文本。
- 错误的 `Display` 与上下文字段同样遵守 06 的日志约束：不携带标题、域名、URL。需要定位时只带区间 ID、规则 ID 等标识。
- 入口 `main` 用一个顶层 snafu 错误汇总各模式的启动失败，写日志后按模式返回退出码。命令行模式额外打印到附加的控制台。

### 7.2 tokio 使用范围

tokio 只承担管道与 stdio 的异步 I/O，不接管 Win32 消息循环、SQLite 或 GPUI 的执行器。所有 runtime 都是 `Builder::new_current_thread()`，不使用 multi-thread runtime：它默认按 CPU 核数创建工作线程（测试机是 28 个逻辑核），会突破 08 的线程预算。

| 模式 | runtime 所在线程 | 负责 |
|---|---|---|
| 核心 | 独立的 I/O 线程 | 命名管道 accept、读写、订阅推送；与其他线程用 tokio `mpsc`、`oneshot` 通道通信。主线程与采集线程是同步代码，用 `blocking_send` 或 `try_send` 向 I/O 线程投递 |
| host | 主线程 | 浏览器 stdin/stdout 与命名管道之间的双向转发 |
| 界面 | 一个后台线程 | 管道客户端与重连；通过通道把消息交给 GPUI 的前台执行器，界面代码不直接 await tokio 的 future |

`max_blocking_threads` 设为小值（核心 2、host 1）。`tokio::io::stdin` 等会使用 blocking 线程池，这些线程计入 08 的线程预算。数据库访问不用 `spawn_blocking`，仍由数据库线程处理（第 6 节）。

release profile：`lto = "fat"`，`codegen-units = 1`，`panic = "abort"`，`strip = true`。`opt-level` 在 `"s"` 和 `3` 之间按实测决定。

## 8. 自启与更新

- 自启写入 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Quartz`，值为 `"<exe 绝对路径>" --autostart`。设置里可以开关。
- 首版手动更新：利用 Windows 允许重命名运行中 exe 的行为，把 `quartz.exe` 重命名为 `quartz.old.exe`，放入新 `quartz.exe`，然后退出并重启 Quartz；下次启动删除 `.old`。
- 程序目录和数据目录分开。数据目录是 `%LOCALAPPDATA%\Quartz\`，里面有 `quartz.db`、`logs\`、`icons\`、`backups\`。
- 数据库升级、备份、降级处理见 03 第 7–8 节：迁移前用 `VACUUM INTO` 备份，只保留最近 2 份迁移备份；数据库来自更新版本时，核心拒绝启动，不尝试写入。按隐私规则清理历史时提示并可一并删除备份（03 第 6 节）。
- 换回旧版本 exe 前，用 `quartz restore-backup <文件>` 恢复迁移前备份；备份之后记录的数据会丢失；不带 `--yes` 时仅打印替换范围并返回非零退出码，确认后加 `--yes` 执行。
