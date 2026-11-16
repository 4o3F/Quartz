# 01 总体架构

## 1. 进程模型

```text
quartz.exe（同一个文件）
├─ 核心模式（默认，常驻）
│  ├─ 主线程：Win32 消息循环、托盘、WinEvent hook、会话与电源通知
│  ├─ 采集线程：应用身份解析 → 隐私策略 → 计时状态机 → 写入队列
│  ├─ 数据库线程：diesel（SQLite）单写者，查询也在这里执行
│  └─ I/O 线程：tokio current-thread，命名管道服务
│
├─ 界面模式（`ui` 子命令，按需启动）
│  └─ GPUI 窗口：连接核心 → 查询与订阅 → 渲染；最后一个窗口关闭即退出进程
│
└─ host 模式（由浏览器启动）
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

入口先识别模式，再初始化日志、线程、数据库和平台设施。识别分两步。

**第一步：浏览器启动预检。** 浏览器拉起 host 时的参数格式由浏览器决定，不是 Quartz 的命令行语法。Chrome、Edge 还会追加 `--parent-window=N`，交给 clap 会被当作未知参数报错。所以先用固定模式匹配原始参数（`std::env::args_os`）：

| 条件 | 模式 |
|---|---|
| 第一个参数以 `chrome-extension://` 开头 | host（Chrome、Edge 传扩展 origin，后面可能跟 `--parent-window=N`） |
| 恰好两个参数，第一个是以 `.json` 结尾的绝对路径，第二个是已登记的 Firefox 扩展 ID | host（Firefox 传 manifest 路径和扩展 ID） |

Native Messaging manifest 的 `path` 字段不能带参数，所以只能这样识别，详见 [05-browser.md](05-browser.md)。

**第二步：clap 解析。** 预检不命中时，用 `clap`（derive）解析 Quartz 自己的命令行：

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
        #[arg(long)]
        prewarm: bool,
    },
    /// 显式 host 入口，测试用
    #[command(hide = true)]
    NativeHost,
    /// 打印某天的区间（经管道以 cli 角色查询核心；M1 可直接只读打开数据库）
    Dump { #[arg(long)] date: String },
    /// 核心状态
    Status,
    /// 向独立模拟库写入一年模拟数据，不触碰生产数据库
    GenData { #[arg(long)] db: PathBuf },
    /// 只读模拟库的查询基准
    BenchQuery { #[arg(long)] db: PathBuf },
    /// 用迁移前备份替换数据库；核心必须已退出
    RestoreBackup { file: PathBuf },
}
```

| 解析结果 | 模式 |
|---|---|
| 无子命令 | 核心 |
| `ui [--prewarm]` | 界面 |
| `native-host` | host |
| `dump`、`status`、`gen-data`、`bench-query`、`restore-backup` | 命令行 |
| 解析失败 | 报错退出，不进入核心 |

解析失败时退出码为 2，不启动核心。这样，浏览器或其他调用方传入未预料的参数时，不会误启动第二个核心。`ui` 和 `native-host` 在帮助中隐藏，它们是进程间约定，不是用户命令。

**为什么界面需要显式子命令。** 界面和核心是同一个 exe，但启动后的行为完全不同。核心拉起界面时必须明确告诉子进程“你是界面”：

1. 不能靠“核心是否已在运行”推断。核心重启、更新或崩溃恢复期间，数据目录锁会短暂空出；按推断启动的界面子进程会拿到锁，变成第二个核心，开始采集。显式子命令让模式只取决于调用方意图，与时序无关。
2. 预热进程（`ui --prewarm`）不开窗口，它的行为本来就需要用参数区分。
3. 用户双击 exe 或从开始菜单启动时不带参数，走核心路径：核心已在运行时，把“打开界面”转发给核心后退出（第 3 节），由核心按第 4 节拉起界面。所以界面进程始终由核心创建，UI 状态机（`starting`、`prewarming`、`visible`）只有核心这一个所有者，不会和用户直接启动的界面实例竞争。

**控制台输出。** exe 使用 `windows` 子系统，启动时没有控制台。命令行模式在解析前调用 `AttachConsole(ATTACH_PARENT_PROCESS)`，clap 的帮助与错误输出写到调用者的控制台。没有父控制台时（例如被双击后解析失败），错误写入日志文件。GUI 子系统进程附加控制台后，cmd 不会等它退出，输出可能和下一个提示符交错。M0 评估这个体验；不可接受时，与 host 共用一个 console 子系统的小 exe（见下）。

host 模式要验证 GUI 子系统进程能否继承浏览器给的标准句柄并按二进制读写。验证失败时改用独立的 console 子系统 `quartz-host.exe`，命令行模式也可迁到这个 exe。它很小，不链接 GPUI。

## 3. 单实例

- 同一用户只允许一个采集会话。核心在初始化数据库与管道前，用 `LockFileEx` 独占数据目录的 `quartz.lock`，并在整个生命周期持有；锁文件跨 Windows 会话生效。
- `Local\Quartz.<SID>.core` 互斥量只用于会话内快速转发。同会话重复启动时，把参数（例如"打开界面"）通过管道转发后退出。另一会话启动时，提示"Quartz 已在另一个 Windows 会话中运行"并退出。
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
├─ quartz          exe 入口；核心装配、平台层、存储、查询、托盘、host
├─ quartz-domain   纯逻辑：计时状态机、区间运算、可用状态裁剪、规则匹配、提取、隐私策略求值
├─ quartz-protocol 消息类型、分帧编解码、版本与错误码
├─ quartz-i18n     翻译资源、语言协商、复数、时长与日期格式化（10）
└─ quartz-ui       GPUI 界面，只依赖 quartz-protocol 与 quartz-i18n
```

依赖规则：

- `quartz-domain` 不依赖 Windows API、SQLite 和 GPUI，可以在任意平台跑单元测试。时间来源通过 `Clock` trait 注入。
- `quartz-ui` 不依赖 `quartz-domain`，展示所需的一切都来自协议消息；文字翻译与格式化来自 `quartz-i18n`。
- `quartz-i18n` 不依赖 GPUI、Windows API 与 SQLite，核心和界面共用。界面里要显示"命中规则"或"预览命中"，都向核心请求。
- `quartz` 对 `quartz-ui` 的依赖只出现在入口里一个函数调用：`quartz_ui::run(args)`。日后要拆成独立的 `quartz-ui.exe`，只需把这一行移到新的 bin crate。
- 平台代码放在 `quartz/src/platform/windows/`，对外只暴露领域事件，不暴露 HWND、HANDLE 或 COM 类型。macOS 实现以后放在同级目录。

`quartz` crate 内部模块：

| 模块 | 职责 |
|---|---|
| `entry` | 浏览器启动预检、clap 参数解析与模式分发 |
| `core::app` | 核心装配、线程启动、关闭顺序 |
| `platform::windows::{foreground, idle, session, power, tray, process, autostart}` | Win32 采集与系统集成 |
| `capture` | 观测事件 → 身份解析 → 隐私策略 → 状态机 → 写入命令 |
| `store` | schema 迁移、写入、读取、保留清理 |
| `query` | 查询管线与缓存 |
| `ipc` | 管道服务、会话、角色权限、订阅 |
| `host` | Native Messaging 转发 |

## 6. 线程模型

| 线程 | 内容 | 阻塞规则 |
|---|---|---|
| 主线程 | 消息循环；WinEvent 回调；托盘；隐藏顶层窗口接收会话和电源消息；空闲检查定时器（`SetTimer`） | 所有 hook 注册、重绑、解绑均在主线程；回调立即读取标题快照和时间并入队，不做磁盘 I/O |
| 采集线程 | 进程身份查询、处理标题快照、隐私策略、状态机；发送带前台代次的 hook 重绑命令，生成写入命令 | 可以调用 `OpenProcess` 等慢 API；不访问数据库 |
| 数据库线程 | 持有写连接和只读连接；写连接批量写入，只读连接持有查询的 WAL 快照 | 查询分批计算，批间让出给写连接处理写入；同一响应使用同一读事务 |
| I/O 线程 | tokio current-thread；管道 accept、读写、订阅推送 | 不执行 SQLite；查询请求转给数据库线程，用 oneshot 取回结果 |

线程之间用有界通道连接：

```text
主线程 ──ObservedEvent──▶ 采集线程 ──WriteCommand──▶ 数据库线程 ◀──Query── I/O 线程
   ▲                          │                              │
   └──── 托盘与 UI 控制命令 ◀──┴─────── StateChanged ─────────┴──▶ I/O 线程（推送）
```

主线程到采集线程的队列容量为 1,024。满时主线程不阻塞，置过载标志并丢弃新事件。采集线程发现标志后，把当前区间结束于最后可信时间，写 `capture_gaps`，清空不再可信的排队事件；恢复后重读前台，缺口到重读成功时结束。

采集线程到数据库线程的队列容量为 4,096。满时先合并可替换的确认点命令；仍无法入队则按同一过载流程记录未知范围。数据库写入失败时托盘显示异常；写入队列作为失败缓冲，最多保留 4,096 条命令或 10 分钟，以先到者为准，超限范围转为 `capture_gaps`。数据库恢复后优先落盘缺口，再恢复采集。缺口不包含标题或应用身份。

## 7. 依赖选型

用户指定的基础 crate：`windows`、`snafu`、`tokio`、`clap`、`tracing`。其余按下表选择。

| 用途 | 选择 | 说明 |
|---|---|---|
| Win32 | `windows 0.62`，只开需要的 feature | 不混用 `windows-sys`；平台层集中在 `platform::windows` |
| 命令行 | `clap 4`（`derive`，关闭 `color`、`suggestions` 等非必要 feature） | 用法见第 2 节；浏览器启动参数在 clap 之前预检 |
| 错误 | `snafu` | 见下文“错误处理” |
| 异步 I/O | `tokio`，只开 `rt`、`net`、`io-util`、`io-std`、`sync`、`time`、`macros` | 只用 current-thread runtime，见下文“tokio 使用范围” |
| 日志 | `tracing` + 精简 `tracing-subscriber`（`fmt`、`env-filter`），自写按大小滚动的文件 writer | 日志里不写标题、域名等内容，见 06 |
| 界面 | `gpui-kit 0.9.1`（固定版本，及其依赖的 `gpui-pre`） | 只在 `quartz-ui` 中 |
| 托盘 | `tray-icon 0.26` | 复用核心的消息循环。如果无法拿到悬停事件或需要自绘 HMENU，就改为直接调用 `Shell_NotifyIconW` |
| 数据库 | `diesel 2.3`（`sqlite`、`returning_clauses_for_sqlite_3_35`）+ `diesel_migrations`；`libsqlite3-sys` 开 `bundled` | 只在 `store` 模块中，见 03 第 4.5、8 节。启动时检查 SQLite 版本不低于 3.51.3（WAL-reset 缺陷修复版本） |
| 序列化 | `serde` + `serde_json` | 协议与浏览器共用 JSON |
| 日期 | `jiff` 或 `chrono` + `chrono-tz` | M1 时比较体积后选定；需要历史时区规则 |
| 正则 | `regex`，规则编译时设置 `size_limit` 和 `dfa_size_limit` | 防止用户正则耗尽内存 |
| 界面文字 | `rust-i18n`（与 gpui-component 相同） | 只在 `quartz-i18n` 中声明翻译资源，见 10 第 2 节 |
| 文本比较 | ICU4X `icu_normalizer`、`icu_casemap`（编译期数据） | `quartz-domain` 的规则匹配使用；M1 测量体积（09 H10） |
| 区域格式 | ICU4X `icu_plurals`、`icu_decimal`；界面另用 `icu_collator` | 复数、数字格式、按语言排序 |
| 域名 | `idna` | 用户输入的 Unicode 域名转 punycode |

### 7.1 错误处理

- 每个 crate 和 `quartz` 的每个模块定义自己的 snafu 错误枚举，用 context selector 附加上下文（路径、操作名、规则 ID 等），不用字符串拼接错误。
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
- 换回旧版本 exe 前，用 `quartz restore-backup <文件>` 恢复迁移前备份；备份之后记录的数据会丢失，命令执行前会提示。
