# 资源测量

## 2026-11-16 M0-B2：无系统标题栏的空白 GPUI 窗口与独立 UI 进程

构建基于 `55dfa4f1fa218cad5e97787394900e0d7b659163`（HEAD）加 `uncommitted M0-B2 working tree`。使用 Rust 1.100.0、`gpui-kit = "=0.10.0"`，关闭默认功能，启用 `component` 与 `assets`，通过 `icon_assets!` 仅嵌入最小化、最大化、还原和关闭四个图标；使用仓库的 release 配置和静态 C 运行库。没有引入 spike 的 WARP 补丁。

UI 使用 gpui-component `TitleBar` 与 `TitleBar::window_options()`，通过 `appears_transparent` 隐藏系统标题栏。960 × 640 逻辑像素的窗口居中打开，32 px 自绘标题栏与内容区共用 `bg.base` 背景。标题采用本地化应用名称、`type.caption` 的 12/16 px Regular 与 Segoe UI Variable Small，左侧间距为 `space.m`（12 px）；令牌和亮暗主题位于嵌入的 `crates/quartz-ui/themes/fluent.json`。标题使用组件 `Label`，布局方向按 Quartz 已协商的语言设置；启动与系统外观变更时同步主题。拖动区域与窗口按钮复用 GPUI 原生命中区域，最后一个窗口关闭后调用 `cx.quit()`。M0 的 `ui --prewarm` 成功退出且不开窗；D3D 预热、握手与生存时间控制保留为 `TODO(M3)`。

### 测量方法

在 Windows 桌面会话中执行以下命令。核心稳定 30 秒后记录 CPU 起点，空闲 600 秒后采样；UI 稳定 30 秒后采集 30 秒空闲 CPU。随后通过向 `Quartz.Core` 隐藏窗口投递 `AppMessage::TrayCallback`（`WM_APP + 1`），在 `lParam` 低 16 位传入 `NIN_SELECT`，打开并用 `WM_CLOSE` 关闭 UI 共 20 次。每次等待 UI 退出并确认没有剩余 UI 子进程，最后再稳定 30 秒并测量 30 秒核心空闲 CPU。表格同时记录初始空闲、首次 UI 关闭后及每 5 次循环的核心指标；20 次循环的增量以首次 UI 关闭后为起点，首次 UI 带来的增量单独列出。

本节结果由当时的 `measure.ps1` 测得。当前使用 Deno 2.9.8，按相同参数执行：

```powershell
deno task measure --Exe ./target/release/quartz.exe --SettleSeconds 30 --IdleSeconds 600
```

Private Bytes 与 Private Working Set 使用 `GetProcessMemoryInfo` 的 `PROCESS_MEMORY_COUNTERS_EX2`；线程、句柄和 CPU 使用 `Get-Process`。机器信息使用 WMI，在资源采样前读取。脚本不调用 WMI 性能计数器或 `EmptyWorkingSet`。`PROCESS_MEMORY_COUNTERS_EX2` 要求 Windows 10/11 安装 2023 年 9 月或之后的相应累积更新，参见 [Microsoft 文档](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex2)。

### 测量结果

测量结束时间：2026-11-16 21:42:59 -0800（America/Los_Angeles）；Windows 本地时间为 2026-11-17 13:42:59 +0800。机器信息来自本次 WMI 查询：Intel(R) Core(TM) i7-14700HX; GPU: GameViewer Virtual Display Adapter, NVIDIA GeForce RTX 4060 Laptop GPU; OS: Microsoft Windows 11 家庭版, build 26300。运行于普通桌面会话，使用 gpui-kit 0.10.0 的无系统标题栏自绘窗口。

| Scenario | Private Bytes (MiB) | Private WS (MiB) | Threads | Handles | Total CPU (ms) | Idle CPU (ms) |
|---|---:|---:|---:|---:|---:|---:|
| core idle | 2.379 | 1.590 | 4 | 161 | 31.250 | 0.000 |
| UI open | 94.098 | 15.656 | 56 | 554 | 1843.750 | 781.250 |
| core before 20 cycles (first UI closed) | 2.320 | 1.559 | 2 | 163 | 93.750 | - |
| core after 5 cycles | 2.320 | 1.559 | 2 | 163 | 515.625 | - |
| core after 10 cycles | 2.320 | 1.559 | 2 | 163 | 937.500 | - |
| core after 15 cycles | 2.320 | 1.559 | 2 | 163 | 1328.125 | - |
| core after 20 cycles | 2.320 | 1.559 | 2 | 163 | 1765.625 | - |
| core after 20 cycles, settled | 2.320 | 1.559 | 2 | 163 | 1765.625 | 0.000 |

核心初始空闲 Private Bytes 为 2.379 MiB、Private WS 为 1.590 MiB，600 秒 CPU 增量为 0 ms，线程数为 4，均满足核心预算。首次 UI 关闭后，相对初始空闲，Private Bytes 减少 0.059 MiB、Private WS 减少 0.031 MiB、线程数减少 2、句柄数增加 2。此后 20 次循环的 Private Bytes、Private WS、线程数及句柄数增量均为 0。相对最初空闲，最终句柄数仍多 2 个，来源尚未定位。

20 次循环结束后剩余 UI 子进程为 0。包含首次测量窗口在内，全部 21 次 WM_CLOSE 关闭均在 1 秒内退出，最大耗时为 77.109 ms。

UI 打开时 Private Bytes 为 94.098 MiB，满足 100 MiB 目标。已知偏差（M3）：静止 30 秒 CPU 增量为 781.250 ms，超过原定 20 ms / 30 s 预算，按 `docs/design/09-milestones.md` 的 M3 交付项继续检查 GPUI Windows 帧调度 / `VSyncProvider` 路径。`docs/design/08-resources.md` 的预算保持原样。

核心在 UI 打开前和循环结束后均未加载 `nv*.dll`。两次模块集合相同，包含静态导入的 `d3d11.dll`、`dxgi.dll`、`dcomp.dll` 和 `dwrite.dll`，按 M0 约定记录为非阻塞观察项。UI 进程加载了 `nvwgf2umx.dll`、`nvgpucomp64.dll` 等 NVIDIA 驱动模块。

Release 文件体积：`quartz.exe` 为 12,868,608 字节（12.272 MiB），`quartz-host.exe` 为 203,776 字节（0.194 MiB）。

### 与 0.9.1 无边框基线对照

两次均使用 32 px 自绘标题栏、30 秒稳定时间、600 秒核心空闲采样、30 秒 UI 空闲采样及 20 次循环。0.9.1 数据采于 2026-11-16 21:05:16 -0800，0.10.0 数据为本节实测。

| 指标 | gpui-kit 0.9.1 | gpui-kit 0.10.0 |
|---|---:|---:|
| 核心空闲 Private Bytes（MiB） | 2.383 | 2.379 |
| 核心空闲 Private WS（MiB） | 1.598 | 1.590 |
| 核心空闲 CPU（ms / 600 s） | 0.000 | 0.000 |
| UI Private Bytes（MiB） | 93.727 | 94.098 |
| UI Private WS（MiB） | 15.668 | 15.656 |
| UI 空闲 CPU（ms / 30 s） | 406.250 | 781.250 |
| 20 次循环核心 Private Bytes 增量（MiB） | -0.004 | 0.000 |
| 20 次循环核心线程 / 句柄增量 | 0 / 0 | 0 / 0 |
| WM_CLOSE 最大退出耗时（ms） | 45.176 | 77.109 |
| 剩余 UI 子进程 | 0 | 0 |
| quartz.exe（字节） | 12,771,840 | 12,868,608 |
| quartz-host.exe（字节） | 203,776 | 203,776 |

UI Private Bytes 增加 0.371 MiB，空闲 CPU 增加 375.000 ms / 30 s，两个版本均未达到 UI 空闲 CPU 预算。`quartz.exe` 增加 96,768 字节；host 体积不变。

### 无边框窗口交互与截图

使用定向窗口消息验证了标题栏双击最大化与还原、最小化按钮、最大化与还原按钮，以及关闭按钮。关闭按钮触发进程以退出码 0 结束，耗时 59.072 ms。标题栏返回 `HTCAPTION`（2）；按钮分别返回 `HTMINBUTTON`（8）、`HTMAXBUTTON`（9）、`HTCLOSE`（20）；左、右、上、下缩放边分别返回 10、11、12、15。

GPUI 的最大化按钮返回 Windows 11 Snap 布局所需的 `HTMAXBUTTON`（9），本次已通过定向命中测试确认。

暗色截图使用 `PrintWindow(PW_RENDERFULLCONTENT)`，按 `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` 裁切，去除 DWM 不可见边框；已检查标题栏、文字和按钮。截图跟随当前系统暗色主题，没有修改系统主题设置；本次未采集亮色截图。

### 构建与验证

| 检查 | 结果 |
|---|---|
| `cargo fmt --all --check` | 通过 |
| Linux `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| Linux `cargo test --workspace` | 22 项通过 |
| Windows `cargo.exe clippy --workspace --all-targets -- -D warnings` | 通过 |
| Windows `cargo.exe test --workspace` | 31 项通过 |
| Windows `cargo.exe build --release --workspace` | 通过 |
| Windows 旧版 `measure.ps1`，稳定 30 秒、空闲 600 秒（当前入口为 `deno task measure`） | 运行完成，退出码 0；预算偏差见测量结果 |
| 核心与 UI 冒烟检查 | 核心存活；第二次启动退出码 0；直接 `quartz.exe ui` 打开 Quartz 窗口，WM_CLOSE 后 34.323 ms 退出且退出码为 0；`ui --prewarm` 不开窗，退出码 0 |
| `cargo deny check` | advisories、bans、licenses、sources 全部通过；重复版本仅有警告 |

`Cargo.lock` 的外部包数从 B1 的 97 增至 811，增加 714 个，统计整个锁文件。当前 Windows 工作区依赖树使用 408 个外部包，Linux 工作区依赖树使用 84 个，Linux 树中没有 GPUI。0.9.1 的 Windows 树为 407 个包，本次增加的可达依赖为 `sys-locale 0.3.2`；锁文件的外部包总数保持 811。计数按包名与版本去重；使用 `cargo tree --workspace --target <target> --prefix none --format '{p}'` 检查实际目标树。

### 依赖版本与源码核对

在本次升级中运行 `cargo update`，随后逐个执行 `cargo info --registry crates-io <crate>` 核对全部外部直接依赖。`required` 来自工作区清单，`resolved` 来自 `Cargo.lock`，`latest` 来自 registry 查询：

| crate | required | resolved | latest |
|---|---|---|---|
| `anyhow` | `1` | `1.0.104` | `1.0.104` |
| `clap` | `4.6.8` | `4.6.8` | `4.6.8` |
| `gpui-kit` | `=0.10.0` | `0.10.0` | `0.10.0` |
| `rust-i18n` | `4.2.4` | `4.2.4` | `4.2.4` |
| `serde` | `1` | `1.0.229` | `1.0.229` |
| `serde_json` | `1` | `1.0.154` | `1.0.154` |
| `snafu` | `0.9.2` | `0.9.2` | `0.9.2` |
| `tracing` | `0.1` | `0.1.44` | `0.1.44` |
| `tracing-subscriber` | `0.3.23` | `0.3.23` | `0.3.23` |
| `windows` | `0.62.2` | `0.62.2` | `0.62.2` |

更新了 `gpui-kit`、`gpui-base`、`gpui-component`、`gpui-component-macros`、`gpui-kit-assets` 五个包，从 0.9.1 升至 0.10.0。随后执行 `cargo update --verbose --dry-run`，可更新包数为 0。上游精确约束保留了 `gpui-pre` 系列 0.3.12（已有 0.3.13）和 `generic-array 0.14.7`（已有 0.14.9）。

比较 registry 中已发布的 0.9.1 与 0.10.0 源码：`TitleBar` 增加逻辑方向布局，并修正按钮按压、小幅鼠标移动与文本选择之间的处理；主题调整了 Windows 系统滚动条默认模式。初始化、运行、`open_window`、语言设置和图标资源宏沿用现有 API。Quartz 使用 `Label` 显示标题，并显式将布局方向绑定到已协商语言。

两版都精确依赖 `gpui-pre = 0.3.12`，`gpui-pre-windows` 也保持 0.3.12，锁文件中的 Windows 后端版本与校验和均未改变。其 `VSyncProvider` 仍持续等待垂直同步，并对窗口调用 `RedrawWindow(..., RDW_INVALIDATE)`。

`anyhow` 仅作为 `quartz-ui` 的 GPUI `open_window` 错误边界的 `source` 类型，Quartz 内部错误使用 `snafu`。`deny.toml` 保留已批准的全局 `0BSD`，本次没有调整依赖审计规则。

### 加载模块

核心首次空闲与循环后的模块集合一致：

`advapi32.dll`, `bcryptprimitives.dll`, `combase.dll`, `comctl32.dll`, `d3d11.dll`, `dcomp.dll`, `dwmapi.dll`, `dwrite.dll`, `dxgi.dll`, `GDI32.dll`, `gdi32full.dll`, `icu.DLL`, `icuuc.dll`, `imm32.dll`, `kernel.appcore.dll`, `KERNEL32.DLL`, `KERNELBASE.dll`, `Microsoft.Internal.WarpPal.dll`, `MSCTF.dll`, `msvcp_win.dll`, `msvcrt.dll`, `ntdll.dll`, `ole32.dll`, `oleaut32.dll`, `powrprof.dll`, `quartz.exe`, `RPCRT4.dll`, `sechost.dll`, `shcore.dll`, `shell32.dll`, `shlwapi.dll`, `ucrtbase.dll`, `uiautomationcore.dll`, `UMPDC.dll`, `user32.dll`, `userenv.dll`, `uxtheme.dll`, `win32u.dll`, `windows.storage.dll`, `winmm.dll`.

UI 打开时的模块：

`advapi32.dll`, `bcrypt.dll`, `bcryptprimitives.dll`, `cfgmgr32.dll`, `clbcatq.dll`, `combase.dll`, `comctl32.dll`, `CRYPT32.dll`, `cryptbase.dll`, `cryptnet.dll`, `CRYPTSP.dll`, `d3d11.dll`, `dataexchange.dll`, `dcomp.dll`, `devobj.dll`, `directmanipulation.dll`, `directxdatabasehelper.dll`, `drvstore.dll`, `dwmapi.dll`, `dwrite.dll`, `dxcore.dll`, `dxgi.dll`, `GDI32.dll`, `gdi32full.dll`, `icu.DLL`, `icuuc.dll`, `imagehlp.dll`, `imm32.dll`, `kernel.appcore.dll`, `KERNEL32.DLL`, `KERNELBASE.dll`, `Microsoft.Internal.WarpPal.dll`, `msasn1.dll`, `MSCTF.dll`, `msvcp_win.dll`, `msvcrt.dll`, `ntdll.dll`, `ntmarta.dll`, `nvgpucomp64.dll`, `nvldumdx.dll`, `NvMemMapStoragex.dll`, `nvppex.dll`, `nvspcap64.dll`, `nvwgf2umx.dll`, `ole32.dll`, `Oleacc.dll`, `oleaut32.dll`, `powrprof.dll`, `profapi.dll`, `quartz.exe`, `RPCRT4.dll`, `rsaenh.dll`, `sechost.dll`, `Setupapi.dll`, `shcore.dll`, `shell32.dll`, `shlwapi.dll`, `sxs.dll`, `textinputframework.dll`, `TextShaping.dll`, `twinapi.appcore.dll`, `ucrtbase.dll`, `uiautomationcore.dll`, `UMPDC.dll`, `user32.dll`, `userenv.dll`, `uxtheme.dll`, `VERSION.dll`, `win32u.dll`, `Windows.Storage.dll`, `Windows.UI.dll`, `Windows.UI.Immersive.dll`, `WindowsCodecs.dll`, `winmm.dll`, `wintrust.dll`, `wldp.dll`.

### 后续测量

本次记录核心空闲和 UI 开关场景。ETW 唤醒统计、8 小时运行、采集与数据库场景按各自里程碑补充。`quartz-host.exe` 当前为 M5 占位程序，启动后直接退出，尚无常驻资源测量。

## 2026-11-16 M1-A：文本比较与 H10 Windows release 体积

构建基于 `2cf93289e7168d27e5a194e4f8530ee967df74e9` 加本次未提交的 M1-A 修改。Windows x86_64，Rust 1.100.0；沿用仓库 release 配置：`opt-level = 3`、`lto = "fat"`、`codegen-units = 1`、`panic = "abort"`、`strip = true`，并启用静态 C 运行库。

### 实现与语义

`quartz-domain::text::NfcText` 用于区分大小写的相等、子串、前缀、后缀比较，以及正则输入；`FoldedKey` 按 NFKC → Unicode 默认完整大小写折叠 → NFKC 生成比较键。原始标题由调用方保留，规范化键单独缓存。两种键的比较方法只接受同类型的键，且不读取界面语言或系统区域设置。

`quartz-domain::domain_name::DomainName` 使用 idna 的非过渡 UTS #46 转换，保存小写 ASCII / Punycode，提供 Unicode 拼写和完整标签边界的域名后缀匹配。采用 URL 主机字符限制、浏览器的连字符处理与 DNS 长度验证；移除尾部根点，使 `example.com.` 与 `example.com` 使用同一比较键。无效输入返回 `NormalizeDomainError::InvalidDomain`，保留不含输入文本的 IDNA 原因。

Q1 已按用户决策选定 jiff，查询到的最新发布版为 1.0.0。已核对其 [Windows 平台文档](https://docs.rs/jiff/1.0.0/jiff/_documentation/platform/index.html#windows)：通过 Windows API 识别系统时区，默认在 Windows 内嵌 IANA 时区数据库。本步骤未添加时间库依赖。

### H10 测量方法

先执行 Windows `cargo.exe build --release -p quartz`，保存没有调用 quartz-domain 的基线。随后临时给 quartz 添加 quartz-domain 路径依赖，并在入口加入由环境变量控制的调用：输入来自运行时环境变量，经 `std::hint::black_box` 传给 NFC、折叠和各项比较 API，输出也传给 `black_box`，保证 release 优化保留实际代码和数据。先测 NFC 与完整折叠，再加入域名 ASCII / Unicode 转换与后缀比较。每次均使用相同的构建命令和发布配置。

文件大小按字节统计；PE 节表分别记录 `.text` 与 `.rdata` 的实际内容长度及文件对齐后的长度。另从 ICU4X 2.4.0 的生成数据源码中提取至少 16 字节的字节数组，在最终 PE 的 `.rdata` 中查找完整字节序列，合并重叠地址后统计覆盖量；它衡量实际保留的数据载荷，结构体描述符、其他常量与对齐计入只读节增量。

| 构建 | exe（字节） | 相对基线增量（字节） | `.text` 内容（字节） | `.rdata` 内容（字节） | `.rdata` 文件占用（字节） | ICU 数据数组载荷（字节） |
|---|---:|---:|---:|---:|---:|---:|
| 基线 | 12,868,608 | 0 | 10,236,320 | 2,288,206 | 2,288,640 | 0 |
| NFC + NFKC / 完整折叠 | 13,028,352 | 159,744 | 10,270,336 | 2,413,070 | 2,413,568 | 123,835 |
| 完整模块：再加入 IDN | 13,168,128 | 299,520 | 10,316,368 | 2,507,078 | 2,507,264 | 216,035 |

仅 NFC 与完整折叠使 exe 增加 159,744 字节（156 KiB），其中数据数组载荷为 123,835 字节，`.rdata` 内容增加 124,864 字节。完整模块使 exe 从 12.272 MiB 增至 12.558 MiB，增加 299,520 字节（292.5 KiB，2.33%）；其中数据数组载荷为 216,035 字节，`.rdata` 内容增加 218,872 字节。IDN 相对仅规范化与折叠再增加 139,776 字节，其中新增数据数组为 92,200 字节。

识别到的规范化与折叠数组分别为：case map 23,145 字节、NFC 5,308 字节、NFD 分解 35,028 字节、NFD 表 2,129 字节、NFKD 分解 52,308 字节、NFKD 表 5,917 字节。IDN 再保留 UTS #46 数据 57,368 字节、双向文本类别 9,832 字节、通用类别 17,908 字节、连接类型 7,092 字节。基线中没有匹配到这些数组。它们位于只读映射中；各构建 `.data` 内容均为 142,888 字节。

测量结束后移除入口调用和 quartz 对 quartz-domain 的临时依赖，恢复锁文件中的正式依赖关系，并重新构建基线。重建后 exe 恢复为 12,868,608 字节，各 PE 节长度与基线相同；二进制差异仅为 PE / 调试目录时间戳与 CodeView GUID，屏蔽这些构建元数据后逐字节一致。第 08 章没有单独设置可执行文件体积上限；H10 本步骤完成体积测量，常驻核心的 10 分钟内存验收随 M1 采集与存储接入后的资源回归完成。

### 验证

| 检查 | 结果 |
|---|---|
| `cargo fmt --all --check` | 通过 |
| Linux `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| Linux `cargo test --workspace` | 29 项通过，其中 quartz-domain 7 项 |
| `cargo deny check` | advisories、bans、licenses、sources 全部通过；重复版本仅有警告 |
| Windows `cargo.exe clippy --workspace --all-targets -- -D warnings` | 通过 |
| Windows `cargo.exe test --workspace` | 38 项通过，其中 quartz-domain 7 项 |
| `cargo doc -p quartz-domain --no-deps` | 通过 |
| Windows `cargo.exe build --release -p quartz` | 基线、仅规范化与折叠、完整模块及移除临时调用后的重建均通过 |

7 项文本与域名测试覆盖：全角 / 半角拉丁字母数字与片假名、兼容连字、德语 ß / ẞ、土耳其语点 / 无点 i 的非 Turkic 折叠、希腊末尾 sigma、中文子串、组合音标、折叠后的再次规范化、IDN / Punycode 往返、域名标签边界和无效域名。

### 依赖版本

逐项执行 `cargo info --registry crates-io <crate>` 核对全部外部直接依赖；required 来自工作区清单，resolved 来自 Cargo.lock，latest 来自本次 registry 查询。新增 ICU4X 依赖只启用 `compiled_data`；idna 只启用 `alloc` 与 `compiled_data`，snafu 复用工作区配置。

| crate | required | resolved | latest |
|---|---|---|---|
| `anyhow` | `1` | `1.0.104` | `1.0.104` |
| `clap` | `4.6.8` | `4.6.8` | `4.6.8` |
| `gpui-kit` | `=0.10.0` | `0.10.0` | `0.10.0` |
| `icu_casemap` | `2.4.0` | `2.4.0` | `2.4.0` |
| `icu_normalizer` | `2.4.0` | `2.4.0` | `2.4.0` |
| `idna` | `1.1.0` | `1.1.0` | `1.1.0` |
| `rust-i18n` | `4.2.4` | `4.2.4` | `4.2.4` |
| `serde` | `1` | `1.0.229` | `1.0.229` |
| `serde_json` | `1` | `1.0.154` | `1.0.154` |
| `snafu` | `0.9.2` | `0.9.2` | `0.9.2` |
| `tracing` | `0.1` | `0.1.44` | `0.1.44` |
| `tracing-subscriber` | `0.3.23` | `0.3.23` | `0.3.23` |
| `windows` | `0.62.2` | `0.62.2` | `0.62.2` |
| `jiff` | 本步骤未添加 | — | `1.0.0` |

`icu_casemap_data` 是本次新增的间接依赖，resolved 与本次查询的 latest 均为 2.4.0。`cargo update --verbose --dry-run` 的可更新包数为 0；上游精确约束保留 `generic-array 0.14.7`（已发布 0.14.9）及 `gpui-pre` 系列 0.3.12（已发布 0.3.13）。新增 ICU4X 依赖声明的最低 Rust 版本为 1.88，idna 为 1.57，其实际后端 idna_adapter 1.2.2 为 1.86；当前 Rust 1.100.0 完成了全部构建，rust-toolchain.toml 保持原样。

## 2026-11-16 M1-B：SQLite 存储、迁移与 H9

基线代码提交为 `ba2a3ccdbf0dba1cf71cfdbf1d4672aeb61eddf6`（M1-A）。本次使用 Windows x86_64、Rust 1.100.0 和仓库 release 配置：`opt-level = 3`、`lto = "fat"`、`codegen-units = 1`、`panic = "abort"`、`strip = true`，静态链接 C 运行库。存储实现以未提交修改进行构建。

### 实现范围

`quartz::store` 包含 schema v1 的全部 20 张表、16 个显式索引、21 组枚举标签和 5 个排查视图。全部表均提供 `Queryable`、`Selectable`、`Insertable`、`AsChangeset` 行模型，并用 `check_for_backend(Sqlite)` 检查类型。实体标识、UTC 毫秒、单调时间与时长使用 `i64`。枚举通过同一个宏生成显式编码、SQLite 序列化与反序列化、用于校验的标签行；未知编码返回结构化解码错误。

核心持有数据目录锁后，在主线程完成版本检查、迁移前备份、迁移与连接初始化，再进入消息循环。已有数据库先以 `mode=ro` 检查未知迁移，兼容后才打开写连接并设置 WAL；只读查询连接另设 `query_only=ON`。写连接采用第 03 章的全部连接设置，保留自动 checkpoint。此步骤增加的线程数为 0。数据库线程、采集运行与时钟段的写入、批量写入、确认点和空闲维护调度在后续步骤接入。

`quartz restore-backup <file>` 在持有同一数据目录锁期间执行，核心运行时拒绝恢复。命令先用英文列出源备份、目标数据库和数据丢失提示；没有 `--yes` 时仅返回非零退出码，提供 `--yes` 后检查源库的版本、完整性和外键，生成独立副本，同步副本到磁盘，对目标库执行 WAL checkpoint，最后原子替换主库。源库使用 `mode=ro`；仅在生成副本期间关闭 `query_only`，允许 `VACUUM INTO` 写出目标文件。

### 设计细节

- v1 的表、CHECK 和索引采用第 03 章给出的 SQL；枚举标签使用列注释中的名称。排查视图保留原始列，增加枚举名称与本地时间；前台视图连接应用与标题，网页视图连接应用、标题与域名。
- 第 8.2 节的版本兼容性按已应用迁移的集合判断。任何程序不认识的迁移均拒绝启动，包括版本号更小但未知的迁移。
- 初次创建的空库也先备份，旧版本标为 `0`。备份名使用 jiff 的 UTC 毫秒时间戳，成功迁移后仅清理程序生成的旧迁移备份，保留最近两份。
- Diesel 的迁移接口使用擦除错误类型，且 SQL 迁移的具体运行错误未公开。自定义 SQL 迁移适配器保留内嵌迁移的名称与事务元数据，在 `store::raw` 执行 SQL，立即把接口返回的错误转换回具体类型；自有错误类型不保存 `Box<dyn Error>`。Rust 迁移与 SQL 迁移共用自定义来源和版本序列。
- `store/schema.rs` 由 Diesel CLI 2.4.0 生成。`diesel.toml` 和 `crates/quartz/schema.patch` 将 SQLite `INTEGER` 的时间、时长、标识等字段映射为 `BigInt`，枚举与布尔标志保留整数类型。执行 `deno task check-schema` 会迁移临时空库、重新生成 schema 并比较，需要 Deno 2.9.8；Diesel CLI 安装命令为 `cargo install diesel_cli --version 2.4.0 --no-default-features --features sqlite-bundled --locked`。
- `crates/quartz/migrations.lock` 校验迁移目录中每个文件的 SHA-256 和完整文件清单，增删或修改文件都会导致测试失败。`tests/fixtures/db/v1.db` 保存全部表的测试数据，覆盖开放记录、零时长、Unicode、隐藏标题和未知 JSON 字段。

### 依赖版本与许可

对全部外部直接依赖逐项执行 `cargo info --registry crates-io <crate>`；required 来自工作区清单，resolved 来自 `Cargo.lock`，latest 来自 registry 查询。Diesel 2.4.0 允许 `libsqlite3-sys >=0.17.2, <0.41.0`，因此使用最新的 0.40.0；该版本内嵌 SQLite 3.54.1，高于启动要求的 3.51.3。

| crate | required | resolved | latest |
|---|---|---|---|
| `diesel` | `2.4.0` | `2.4.0` | `2.4.0` |
| `diesel_migrations` | `2.4.0` | `2.4.0` | `2.4.0` |
| `libsqlite3-sys` | `0.40.0` | `0.40.0` | `0.40.0` |
| `jiff` | `1.0.0` | `1.0.0` | `1.0.0` |
| `sha2` | `0.11.1` | `0.11.1` | `0.11.1` |
| `tempfile` | `3.27.0` | `3.27.0` | `3.27.0` |
| `anyhow` | `1` | `1.0.104` | `1.0.104` |
| `clap` | `4.6.8` | `4.6.8` | `4.6.8` |
| `gpui-kit` | `=0.10.0` | `0.10.0` | `0.10.0` |
| `icu_casemap` | `2.4.0` | `2.4.0` | `2.4.0` |
| `icu_normalizer` | `2.4.0` | `2.4.0` | `2.4.0` |
| `idna` | `1.1.0` | `1.1.0` | `1.1.0` |
| `rust-i18n` | `4.2.4` | `4.2.4` | `4.2.4` |
| `serde` | `1` | `1.0.229` | `1.0.229` |
| `serde_json` | `1` | `1.0.154` | `1.0.154` |
| `snafu` | `0.9.2` | `0.9.2` | `0.9.2` |
| `tracing` | `0.1` | `0.1.44` | `0.1.44` |
| `tracing-subscriber` | `0.3.23` | `0.3.23` | `0.3.23` |
| `windows` | `0.62.2` | `0.62.2` | `0.62.2` |

新增直接依赖均关闭默认 feature：Diesel 启用 `std`、`sqlite`、SQLite `RETURNING` 和支持前台表所需的 `32-column-tables`；diesel_migrations 启用 `sqlite`；libsqlite3-sys 启用 `bundled`；jiff 仅启用 `std`，用于备份时间戳；sha2 和 tempfile 仅用于测试。新增的 23 个锁文件包声明的许可为 MIT、MIT OR Apache-2.0、MIT/Apache-2.0、Unlicense OR MIT；jiff 系列按 MIT 接受。`deny.toml` 保持不变。

### H9 测量

基线和存储实现分别执行 Windows `cargo.exe build --release --workspace`，按实际文件字节数比较。增量编译先完成 Windows debug `cargo.exe build -p quartz`，只将 `store/mod.rs` 中的一条日志文本从 `upgrading database` 改为 `applying database migrations`，再用 Stopwatch 测量同一构建命令；构建日志仅重新编译 quartz。测量后恢复原始源码，并重新构建 release。

| 指标 | M1-A 基线 | M1-B | 变化 |
|---|---:|---:|---:|
| `quartz.exe`（字节） | 12,868,096 | 15,003,136 | +2,135,040（2.036 MiB，16.59%） |
| Windows debug 一行增量编译（秒） | — | 9.028 | 单次 Stopwatch 实测；Cargo 报告 8.89 秒 |
| 核心启动后 Private Bytes（字节） | 2,486,272 | 3,276,800 | +790,528（0.754 MiB） |
| 核心启动后线程数 | 4 | 4 | 0 |

Private Bytes 在启动完成后约 3 秒读取 `Process.PrivateMemorySize64`，只作本步骤的短时观察；核心没有加载 `nv*.dll`。第 08 章的 10 分钟空闲、界面开关、ETW 唤醒和真实采集负载回归在 M1 完成时执行。第 08 章未设置 exe 大小或增量编译时间上限；H9 本步骤完成大小与编译测量。

### 验证

| 检查 | 结果 |
|---|---|
| `cargo fmt --all --check` | 通过 |
| Linux `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| Linux `cargo test --workspace` | 45 项通过，其中 store 11 项 |
| `cargo deny check` | advisories、bans、licenses、sources 全部通过；重复版本仅有警告；规则未改动 |
| Windows `cargo.exe clippy --workspace --all-targets -- -D warnings` | 通过 |
| Windows `cargo.exe test --workspace` | 56 项通过 |
| Windows `cargo.exe build --release --workspace` | 通过；本轮构建 quartz.exe 为 14,997,504 字节 |
| 原 schema 检查脚本（Diesel CLI 2.4.0；当前入口为 `deno task check-schema`） | 空库迁移与 schema.rs 比较通过 |
| Windows 核心与恢复命令冒烟检查 | 核心存活并创建 quartz.db；迁移记录为 `20261116000000`，20 张业务表、5 个视图，完整性检查为 ok；第二次启动返回 0；通过 taskkill 停止核心 |
| Core/UI 文件日志 | 核心与 `ui --prewarm` 均启动成功，core.log 与 ui.log 正常写入，UI 预热占位进程返回 0 |
| 不提供 `--yes` 的恢复命令 | 返回 1，列出源备份、目标数据库及数据丢失提示；不存在的数据目录保持不存在，已有目录的全部文件清单及 SHA-256 保持不变；标准输入保持打开仍立即退出 |
| 核心运行时 `quartz.exe restore-backup <file> --yes` | 返回 1，控制台提示先停止核心 |
| 停止核心后使用 `--yes` 恢复 v1 样本 | 返回 0；恢复后迁移版本、完整性与两条样本运行记录均正确 |

store 测试覆盖空库迁移及重复启动、枚举标签与未知编码、只读连接与特殊路径、全部原始语句的准备、CHECK 与三组开放行部分唯一索引、未来 schema 拒绝且主库字节不变、样本库 schema 一致性、迁移中途失败的事务回滚与备份恢复、备份保留数量、迁移文件内容及清单校验。迁移失败测试通过同一个启动函数注入两条 Rust 迁移：第一条成功，第二条建表并写入设置后失败；第二条回滚，数据库停在第一条，恢复备份后 schema 与升级前完全相同。

`quartz-i18n` 的全部测试共用一个互斥锁，覆盖全局语言切换及翻译后端初始化。用 `cargo test -p quartz-i18n --lib --no-run --message-format=json`（Windows 使用 `cargo.exe`）取得 `compiler-artifact.executable`，在每次新建的进程中执行测试二进制并传入 `--test-threads=8`：Linux 连续 30 次、Windows 连续 10 次，每次 6 项测试全部通过。

核心启动、恢复命令、迁移失败备份列表和界面日志共用 `data_directory::resolve()` 与 `ResolveDataDirectoryError`，统一验证 `LOCALAPPDATA` 为绝对路径。

`logging::init_file_log` 接收已解析的数据目录和 `LogTarget::{Core, Ui}`，统一创建 logs、追加打开 core.log 或 ui.log 并安装订阅器；文件日志错误只由 `InitFileLogError` 定义，核心和界面启动错误只通过 `source` 包装。

`commands::restore_backup` 负责确认、目录、实例锁和存储恢复的调用顺序；数据目录的创建及其 `CreateDataDirectoryError` 由 `data_directory` 定义，平台模块只提供实例锁操作及其结果。

workspace 的窗口、托盘、进程、语言、领域转换、迁移、连接和恢复错误均已检查归属，已有的下层错误保持为 `source`，没有按错误文本分支。命令行提示和错误按第 10 章第 3 节保持英文，GUI 错误由 quartz-ui 按界面语言本地化。clap 测试覆盖 `--yes` 在文件前后的位置、默认不确认、缺少文件、错误确认写法和帮助文本；目录解析测试覆盖缺失、空值、相对路径和绝对路径。

## 2026-11-17 M1-B：启动错误 UI 与托盘边界

基于 `609810c` 及 M1-B 未提交修改构建，未修改原有暂存条目。核心删除全部 MessageBox 与错误文字展示，启动错误使用 `quartz-protocol::CoreErrorCode` 的稳定字符串：`newer_database`、`migration_failed`、`other_session`、`core_start_failed`。核心记录诊断后通过原生进程启动传递 `ui --core-error <code>`，备份路径逐个使用 `--backup-path <path>` 参数传递，核心返回 1；同会话重复启动仍返回 0。

`quartz-ui` 使用主窗口相同的无边框标题栏显示本地化错误、备份路径、`quartz restore-backup --yes "<path>"` 命令和恢复会丢失后续记录的提示。错误窗口关闭后进程返回 0。UI 日志初始化失败时仍可展示结构化错误。CoreError 的 JSON 与 argv 代码使用相同表示，测试覆盖四种代码往返、未知代码、独立备份路径和未知 JSON 字段；clap 检查迁移失败必须带备份路径，并禁止错误窗口与预热参数并用。

托盘的隐藏原生窗口、AppMessage/CoreEvent 转换、菜单与 Shell 调用集中在 `platform/windows/tray/`。核心只提交 `TrayState` 并处理 `TrayCommand::{OpenUi, Exit}`，窗口与图标的生命周期由模块管理。运行时故障通过管道推送状态并选择警告图标的接入点标记为 TODO(M2)。第 01 章第 5 节与第 07 章第 1 节记录了以后将托盘模块移到独立可执行文件的边界。

### 验证

| 检查 | 结果 |
|---|---|
| `cargo fmt --all --check` | 通过 |
| Linux `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| Linux `cargo test --workspace` | 50 项通过 |
| Windows `cargo.exe clippy --workspace --all-targets -- -D warnings` | 通过 |
| Windows `cargo.exe test --workspace` | 61 项通过 |
| `cargo deny check` | advisories、bans、licenses、sources 通过；没有放宽规则 |
| 原 schema 检查脚本（当前入口为 `deno task check-schema`） | 通过，迁移与 schema 未改变 |
| Windows `cargo.exe build --release --workspace` | 通过，quartz.exe 为 15,016,960 字节 |
| 核心正常启动、第二次启动、taskkill | 核心创建数据库并运行；第二次启动返回 0；按 PID 停止核心 |
| 托盘打开 UI | 向托盘隐藏窗口发送选择事件，实际启动普通 UI 子进程；关闭 UI 返回 0，核心继续运行 |
| 新版本数据库 | 在 v1 样本加入未知迁移版本 `99999999999999`，核心返回 1，UI 子进程收到 `newer_database` 与独立备份路径；主库 SHA-256 不变 |
| 迁移失败 | 无迁移记录的库预先建立 settings 表，触发 v1 SQL 的表名冲突；核心返回 1，UI 子进程收到 `migration_failed` 与实际 VACUUM INTO 备份路径；主库和备份完整性检查为 ok，测试标记数据保留，迁移中的 capture_runs 表回滚 |
| 另一会话占用 | 单独持有 quartz.lock 的一字节排他锁，不创建会话互斥量；核心返回 1，UI 子进程收到 `other_session` |
| 通用启动失败 | quartz.lock 路径被目录占用，核心返回 1，UI 子进程收到 `core_start_failed` |
| 错误窗口 | 四种实际启动错误均打开 GPUI 窗口，关闭后 UI 返回 0，核心保持退出；三个指定场景的暗色截图已检查 |
| 恢复命令回归 | 没有 --yes 时返回 1，目录与文件 SHA-256 不变；核心运行中带 --yes 拒绝恢复，核心停止后返回 0 并恢复样本数据 |

全部外部直接依赖再次逐项运行 `cargo info --registry crates-io <crate>`，最新发布版本与上一节依赖表一致；没有增加外部 crate，仅调整本地 crate 依赖及现有 serde、snafu 的使用位置。`quartz` 的 quartz-i18n 依赖限定 Windows，文字查找仅用于托盘；语言协商不生成错误文字。

## 2026-11-18 M1-E1：验收工具与事实范围查询

本次在 Windows x86_64、Intel Core i7-14700HX（28 个逻辑处理器）、Rust 1.100.0 上测量，源代码为 `e3cb5e5` 上的本次工作区修改。构建采用仓库 release 配置：`opt-level = 3`、`lto = "fat"`、`codegen-units = 1`、`panic = "abort"`、`strip = true`，静态链接 C 运行库。编译结束后再运行测量命令。

在仓库根目录执行以下 Windows 命令，模拟目录提前创建，数据库为新文件：

```powershell
cargo.exe build --release -p quartz --target-dir target/windows
New-Item -ItemType Directory -Force target/windows/sim
.\target\windows\release\quartz.exe gen-data --db target/windows/sim/typical-30d.db --profile typical --days 30 --seed 1
.\target\windows\release\quartz.exe bench-query --db target/windows/sim/typical-30d.db --runs 100
```

### 生成速度与数据库体积

生成器通过采集存储视图按天提交 `CaptureWrite`。typical 档工作日生成 2,000 个逻辑切片、550 个新标题，周末减半；含每秒标题变化、4,096 UTF-16 单元长标题、空闲、锁屏、睡眠、午夜拆分和采集缺口。网页行等待采集写入器支持，当前输出 900 个网页切片/工作日的目标数量并跳过写入。首日、中间日、末日的存储层时长守恒检查全部通过。

总耗时采用 `gen-data` 输出的内部计时，包含迁移、逐日事务、写连接关闭时的 WAL 写回、三日守恒检查和行数读取。`writes` 为 `CaptureWrite` 条数，事实行数为 `intervals`、`availability_intervals`、`capture_gaps` 的行数之和。

| 指标 | 实测 |
|---|---:|
| 模拟日数 | 30 |
| 总耗时（秒） | 6.678 |
| `CaptureWrite` 条数 | 104,469 |
| `CaptureWrite` 条数/秒 | 15,643 |
| 事实行数 | 52,120 |
| 事实行数/秒 | 7,805 |
| 数据库主文件（字节） | 7,966,720 |
| 数据库主文件（MiB） | 7.598 |

测量结束时，WAL 文件为 0 字节，共享内存文件为 32,768 字节。各表行数如下：

| 表 | 行数 |
|---|---:|
| `applications` | 4 |
| `availability_intervals` | 98 |
| `capture_gaps` | 5 |
| `capture_runs` | 30 |
| `clock_segments` | 35 |
| `intervals` | 52,017 |
| `timed_policies` | 0 |
| `titles` | 14,300 |
| `web_intervals` | 0 |
| `window_instances` | 120 |

### 事实范围读取

数据库包含从 2025-01-01 开始的 30 个模拟日。day、week、year 查询从最早运行的 UTC 开始时间起，分别读取 1、7、365 个 24 小时区间；year 范围读取库内已有的全部 30 日事实。每个范围在同一进程内连续执行 100 次，沿用进程与操作系统缓存，包含首次事实读取，使用 nearest-rank（排序后第 `ceil(次数 × 百分位)` 个样本）计算 P50 和 P95。计时包含 SQLite 读取和存储结果构造，返回结果的释放在计时之外。读取对象为前台与无前台事实、可用状态及缺口；已结束的前台和可用状态使用 `start_utc_ms >= from - 86_400_000` 条件，开放行和缺口使用完整相交条件，每次读取在一个只读快照事务内完成。

| 范围 | 前台与无前台 | 可用状态 | 缺口 | 总行数 | P50（ms） | P95（ms） | 最大值（ms） |
|---|---:|---:|---:|---:|---:|---:|---:|
| day | 2,000 | 3 | 1 | 2,004 | 2.662 | 2.884 | 3.755 |
| week | 12,004 | 23 | 1 | 12,028 | 20.065 | 21.331 | 21.939 |
| year | 52,017 | 98 | 5 | 52,120 | 229.515 | 260.142 | 269.688 |

### Release 文件与时区功能

本次新构建的 `quartz.exe` 为 17,567,232 字节（16.753 MiB）。体积对照基准为 `fe09a54` 的 CI 制品。该制品中的 `quartz.exe` 为 16,948,736 字节、`quartz-host.exe` 为 203,776 字节；本次增加 618,496 字节（+604 KiB），包含 jiff 内置时区数据与 `dump`、`gen-data`、`bench-query` 三个命令。`quartz` 的 jiff 依赖在 `std` 上启用 `tz-system`、`tzdb-bundle-platform`、`tzdb-zoneinfo`；Windows 构建由 `tzdb-bundle-platform` 带入 `jiff-tzdb` 的 IANA 时区数据，用于系统时区识别、本地日历日与 UTC 范围转换及夏令时计算。

### 验证

| 命令 | 结果 |
|---|---|
| `cargo fmt --all --check`（WSL，仅格式检查） | 通过 |
| Windows `cargo.exe clippy -p quartz -p quartz-domain -p quartz-i18n --all-targets --target-dir target/windows -- -D warnings` | 通过 |
| Windows `cargo.exe test -p quartz -p quartz-domain -p quartz-i18n --target-dir target/windows` | 211 项通过：quartz 122、quartz-domain 81、quartz-i18n 8 |
| Windows `cargo.exe build --release -p quartz --target-dir target/windows` | 通过 |

回归覆盖缺失主键与无效端点的类型化解码错误、开放确认点、存储枚举标签序列化、停止状态并集与空闲掩码、固定种子的行数与事实哈希、三档标题参数、生产路径与已有文件拒绝写入，以及小型模拟库的基准读取。

## 2026-11-18 M1-E2a：一年模拟数据与离线验收断言

本次使用当前 main `201fedc28921e90f137ea28a3c16e07562e394fc`，Windows 11 build 26300、Intel Core i7-14700HX（28 个逻辑处理器）、Rust 1.100.0。Windows release 构建命令为 `cargo.exe build --release -p quartz --target-dir target/windows`，构建成功，`quartz.exe` 为 17,543,680 字节。生成与基准串行执行，使用同一个 release 文件；本轮没有启动采集核心、UI、预览或资源测量任务。

在仓库根目录执行以下命令，两个数据库都是新文件，位于 gitignored 的 `D:\Documents\403F\Quartz\target\windows\sim`：

```powershell
New-Item -ItemType Directory -Force target/windows/sim
.\target\windows\release\quartz.exe gen-data --db target/windows/sim/m1-e2a-typical-365d-20261118.db --profile typical --days 365 --seed 1
.\target\windows\release\quartz.exe bench-query --db target/windows/sim/m1-e2a-typical-365d-20261118.db --runs 100
.\target\windows\release\quartz.exe gen-data --db target/windows/sim/m1-e2a-heavy-365d-20261118.db --profile heavy --days 365 --seed 1
.\target\windows\release\quartz.exe bench-query --db target/windows/sim/m1-e2a-heavy-365d-20261118.db --runs 100
```

生成耗时使用命令内部计时，包含迁移、逐日事务、关闭写连接时 WAL 写回、首日/中间日/末日守恒检查及行数统计。两档均按工作日完整参数、周末减半生成；网页写入尚待 M5，`web_intervals` 为零。与 [03 数据模型 §4.6](design/03-data-model.md) 决策后的 typical 150 MiB、heavy 442 MiB 估算对照时，需要同时考虑估算包含的网页行、工作日比例及当前生成事实的差异。

| 档位 | 生成耗时（s） | 主库字节 | 主库（MiB） | 估算（MiB） | 与估算差值（MiB） | CaptureWrite 数 |
|---|---:|---:|---:|---:|---:|---:|
| typical | 141.173 | 99,586,048 | 94.973 | 150 | -55.027 | 1,257,685 |
| heavy | 511.592 | 301,072,384 | 287.125 | 442 | -154.875 | 3,761,685 |

主库分别为估算的 63.32% 和 64.96%，均未执行 VACUUM。两档结束后 WAL 均为 0 字节，SHM 为 32,768 字节；首日、中间日、末日的存储守恒检查均通过。

| 表 | typical 行数 | heavy 行数 |
|---|---:|---:|
| applications | 4 | 4 |
| availability_intervals | 1,199 | 1,199 |
| capture_gaps | 53 | 53 |
| capture_runs | 365 | 365 |
| clock_segments | 418 | 418 |
| intervals | 626,208 | 1,878,208 |
| timed_policies | 0 | 0 |
| titles | 172,150 | 532,100 |
| web_intervals | 0 | 0 |
| window_instances | 1,460 | 1,460 |

### 一年库的事实范围读取

查询起点为首个运行的 UTC `1735754401000`（2025-01-01 18:00:01Z），day、week、year 分别读取随后 1、7、365 个 24 小时范围。每档、每范围在同一进程内执行 100 次，包含首次读取；P50/P95 使用 nearest-rank。计时包含 SQLite 取行与 Rust 事实结果构造，结果释放在计时之外。读取连接为只读、每次读取使用一个 WAL 快照，进程与系统缓存沿用；没有清空系统缓存。

| 档位 | 范围 | 前台与无前台 | 可用状态 | 缺口 | 总行数 | P50（ms） | P95（ms） | 最大值（ms） |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| typical | day | 2,000 | 3 | 1 | 2,004 | 2.657 | 2.915 | 3.961 |
| typical | week | 12,004 | 23 | 1 | 12,028 | 19.958 | 21.368 | 23.585 |
| typical | year | 626,208 | 1,199 | 53 | 627,460 | 2675.223 | 2864.804 | 2922.640 |
| heavy | day | 6,000 | 3 | 1 | 6,004 | 9.394 | 10.015 | 10.498 |
| heavy | week | 36,004 | 23 | 1 | 36,028 | 156.971 | 190.863 | 199.023 |
| heavy | year | 1,878,208 | 1,199 | 53 | 1,879,460 | 8101.305 | 8702.371 | 8915.049 |

### 预算对照与查询计划

[08 资源预算 §1.4](design/08-resources.md) 的 typical 一年日、周 P95 < 100 ms 条件，在本次首日起点的两个范围均满足，分别余量 97.085 ms 和 78.632 ms。heavy 日范围也低于同一参考值，周 P95 190.863 ms，比 100 ms 高 90.863 ms（1.909 倍），作为压力档结果记录。

typical 的 year 原始事实读取 P95 2864.804 ms，比年度汇总 500 ms 参考值高 2364.804 ms（5.730 倍）。heavy 的 year P95 为 8702.371 ms，比 500 ms 参考值高 8202.371 ms（17.405 倍）。M1 此命令读取前台、可用状态和缺口的全部相交事实并构造结果；年度汇总管线在 M2/M4 接入后验收。

只读诊断使用 WSL Python 的 SQLite 3.45.1，连接设置 `mode=ro`、`PRAGMA query_only=ON`；heavy 静态主库在确认 WAL 为 0 字节后使用 `immutable=1` 读取。两档计划相同，使用与 `store::facts::load_range` 相同的谓词，对 year 范围执行以下查询：

```sql
EXPLAIN QUERY PLAN SELECT * FROM intervals
WHERE start_utc_ms >= 1735668001000
  AND start_utc_ms < 1767290401000 AND end_utc_ms > 1735754401000;
EXPLAIN QUERY PLAN SELECT * FROM intervals
WHERE end_utc_ms IS NULL
  AND start_utc_ms < 1767290401000 AND confirmed_utc_ms > 1735754401000;
EXPLAIN QUERY PLAN SELECT * FROM availability_intervals
WHERE start_utc_ms >= 1735668001000
  AND start_utc_ms < 1767290401000 AND end_utc_ms > 1735754401000;
EXPLAIN QUERY PLAN SELECT * FROM availability_intervals
WHERE end_utc_ms IS NULL
  AND start_utc_ms < 1767290401000 AND confirmed_utc_ms > 1735754401000;
EXPLAIN QUERY PLAN SELECT * FROM capture_gaps
WHERE start_utc_ms < 1767290401000 AND end_utc_ms > 1735754401000;
```

| 分支 | EXPLAIN QUERY PLAN 的索引与范围 |
|---|---|
| 已关闭前台 | `intervals_time (start_utc_ms>? AND start_utc_ms<?)` |
| 开放前台 | `intervals_time (start_utc_ms<?)` |
| 已关闭可用状态 | `availability_time (start_utc_ms>? AND start_utc_ms<?)` |
| 开放可用状态 | `availability_time (start_utc_ms<?)` |
| 缺口 | `capture_gaps_time (start_utc_ms<?)` |

这里的读取形态是按时间索引逐行返回事实。year 的前台事实数量为 typical 626,208、heavy 1,878,208；heavy 的 week 为 36,004。开放行分支只有开始时间上界，所列计划选择时间索引，并未选择 `intervals_one_open` / `availability_one_open`，仍需检查上界之前的历史候选。根据返回规模和该计划判断，全年逐行读取、结果构造及开放分支的历史候选扫描是后续需要分析的成本来源。本步骤保留查询、索引和缓存设置。

### 离线工具验证与 E2b

`deno task check`（fmt、lint、严格 TypeScript 类型检查）通过，`deno task test` 的 81 项测试通过，所有夹具为手写的小型 JSON 对象。命令行另验证文件输入、标准输入成功返回 0，缺少覆盖事实的输入返回 1。新增工具使用现有依赖，没有新增包，`deno.json` 的精确版本与 `deno.lock` 保持冻结。Rust 源码未修改，本轮运行 Windows release 构建。

[人工验收手册](acceptance/m1.md) 包含 02 §6 的十组场景、M1 六条验收项、独立数据目录、托盘启动/退出、逐场景 dump/断言、崩溃前后快照和十分钟资源回归。现有 `LOCALAPPDATA` 覆盖可隔离到验收目录，无需新增生产路径配置；暂停、故障注入和精确时序采样入口的待准备条件在手册对应场景列出。

`dump` 当前没有 `--db`，按数据目录直接读取 `Quartz\quartz.db`，本轮未对模拟库运行 `deno task acceptance`。真实采集、用户在场操作和会开关界面 20 次的资源回归由 E2b 执行。

## 2026-11-20 M1-E2b：实机验收

设备同 E2a（Windows 11 build 26300、Intel Core i7-14700HX），通过 RDP 与本机在场两种方式执行；多显示器轮为主屏 2560×1440 加副屏 1707×1067。所有轮次使用 `target/windows/release/quartz.exe`，独立数据目录 `target\windows\acceptance-e2b\suite\Quartz`（`LOCALAPPDATA` 覆盖），由 gitignored 的自动化脚本 `target\windows\acceptance-e2b\e2b-suite.ps1` 启动核心、驱动输入、以 `WM_CLOSE` 正常退出、逐日 dump 并运行 `deno task acceptance`。脚本只注入 Alt+Tab、Win 组合键、Ctrl+Tab、Shift 和任务栏按钮点击，使用独立的测试窗口与隔离浏览器配置；UAC 取消、Ctrl+Alt+Del 与解锁由用户操作，改系统时间由用户 UAC 授权的管理员辅助进程执行，结束时按单调计时恢复并与网络时间核对。

切换误差由独立参考采样器给出：另一进程以约 1 ms 间隔轮询 `GetForegroundWindow` 并记录每次变化的 UTC 毫秒，按 HWND 匹配事实起点（窗口 −200…+2000 ms），误差取绝对值，按 `ceil(0.95 × N)` 取 P95。前台为空（HWND 0）不计；停留 ≥ 500 ms 却没有对应事实的变化计为漏记。

| 轮次 | 手册 | 构建 | 内容 | 断言 | `untriggered_changes` | 回调→事实 P95 / 最大 |
|---|---|---|---|---|---|---|
| switch | §3 | `2900e92` | Alt+Tab 120 次（< 200 ms）、任务栏点击 20 次、虚拟桌面往返 20 次 | PASS | 0 | 15 / 36 ms |
| monitor | §3 | `2900e92` | 跨显示器激活 20 次、跨显示器 Alt+Tab 20 次 | PASS | 0 | 6 / 10 ms |
| same-title | §4 | `2900e92` | 同进程两个同标题窗口交替各 20 次 | PASS | 0 | 12 / 16 ms |
| titles | §5、§12 | `7f40ab6` | 每秒标题 60 秒、每秒 10 次标题风暴、4,095/4,096/4,097 单元与代理项截断标题 | PASS | 0 | 11 / 16 ms |
| titles2 | §5、§12 | `2900e92` | 终端标题 20 次、Edge 标签 Ctrl+Tab 20 次、挂起目标进程后切走再返回 | PASS | 0 | 6 / 11 ms |
| uwp | §6 | `2900e92` | 设置最小化后还原、计算器冷启动停留 70 秒 | PASS | 0 | 114 / 114 ms（单样本，见下） |
| uac | §6 | `2900e92` | UAC 停留后取消、Ctrl+Alt+Del 停留后取消、UAC 同意 | PASS | 0 | 13 / 16 ms |
| crash | §9 | `7f40ab6` | 活动 70 秒后挂起、导出 before、强制结束、重启、正常退出 | PASS（`--before`） | 0 | 13 / 16 ms |
| clock | §8 | `2900e92` | 前拨 5 分钟、回拨 10 分钟、恢复 | PASS | 0 | — |
| midnight-utc | §8 | `2900e92` | 时间设为本地 07:58:30（UTC 23:58:30），跨 UTC 午夜后恢复 | PASS | 0 | — |
| midnight-local | §8 | `2900e92` | 时间设为本地 23:58:30，跨本地午夜后恢复 | PASS | 0 | — |
| idle | §8 | `2900e92` | 不操作 179 秒后输入，再不操作 190 秒后输入，6 秒后退出 | PASS | 0 | — |
| lock | §7 | `c0a2ee6` | 锁屏 10 次，每次随即解锁 | PASS | 0 | 19 / 23 ms |
| private | §6、H5 | `c0a2ee6` | Edge、Chrome、Firefox 普通窗口与 zh-CN、en-US 无痕窗口 | PASS | 0 | 10 / 12 ms |

构建列为运行时工作树对应的提交；运行时这些改动尚未提交，提交内容与运行时一致。所有轮次批量提交 P95 为 914–923 ms，低于 1 秒提交期限。

**切换误差**：switch 轮参考采样器记录 702 次变化，505 次与事实匹配，P95 12 ms、最大 187 ms，漏记 0；未匹配的变化中 132 次为前台为空，1 次为停留 180 ms 的过渡窗口（两次读取之间被替换，按 02 §1 接受不记录）。monitor 轮 104 次匹配，P95 7 ms、最大 29 ms，漏记 0。两项均满足 P95 < 100 ms。虚拟桌面与跨显示器切换均由 `EVENT_SYSTEM_FOREGROUND` 覆盖，无需补订阅。

**通知覆盖**：全部轮次 `untriggered_changes` 为 0；本节合计约 30 分钟真实切换，一小时连续轮未执行。此前手动轮（重设计后首轮）出现过 2 次后备复核先于迟到的 `EVENT_SYSTEM_FOREGROUND` 读到变化的竞争，已按原生事件时间延后判定处理。

**UWP**：计算器冷启动时内容窗口尚未挂入框架，身份由框架 AUMID 解析，显示名取自 AppsFolder（「计算器」）；34 秒后既有维护唤醒的内部复核以进程元数据补全 exe（`calculatorapp.exe`），窗口实例与 generation 不变、不切段。补全后的显示名来自 exe 版本信息（英文 “Calculator”）。该轮唯一的回调延迟样本 114 ms 来自首次 AppsFolder 查询初始化 Shell 命名空间；事实边界取回调开始时间，不受影响。

**安全桌面与锁屏**：UAC 提示 17.1 秒、Ctrl+Alt+Del 11.7 秒、第二次 UAC 5.75 秒均为 `no_foreground/unknown_desktop`，前一段前台以 `no_foreground` 结束，返回后立即读取。锁屏 10 次各有一条 `unknown_desktop`（结束原因 `locked`）与 `locked` availability（结束原因 `resumed`）。修复前，锁屏与解锁时收到的 `WM_TIMECHANGE` 使时钟段每次切换（墙上与单调映射仅偏移 −5…+3 ms，系统日志无改时记录）；`c0a2ee6` 后同样 10 次锁屏只有一个时钟段。

**时钟与空闲**：改时轮旧前台以 `clock_jump` 结束、旧段以 `time_change` 结束；UTC 午夜在本地 08:00:00 产生 `midnight_split`，片段共享 `session_key`；本地午夜不拆分事实，按本地日期 dump 两日均可读到跨午夜行。空闲轮 179 秒处未进入 idle；第二次 idle 起点等于 `last_input_utc_ms + 180000`，检测延迟 15 ms，注入输入后约 30 ms 以 `resumed` 结束。

**浏览器无痕窗口（H5）**：

| 浏览器 | 普通窗口标题 | 无痕窗口标题（zh-CN / en-US） | 结果 |
|---|---|---|---|
| Edge 157.0.4341.60 | `… - 用户配置 1 - Microsoft Edge` | `… - [InPrivate] - Microsoft Edge`（两种语言相同） | 命中 `browser-private-edge`，`title_hidden` |
| Firefox 160.0.1 | `… — Mozilla Firefox` | `… — Mozilla Firefox 隐私浏览` / `… — Mozilla Firefox Private Browsing` | 命中对应语言规则，`title_hidden`；首个读取的标题为不含页面信息的 `Mozilla Firefox` |
| Chrome 157.0.8103.80 | `… - Google Chrome` | `… - Google Chrome`（两种语言相同） | **无法识别**，页面标题完整保存 |

Chrome 无痕窗口标题与普通窗口相同，`browser-private-chrome-zh-cn` 与 `browser-private-chrome-en` 在该版本不会命中。用户决定暂不启用 06 §3.4 的保守规则，记为 M1 已知缺口。

**未执行**：按用户决定，睡眠 10 次、RDP 断开与重连、长锁屏崩溃、一小时连续轮、`deno task measure` 十分钟资源回归与 §18 体积和增量编译本轮未执行；H9、H10 的常驻内存与 08 §1.1 资源验收仍待 `measure`。

**一年模拟库复测**：使用 `7f40ab6` 的 release 重新生成 `m1-e2b-typical-365d.db` 与 `m1-e2b-heavy-365d.db`（seed 1）。typical 生成 46.5 秒、99,586,048 字节，day/week/year P95 为 3.927/22.023/1197.072 ms；heavy 生成 141.2 秒、301,072,384 字节，day/week/year P95 为 9.589/212.614/8703.846 ms，三日守恒均通过。同一 heavy 库在被中止的前一次运行中 week P95 为 64.395 ms，生成耗时也比 E2a 快约 3 倍，读数随机器负载与缓存状态波动较大，正式对照需在空闲机器上重测。

**后续事项**：窗口在身份解析期间关闭时 `window class unavailable` 以 WARN 记录，属正常竞争，可降为 DEBUG；锁屏广播 `WM_TIMECHANGE` 的来源未定位。

## 2026-11-20 M2-C1：一年模拟库上的查询测量

环境为 Windows 11 build 26300、Intel Core i7-14700HX、Rust 1.100.0，代码基线为 `b825fd3`。使用仓库的 release 配置构建；日查询使用当前系统时区 `Asia/Shanghai`，逻辑日为 2025-01-02，`day.start_offset_min` 为 0。日范围为 UTC `[1735747200000, 1735833600000)`，周范围从同一起点覆盖 7 天；时间轴采用 `app` 细节和 60,000 毫秒/像素。

### 数据与命令

沿用以下命令生成的 typical 365 天模拟库。生成耗时 171.696 秒，主库 99,586,048 字节（94.973 MiB），写入命令 1,257,685 条；3 个抽样日期的守恒检查通过。库内有 626,208 条前台与无前台事实、1,199 条可用状态、53 条缺口、365 个运行、418 个时钟段、4 个应用、1,460 个窗口实例和 172,150 个标题，网页行数为 0。

```powershell
.\target\windows\release\quartz.exe gen-data --db target/windows/sim/m2-c1-typical-365d-20261120.db --profile typical --days 365 --seed 1
```

检查、构建和基准命令如下；复测使用同一个模拟库。

```powershell
cargo.exe fmt --all
cargo.exe clippy --workspace --all-targets --all-features --target-dir target/windows -- -D warnings
cargo.exe test --workspace --all-features --target-dir target/windows
cargo.exe build --release -p quartz --target-dir target/windows
.\target\windows\release\quartz.exe bench-query --db target/windows/sim/m2-c1-typical-365d-20261120.db --runs 100
```

每种查询执行 100 次，包含首次执行，百分位使用 nearest-rank，沿用 SQLite 与系统缓存。管线计时包含只读事务、全部取数、完整切片与聚合、分页选择和首个响应体编码，返回结果释放在计时之外。原始事实读取从首个运行开始的 UTC `1735754401000` 起算，分别覆盖 1、7、365 个 24 小时范围。

### 初测

初测先执行原始事实读取，再执行管线，release 可执行文件为 18,410,496 字节。

| 原始事实范围 | 前台与无前台 | 可用状态 | 缺口 | 总行数 | P50（ms） | P95（ms） | 最大值（ms） |
|---|---:|---:|---:|---:|---:|---:|---:|
| day | 2,000 | 3 | 1 | 2,004 | 2.708 | 2.940 | 3.657 |
| week | 12,004 | 23 | 1 | 12,028 | 19.674 | 20.614 | 20.883 |
| year | 626,208 | 1,199 | 53 | 627,460 | 2602.935 | 2784.023 | 2890.001 |

| 查询管线 | 首个响应体字节数 | P50（ms） | P95（ms） | 最大值（ms） |
|---|---:|---:|---:|---:|
| `query.day` | 4,918 | 12.617 | 17.065 | 18.165 |
| `query.timeline`：日 | 200,886 | 13.759 | 19.064 | 23.074 |
| `query.timeline`：周 | 200,886 | 103.535 | 127.607 | 137.986 |

[08 资源预算 §1.4](design/08-resources.md) 要求日、周查询 P95 小于 100 毫秒。初测日查询达到目标，周时间轴超过目标 27.607 毫秒；Private Bytes 峰值未测量。

初测的格式化与 Clippy 通过，测试 465 项通过、1 项实机测试忽略。真实线程测试在 20,000 条历史事实的日查询已开始后提交暂停事件，确认写入提交延迟小于 1 秒。

### 阶段分析与复测

阶段计时使用 `Instant`，仅由 bench-query 启用；核心的分批查询保持原有执行路径。取数阶段包括建立快照、读取版本和 SQL 行转换；切片阶段包括边界构造与扫描；聚合阶段包括合并；分页与编码阶段包括页预算检查和首个响应体编码。各阶段分别统计 100 次样本的百分位。

本轮先测管线，再测原始事实读取，以便在全年读取分配大量内存之前取得空闲基线。优化前、后均使用这一顺序，前后对照以本轮重新取得的阶段基线为准。

阶段计时定位到分批取数为主要开销。已结束事实的 SQL 原先始终以查询起点前一天作为时间索引下界，每批再检查游标条件；修改后将索引下界推进到查询下界与上一批开始时间的较大值，同时保留同时间戳下的行 ID 条件。批次仍为 256 行，批间继续优先服务写入；同时间戳跨批读取和写入提交延迟测试均通过。

| 查询 | 阶段 | 优化前 P50（ms） | 优化前 P95（ms） | 优化后 P50（ms） | 优化后 P95（ms） |
|---|---|---:|---:|---:|---:|
| 日汇总 | 取数 | 3.728 | 4.176 | 3.574 | 3.957 |
| 日汇总 | 边界构造与切片 | 0.819 | 0.954 | 0.857 | 0.956 |
| 日汇总 | 聚合与合并 | 0.152 | 0.186 | 0.157 | 0.169 |
| 日汇总 | 分页与编码 | 0.014 | 0.024 | 0.018 | 0.035 |
| 日时间轴 | 取数 | 3.804 | 4.376 | 3.667 | 4.034 |
| 日时间轴 | 边界构造与切片 | 0.818 | 0.928 | 0.887 | 0.978 |
| 日时间轴 | 聚合与合并 | 0.387 | 0.446 | 0.375 | 0.445 |
| 日时间轴 | 分页与编码 | 0.490 | 0.580 | 0.567 | 0.659 |
| 周时间轴 | 取数 | 30.100 | 32.377 | 19.295 | 20.854 |
| 周时间轴 | 边界构造与切片 | 6.055 | 6.945 | 6.085 | 6.672 |
| 周时间轴 | 聚合与合并 | 2.393 | 2.654 | 2.383 | 2.873 |
| 周时间轴 | 分页与编码 | 0.739 | 0.908 | 0.736 | 0.854 |

| 查询 | 优化前 P50（ms） | 优化前 P95（ms） | 优化前最大值（ms） | 优化后 P50（ms） | 优化后 P95（ms） | 优化后最大值（ms） |
|---|---:|---:|---:|---:|---:|---:|
| 日汇总 | 4.765 | 5.312 | 5.899 | 4.642 | 5.124 | 5.880 |
| 日时间轴 | 5.575 | 6.183 | 7.207 | 5.563 | 6.035 | 6.440 |
| 周时间轴 | 39.629 | 43.076 | 53.488 | 28.910 | 30.862 | 44.644 |

Windows 使用 `GetProcessMemoryInfo` 的 `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`，在三类管线查询开始前取得共同空闲基线，每次查询前和结束时采样，记录各类查询 100 次运行中的采样峰值；采样位于耗时区间之外。优化前空闲基线为 2,740,224 字节，优化后为 2,744,320 字节。

| 查询 | 优化前采样峰值（bytes） | 优化前峰值增量（MiB） | 优化后采样峰值（bytes） | 优化后峰值增量（MiB） |
|---|---:|---:|---:|---:|
| 日汇总 | 6,062,080 | 3.168 | 6,029,312 | 3.133 |
| 日时间轴 | 6,594,560 | 3.676 | 6,606,848 | 3.684 |
| 周时间轴 | 11,948,032 | 8.781 | 11,948,032 | 8.777 |

复测的原始事实行数与初测一致，耗时如下：

| 范围 | P50（ms） | P95（ms） | 最大值（ms） |
|---|---:|---:|---:|
| day | 2.723 | 3.027 | 4.061 |
| week | 20.404 | 22.565 | 28.069 |
| year | 1169.014 | 1234.929 | 1319.119 |

复测日汇总、日时间轴和周时间轴的 P95 均小于 100 毫秒；周时间轴从同口径基线的 43.076 毫秒降至 30.862 毫秒，取数阶段 P95 从 32.377 毫秒降至 20.854 毫秒。查询的最大采样内存增量为 8.777 MiB，低于 20 MiB 预算。当前继续直接计算，未加入日结果缓存。

最终格式化、Clippy、协议文档构建和 release 构建通过；工作区测试 469 项通过，1 项既有实机测试忽略。

## 2026-11-20 M2-C2：分类查询测量

Windows 11 build 26300、Intel Core i7-14700HX、Rust 1.100.0，使用仓库 release 配置。C1 为 `ff8854a` 独立归档构建，C2 优化前为加入独立分类计时的原 C2 实现，优化后为当前工作树。使用 seed 1、365 天、2025-01-01 起点的 typical 模拟数据，在计时外安装五个分类与 50 条规则，并准备原应用和真实软件映射两个副本；同一负载下各版本只读同一个文件。以下对比在同一会话中串行执行，每组 100 次，包含首次查询，保留进程与操作系统缓存，按 nearest-rank 统计，未绑定 CPU，采样期间没有编译任务。

C1 运行 `bench-query --db <已准备模拟库> --runs 100`，C2 运行 `bench-query --db <已准备模拟库> --runs 100 --scope pipeline --input prepared`。日查询使用 `Asia/Shanghai`、2025-01-02、日起点偏移 0，UTC 范围 `[1735747200000, 1735833600000)`；七天时间线从同一起点开始，使用 `app` 细节与 60,000 毫秒/像素。读取包含快照准备与结束、分类元数据读取和应用元数据加载；分页与编码包含首个响应体序列化。表内均为 P95，单位为毫秒。

原应用 `editor/browser/terminal/player.exe` 全部未分类：

| 版本 | 查询 | 读取 | 边界扫描与切片 | 分类 | 聚合与合并 | 分页与编码 | 总计 |
|---|---|---:|---:|---:|---:|---:|---:|
| C1 | 日汇总 | 3.617 | 0.917 | — | 0.162 | 0.023 | 4.710 |
| C1 | 七天时间线 | 59.178 | 19.534 | — | 7.585 | 2.379 | 86.845 |
| C2 优化前 | 日汇总 | 4.282 | 1.082 | 2.031 | 0.242 | 0.023 | 7.501 |
| C2 优化前 | 七天时间线 | 67.107 | 24.047 | 44.045 | 10.797 | 4.203 | 147.567 |
| C2 优化后 | 日汇总 | 2.932 | 0.792 | 0.069 | 0.159 | 0.017 | 3.941 |
| C2 优化后 | 七天时间线 | 38.839 | 13.164 | 0.875 | 6.606 | 1.897 | 62.563 |

真实软件映射为 `code.exe/msedge.exe/windowsterminal.exe/vlc.exe`，覆盖开发、娱乐和未分类：

| 版本 | 查询 | 读取 | 边界扫描与切片 | 分类 | 聚合与合并 | 分页与编码 | 总计 |
|---|---|---:|---:|---:|---:|---:|---:|
| C2 优化前 | 日汇总 | 4.069 | 1.095 | 2.046 | 0.298 | 0.051 | 7.545 |
| C2 优化前 | 七天时间线 | 61.152 | 24.142 | 41.191 | 16.456 | 4.434 | 141.549 |
| C2 优化后 | 日汇总 | 3.033 | 0.829 | 0.086 | 0.176 | 0.035 | 4.131 |
| C2 优化后 | 七天时间线 | 36.002 | 13.274 | 0.818 | 7.628 | 2.070 | 61.685 |

分类先在前台事实层计算，切片读取查询内结果索引；缓存依赖由有效规则读取字段决定，只规范化这些字段。七天读取 12,004 条前台事实，其中 11,913 条窗口事实形成 7,905 个应用、标题与采集级别组合；内置规则只读取可执行文件名，因而只需对 4 个应用完整求值。应用元数据按批次收集缺少的 ID，在同一快照中读取一次并共享，前台扫描不再逐行联接应用表；分类引用和规则标识也共享存储，减少切片与聚合的字符串克隆。缓存随查询结束释放。

另按默认隔离副本流程运行 `bench-query --db <原模拟库> --runs 100 --scope pipeline`，同样使用默认调度：

| 负载 | 日汇总 P95（ms） | 日时间线 P95（ms） | 七天时间线 P95（ms） |
|---|---:|---:|---:|
| 原模拟应用 | 3.821 | 4.407 | 78.029 |
| 真实软件映射 | 16.000 | 16.186 | 84.566 |

同库与默认隔离流程的真实应用七天 P95 分别为 61.685 和 84.566 ms，均满足 [08 第 1.4 节](design/08-resources.md) 的 100 ms 目标。按每次查询前后采样，两种负载的最大 Private Bytes 增量在同库流程为 8.684 MiB，在隔离流程为 5.191 MiB，均低于 20 MiB。

补充固定逻辑处理器 0（亲和性掩码 1）运行当前版本、真实软件映射副本 100 次，日汇总、日时间线、七天时间线 P95 分别为 4.418、6.021、38.427 ms；预算判断采用上述默认调度结果。

验证通过：`cargo.exe fmt --all`、`cargo.exe clippy --workspace --all-targets --all-features --target-dir target/windows -- -D warnings`、`cargo.exe clippy --workspace --all-targets --target-dir target/windows -- -D warnings`。修复捕获队列溢出测试的清理通知同步后，`cargo.exe test --workspace --all-features --target-dir target/windows` 连续 5 轮通过，每轮 488 项测试通过，1 项需要可见 ApplicationFrameWindow 的检查被忽略。

## M2-D3：注解查询与待标注空闲

2026-11-20，比较 `49e2c5d` 与本次工作树的 release 构建，使用默认调度，不设置进程亲和性。两者采用相同的 365 天典型模拟数据，应用映射为 `code.exe`、`msedge.exe`、`windowsterminal.exe` 和 `vlc.exe`，提前安装内置分类和规则。无注解负载的注解表为空。

注解负载从同一份模拟数据复制，在首七天各加入三条注解，共 21 条：日起点后 `[0, 60 分钟)` 为 learning 离线活动，`[8 小时, 8 小时 30 分钟)` 为不指定分类的有效活动覆盖，`[10 小时, 10 小时 20 分钟)` 为 communication 分类覆盖。测量完成后删除此次专用的注解数据库副本。

构建命令为 `cargo.exe build --package quartz --release --target-dir target/windows`；对照版本从 `49e2c5d` 的源码归档构建。每份已准备的模拟库执行 `quartz.exe bench-query --db <模拟库> --runs 100 --scope pipeline --input prepared`。每项查询采样 100 次，报告包含快照准备、完整派生与聚合、首个响应体分页和编码的总耗时；`pending_away` 查询使用与日汇总相同的逻辑日期。

验证通过：`cargo.exe fmt --all`、`cargo.exe clippy --workspace --all-targets --all-features --target-dir target/windows -- -D warnings`、`cargo.exe clippy --workspace --all-targets --target-dir target/windows -- -D warnings`。`cargo.exe test --workspace --all-features --target-dir target/windows` 连续三轮通过，每轮 542 项通过、1 项忽略；忽略项需要已有可见的 ApplicationFrameWindow。

逻辑日期为 `2025-01-02`，核心系统时区为 `Asia/Shanghai`，日起点偏移为 0，日范围为 `[1735747200000, 1735833600000)`；七天时间轴从同一起点开始，使用 app 细节和 60,000 毫秒/像素。两个版本均构建完成后，无注解负载交替执行四轮，每轮各项 100 次；表中为四轮 P95 的中位数。注解负载执行一轮，每项 100 次。

| 版本与负载 | 日汇总 P95（ms） | 日时间轴 P95（ms） | 七天时间轴 P95（ms） | pending_away P95（ms） |
|---|---:|---:|---:|---:|
| `49e2c5d`，无注解 | 7.343 | 8.727 | 51.301 | — |
| 当前版本，无注解 | 7.799 | 9.137 | 54.210 | 10.055 |
| 当前版本，21 条注解 | 7.299 | 10.016 | 57.086 | 11.742 |

无注解对照四轮的日汇总、日时间轴、七天时间轴 P95 范围分别为 6.751–7.888、7.790–9.513、50.006–52.653 ms；当前版本分别为 6.926–9.466、8.839–10.648、51.414–55.203 ms，pending_away 为 8.865–11.169 ms。前三项 P95 中位数分别高 6.20%、4.70%、5.67%，各轮测量区间重叠，差异与本轮默认调度下的波动相当。当前日汇总和七天时间轴均低于 D1 约 10.3 和 61 ms 的参考值。

当前无注解与注解负载的最大 Private Bytes 增量为 7.914 MiB，低于 20 MiB。新增 `timeline-annotations.json` 和 `pending-away.json` 两份协议 golden；原有无注解 day 和 timeline golden 文件逐字节保持不变。

## 2026-11-21 M2-F1：验收自动化

基线为 `cb5c666` 加本次未提交修改。Windows 11 build 26300.9974，系统时区为 China Standard Time（UTC+08:00），Rust 1.100.0，Windows 与 WSL 的 Deno 均为 2.9.8。Rust 验证使用 Windows 工具链和 `target/windows`；Deno 检查与夹具测试在 WSL 执行。

```powershell
cargo.exe fmt --all
cargo.exe clippy --workspace --all-targets --all-features --target-dir target/windows -- -D warnings
cargo.exe clippy --workspace --all-targets --target-dir target/windows -- -D warnings
cargo.exe test --workspace --all-features --target-dir target/windows
cargo.exe test --workspace --all-features --target-dir target/windows
deno task check
deno task test
cargo.exe test -p quartz --all-features --target-dir target/windows annotations_lost_response -- --nocapture
cargo.exe test -p quartz --all-features --target-dir target/windows stalled_state_reader_preserves_capture_commits_status_budget_and_private_bytes -- --nocapture
```

全部通过。最终工作树的完整工作区测试连续两轮各 588 项通过、2 项忽略；忽略项为已有可见 ApplicationFrameWindow 检查和新增十分钟慢客户端测试。Deno 共 90 项通过，包含 8 项摘要计算夹具测试，覆盖窗口并集、独立 idle/停止/缺口遮罩、隐私字段、NoForeground、重复 UTC 的独立时钟段、开放确认点、单调时长插值、本地日期裁剪、整分钟向下取整与非法输入。

UUID 测试在两个独立临时库中分别创建 note 和发生包含裁剪的 active_override。UI 发出 annotations.create 后，通过存储快照确认提交，丢弃未读取的回复，再正常关闭并重新启动同目录的真实 CoreRuntime。重试返回原实体；note 仍为 1 行，裁剪场景仍为原左段、写入段、右侧余段共 3 行，余段 UUID 和范围保持一致。

两秒缩短测试使用真实采集/数据库线程、随机名称的 Windows 测试管道和停止读取 state 的 UI 连接，另一个 CLI 连接持续查询 core.status。Private Bytes 在独立测试进程中采样；下表为最终驱动的一次显式运行，报告采样区间为 2.0057458 秒。

| 指标 | 结果 | 断言上限 |
|---|---:|---:|
| 采集提交数 | 335 | 至少 2 次 |
| 采集提交 P95（nearest-rank） | 6.143 ms | < 1,000 ms |
| 状态发布耗时 | 每次通过 | < 100 ms |
| 另一客户端 core.status 往返 | 每次通过 | < 500 ms |
| 连接排队字节峰值 | 12,380 bytes | ≤ 1,048,576 bytes |
| Private Bytes 基线 | 6,615,040 bytes | — |
| Private Bytes 采样峰值 | 6,934,528 bytes | — |
| Private Bytes 增量 | 319,488 bytes | ≤ 34,603,008 bytes（发送预算 + 32 MiB） |

发送预算单独由连接指标检查；进程增量另包含 SQLite 缓存、工作线程、运行时、测试采样与分配器保留空间。十分钟版本使用同一驱动和断言，命令见 [M2 手册第 6 节](acceptance/m2.md#6-十分钟慢客户端)。

release 构建 `cargo.exe build --release -p quartz --target-dir target/windows` 通过。随后在 Windows PowerShell 中按手册覆盖 LOCALAPPDATA，启动独立验收核心（PID 51380）；2026-11-21 18:43（UTC+08:00）的检查执行如下命令，全部通过。

```powershell
& $exe status | Out-Host
deno task acceptance-reject --data-root $dataRoot
deno task acceptance-summary $summaryDump --date $today --no-annotations
deno task acceptance $offlineDump
```

非 Quartz 客户端 PID 3172 连接后无回复断开，同一次连接的核心日志为 `rejected pipe client pid=3172 error=peer executable is not allowed`。真实当日 dump 的摘要计算为 active_ms=0、今日 0h 0m。检查通过 WM_CLOSE 正常停止本次验收核心，然后导出离线事实，format、coverage、idle、midnight、recovery、privacy、clock 全部 PASS。Windows PowerShell 的 quartz.exe 调用使用显式管道等待进程退出，dump 行通过 ForEach-Object 原样传递。

人工待测：托盘各状态的视觉观察、今日提示与脚本输出的分钟比较、暂停满 15 分钟后的恢复、保持计时、隐私模式和长空闲后的快速标注；其他账户/会话拒绝、十分钟慢客户端。语言切换目前由 UI 角色的 settings.set、托盘语言通知及本地化菜单测试覆盖，M3 补实机设置操作。执行步骤及逐项记录要求见 [M2 验收手册](acceptance/m2.md)。

## 2026-11-21 M2-F2：实机验收

HEAD `f23a041`，Windows 11 build 26300，China Standard Time。release 构建，按 [M2 手册](acceptance/m2.md) 在独立 `LOCALAPPDATA`（`target\windows\acceptance-m2-20261121-185128`）启动验收核心 PID 5900，18:51 起。

| 节 | 结果 |
|---|---|
| §2 status / 非 Quartz 客户端 | 两次 `quartz status` 通过；PowerShell PID 38424 无回复断开，日志 `rejected pipe client pid=38424 error=peer executable is not allowed` |
| §3 托盘（用户观察） | 提示两行、菜单各项、暂停 15 分钟（灰色图标与自动恢复提示）后恢复、隐私模式（标记图标、标题隐藏，关闭后恢复）、锁屏后恢复，均符合预期 |
| §4 托盘摘要与 dump | 18:59:33 托盘“今日 7 分钟”，管道 dump 计算 `active_ms=464268` → 0h 7m，一致；第一次采样差 1 分钟为整分钟边界，按手册重采 |
| §6 十分钟慢客户端 | PASS：600.07 s，55,349 次提交，提交 P95 21.593 ms，排队峰值 12,380 bytes，Private Bytes 6,672,384 → 11,689,984（增量 5,017,600，上限 34,603,008） |
| §7 退出与离线不变量 | 托盘“退出 Quartz”正常退出；核心停止后管道 dump 退出码 1 并提示 `--offline`；`dump --offline` 168 行，format、coverage、idle、midnight、recovery、privacy、clock 全部 PASS |

发现：Windows PowerShell 5.1 按 OEM 代码页（中文系统为 GBK）解码 quartz.exe 的 UTF-8 输出，含中文应用名的 dump JSON 首次 89 行中 33 行损坏；quartz 输出本身正确。M1、M2 手册已在导出前设置 `[Console]::OutputEncoding`。

待测：§5 长空闲后的快速标注、§3 保持计时（均需连续无输入，用户本轮未测）；其他账户/会话拒绝（需第二账户）；语言切换实机操作待 M3 设置界面。

## 2026-11-21 M3-A：Mica 呈现（H11）

Windows 11 build 26300.9974，150% 缩放，`AdvancedEffectsEnabled=true`，节电关闭。预览窗口激活、DWM 报告 `DWMSBT_MAINWINDOW`；运行记录确认根层、自绘标题栏、gpui-component `Root` 与 `theme.background` / `title_bar` / `sidebar` 的 alpha 均为 0。用户原壁纸接近中性灰，Mica 与 `bg.base` 像素相同；临时换成 RGB `(20, 80, 180)` 参考壁纸（`deno run … scripts/preview-screenshots.ts --mica-wallpaper reference`，结束后恢复原壁纸）后：

| 采样点 | 参考壁纸、激活 Mica | 强制纯色回退 |
|---|---|---|
| 侧栏、标题栏 | `(240, 243, 249)` | `(243, 243, 243)` = `bg.base` |
| 主区域（`fill.layer`） | `(248, 249, 252)` | `(249, 249, 249)` |
| 卡片（`fill.card`） | `(253, 253, 254)` | `(253, 253, 253)` |

结论：GPUI 的 DirectComposition 路径能在 Mica 上合成，H11 的 Mica 项通过；字体光学尺寸与 `tnum` 待 M3 后续步骤验证。


## 2026-11-22 M3-E/F：常驻界面实测与空闲唤醒调查

用户实机（i7-14700HX、RTX 4060 Laptop 独显直连、165 Hz、150%），release `604da9b`，独立数据目录，界面默认常驻。按命令行区分进程，关窗稳定后采样 30 秒；采样期间未保证无输入。

| 进程 | Private Bytes | 线程 | 30 s CPU | 预算（08） |
|---|---|---|---|---|
| 核心 | 5.5 MiB | 7 | 140 ms | 空闲 ≤ 150 ms / 10 min；活跃 ≤ 3 s / h |
| 隐藏常驻 UI（`Hidden`） | 109.4 MiB | 58 | 78 ms | ≤ 100 MiB；≤ 20 ms / 30 s |

从 `Hidden` 打开到首帧 `click_to_first_frame_ms=106`；独显直连下冷启动约 4 s（M0 spike 同因：D3D11 设备创建）。独显直连时核显不可用，按显卡偏好选择适配器的方案不适用。

M3-F 源码调查（只读，未改代码）结论：

- 可见静态 UI（M2 测得 781 ms / 30 s）：GPUI Windows 后端按刷新率持续唤醒 VSync 线程与 UI 主线程，静态页也执行帧回调；Quartz 代码中没有固定刷新循环。
- 隐藏 UI：最后窗口销毁后平台 D3D 设备与 `VSyncProvider` 仍在，继续按刷新率唤醒；共享 D3D11 context 在窗口销毁时未 `ClearState`/`Flush`，可能保留窗口资源。关窗后未退订 day topic。
- 核心：未见毫秒级轮询，三类主循环期限（逻辑日、预热 TTL、常驻重启）不会退化为零等待。周期工作为 59 s 采集维护、60 s 托盘摘要（另随数据库提交触发，可能相邻两次完整查询）、idle 后每 5 s 检查恢复。140 ms 样本可能含输入驱动的采集，需十分钟无输入复测。

用户决定：隐藏 UI 的 CPU 与内存超出部分暂时接受，功能完整后再优化；暂不对 `gpui-pre-windows` 打补丁，可见静态 UI 的空闲 CPU 偏差保留。候选优化（按需启停 VSync 的依赖补丁、隐藏后进入 Headless 或延迟进入、清理共享 context 绑定、隐藏时退订 day、合并托盘摘要触发）留待届时按实测选择。`scripts/measure.ts` 仍按关窗即退出的生命周期验收，常驻模式下需在 M3-G 更新。

## 2026-11-22：M4-B5 未分类队列查询

本次测量使用 Windows 11 build 26300、Intel Core i7-14700HX 和 Rust 1.100.0，按仓库 release 配置构建。输入为 typical、seed 1、365 日的模拟库，含 365 个采集运行和 626,208 条区间事实。基准在计时外复制源库、安装内置分类与提取规则，先测原始应用名，再将 editor、terminal、player、browser 分别映射为 code.exe、windowsterminal.exe、vlc.exe、msedge.exe 后测量。源库测量前后的 SHA-256 相同。

新增入口为 `bench-query --db <模拟库> --runs 100 --scope unclassified`，位于 `crates/quartz/src/bench_query.rs`。每轮启动独立基准进程，使用 `Process.ProcessorAffinity = [IntPtr]1` 固定到逻辑处理器 0，并核对实际掩码为 1；共 3 轮，每种查询每轮 100 次。进程设置独立的临时 LOCALAPPDATA，所有数据准备均在临时副本中完成。测量期间没有编译任务。每轮包含首次查询，沿用 SQLite 和系统缓存，按 nearest-rank 取排序后第 95 个样本，最终报告三轮 P95 的中位数。

核心系统时区为 China Standard Time（UTC+8），日起点偏移为 0。单日范围为 2025-01-02 至 2025-01-03，七日范围为 2025-01-02 至 2025-01-09，均为左闭右开。搜索使用固定的 `edge`，匹配映射后的未分类浏览器。计时包含快照建立、全范围逐日读取、提取与分类、最终切片、分组、搜索、分页和首个响应体编码。单日派生片段使用完成后即释放，查询内分类及提取缓存继续用于后续日期。

下表为软件名映射副本的耗时，单位为毫秒。

| 分组 / 范围 / 搜索 | 第一轮 P95 | 第二轮 P95 | 第三轮 P95 | P95 中位数 |
| --- | ---: | ---: | ---: | ---: |
| 应用 / 1 日 / 无搜索 | 5.833 | 5.466 | 5.559 | 5.559 |
| 应用 / 1 日 / 搜索 | 5.544 | 5.824 | 5.377 | 5.544 |
| 应用 / 7 日 / 无搜索 | 32.962 | 31.943 | 32.342 | 32.342 |
| 应用 / 7 日 / 搜索 | 34.275 | 31.861 | 31.572 | 31.861 |
| 文档 / 1 日 / 无搜索 | 7.302 | 6.808 | 6.887 | 6.887 |
| 文档 / 1 日 / 搜索 | 6.966 | 6.995 | 7.001 | 6.995 |
| 文档 / 7 日 / 无搜索 | 49.730 | 35.091 | 34.240 | 35.091 |
| 文档 / 7 日 / 搜索 | 53.142 | 36.020 | 35.391 | 36.020 |

映射副本的应用页为 752–753 字节，文档单日页为 176,894 字节，文档七日页为 200,106 字节（无搜索）或 200,122 字节（有搜索），均在 200 KiB 上限内。七日文档列表通过游标继续读取。

验证命令如下，均通过。工作区共 780 项测试通过，原有 2 项测试保持忽略；另运行 `cargo doc -p quartz-protocol --no-deps --target-dir target/windows` 通过。

```text
cargo fmt --all
cargo clippy --workspace --all-targets --all-features --target-dir target/windows -- -D warnings
cargo clippy --workspace --all-targets --target-dir target/windows -- -D warnings
cargo test --workspace --all-features --target-dir target/windows
```

测试证明：无搜索时，应用分组总时长等于最终未分类活跃切片中具有可见应用身份的时长之和；当每个未分类活跃切片都有可见应用时，等于 query.day 的 unclassified_ms。缺少应用身份的离线活动使分组总时长小于日指标；文档分组还会省略缺少文档身份的切片。测试另覆盖最终规则与人工分类、排序与身份同值顺序、样本上限与时序、Unicode 搜索、隐私、完整分页、过期游标、日期上限、夏令时和日起点偏移。

## 2026-11-22：M4-B6 日期范围聚合

本次使用 Intel Core i7-14700HX、Windows 11 build 26300 和 Rust 1.100.0，按仓库 release 配置测量 `query.range`。输入为 typical、seed 1、365 日的模拟库，含 365 个采集运行和 626,208 条区间事实；源库 SHA-256 为 `ea4cb502621474932defd0323b0c630e4bf2fbe278cdb72df03b875af44b1603`。计时外创建独立快照副本、安装内置分类与提取规则，将 editor、terminal、player、browser 分别映射为 code.exe、windowsterminal.exe、vlc.exe、msedge.exe，测量包含实际分类命中的负载。

修复前的耗时由 release 测试 `bench_query::tests::range_benchmark_from_simulation` 执行，`QUARTZ_RANGE_BENCH_DB` 指向模拟库，各轮使用独立的临时 LOCALAPPDATA。每轮启动一个测试进程，以 `Process.ProcessorAffinity = [IntPtr]1` 固定逻辑处理器 0 并验证掩码为 1，共 3 轮，每种查询每轮 100 次；包含首次读取，沿用 SQLite 与系统缓存，按 nearest-rank 取每轮第 95 个样本，最终报告三轮 P95 的中位数。测量进程只运行该测试，未启动核心或界面。

查询覆盖完整请求范围，计时包含快照建立、逐日读取、分类与提取、切片、三维独立聚合、分页选择及第一页响应体编码。各逻辑日使用新的 Pipeline，并复用该查询的分类器；完成当天后释放当天片段。

核心系统时区为 China Standard Time（Asia/Shanghai，UTC+8），日起点为 0 分钟，每周首日为周一。七日范围为 `[2025-01-02, 2025-01-09)`，年度范围为 `[2025-01-02, 2026-01-02)`。源库测量前后的 SHA-256 相同。测量期间未并行执行编译或其他测试。

修复前耗时如下，单位为毫秒。单维度请求为 `[category]`，三维度请求为 `[category, app, document]`。

| 范围 / 桶 | 维度 | 第一轮 P95 | 第二轮 P95 | 第三轮 P95 | P95 中位数 | 预算 | 结果 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| 7 日 / 日桶 | 分类 | 34.107 | 34.246 | 32.137 | 34.107 | < 100 | 通过 |
| 7 日 / 日桶 | 分类、应用、文档 | 42.293 | 43.103 | 43.563 | 43.103 | < 100 | 通过 |
| 7 日 / 周桶 | 分类 | 35.213 | 37.774 | 33.134 | 35.213 | < 100 | 通过 |
| 7 日 / 周桶 | 分类、应用、文档 | 43.143 | 43.606 | 42.669 | 43.143 | < 100 | 通过 |
| 365 日 / 周桶 | 分类 | 1924.823 | 1922.675 | 1901.379 | 1922.675 | < 500 | 超预算 |
| 365 日 / 周桶 | 分类、应用、文档 | 2335.206 | 2323.636 | 2250.855 | 2323.636 | < 500 | 超预算 |
| 365 日 / 日桶 | 分类 | 1922.158 | 1904.365 | 1906.247 | 1906.247 | < 500 | 超预算 |
| 365 日 / 日桶 | 分类、应用、文档 | 2317.684 | 2299.404 | 2285.492 | 2299.404 | < 500 | 超预算 |

修复前年度查询的阶段耗时如下，单位同为毫秒，各列分别取三轮阶段 P95 的中位数。读取列包括查询准备和快照结束；聚合列包含排序、分页选择与候选项编码，编码列为最终响应体序列化。

| 桶 / 维度 | 读取与准备 | 切片 | 分类与提取 | 聚合及候选页准备 | 最终编码 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 周桶 / 分类 | 1231.939 | 210.995 | 358.008 | 17.125 | 0.088 |
| 周桶 / 分类、应用、文档 | 1254.604 | 218.993 | 366.576 | 366.218 | 0.386 |
| 日桶 / 分类 | 1222.182 | 210.143 | 357.847 | 17.652 | 0.346 |
| 日桶 / 分类、应用、文档 | 1254.134 | 219.911 | 367.733 | 350.081 | 0.331 |

查询内状态按日和桶释放：`query/classification.rs` 的提取结果、应用匹配计划、匹配结果和分类结果索引在每天完成后清空；旧实现将它们沿整年扫描保留。`query/range.rs` 在桶完成后立即选入当前响应页并释放该桶分组，跨日仅保留当前桶的聚合、最多 200 KiB 的响应页及日期和版本元数据。文档身份的分组键与值共享同一个 `Arc<DocumentIdentity>`。`store/query.rs` 每日清理当前查询快照拥有的应用字典；SQLite 页缓存保持 `store/raw.rs` 中配置的 2 MiB。

预编译语句复查使用同一读取连接，移除每日缓存重置后，临时记录 Diesel 的 `CacheQuery` 事件：1 日、7 日、365 日和重复 365 日查询累计均为 11 条，每条只进入缓存一次。这 11 条分别读取快照锚点、设置、分类、日期版本、时钟与采集运行、前台闭合区间、前台开放区间、可用性闭合区间、可用性开放区间、采集缺口和批注。可变长度的应用 `eq_any` 查询由 Diesel 标记为不缓存；四种 boxed 区间扫描生成的 SQL 文本固定。年度扫描没有造成语句缓存增长，临时记录已移除。

应用查找现按 `APPLICATION_LOOKUP_ARITY = 8` 分块，始终使用同一条含 8 个等值条件的语句；不足一块时重复该块已有的 ID。四种区间扫描改用静态查询类型，保留原有筛选、排序和读取上限；每日切换不再重置连接的语句缓存。回归测试在同一读取连接上依次查找 1–256 个应用，再执行 64 轮不同日期、端点及范围的 `query.day`、`query.timeline`、`query.range`，断言应用语句只缓存一次、参数数固定为 8，预热后缓存插入次数保持不变。

按日和桶释放查询状态后的三轮测量为每个场景、每轮启动一个新进程，共 24 个进程，均核对 CPU 0 亲和性掩码为 1。通过 `QUARTZ_RANGE_BENCH_DB` 指定模拟库，`QUARTZ_RANGE_BENCH_SCENARIO` 指定下表对应的 `pipeline.range.<7|365>.<day|week>.<category|all>` 场景。七日场景每轮 100 次计时，年度场景每轮 10 次计时，各测 3 轮；年度 10 次的 nearest-rank P95 为该轮最大值。每个进程先执行一次不计时的同范围查询，采集查询内保留状态，再执行计时样本。

内存基线在规则快照准备完成、首次查询开始前读取。查询内每次 `QueryJob::advance` 返回后、响应编码后及快照结束后调用 `GetProcessMemoryInfo`；外部采样器固定在 CPU 2，从基线标记开始，每次等待 5 ms 后再次读取同一进程的 PrivateUsage。两路峰值与计时样本前后读数取最大值，再减去该进程自己的基线。七日每进程记录 411 次内部观测，年度记录 10,821 次；外部观测另行加入峰值。

按日和桶释放查询状态后的三轮耗时如下，单位为毫秒。

| 范围 / 桶 / 维度 | 每轮计时次数 | 第一轮 P95 | 第二轮 P95 | 第三轮 P95 | P95 中位数 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 7 日 / 日桶 / 分类 | 100 | 34.374 | 35.774 | 35.195 | 35.195 |
| 7 日 / 日桶 / 分类、应用、文档 | 100 | 41.936 | 41.517 | 42.436 | 41.936 |
| 7 日 / 周桶 / 分类 | 100 | 34.318 | 34.601 | 35.416 | 34.601 |
| 7 日 / 周桶 / 分类、应用、文档 | 100 | 43.163 | 41.790 | 43.315 | 43.163 |
| 365 日 / 周桶 / 分类 | 10 | 1804.112 | 1809.842 | 1820.550 | 1809.842 |
| 365 日 / 周桶 / 分类、应用、文档 | 10 | 2138.233 | 2111.954 | 2168.987 | 2138.233 |
| 365 日 / 日桶 / 分类 | 10 | 1861.592 | 1805.386 | 1848.431 | 1848.431 |
| 365 日 / 日桶 / 分类、应用、文档 | 10 | 2069.344 | 2052.573 | 2127.541 | 2069.344 |

按日和桶释放查询状态后的年度阶段耗时如下，各列分别取三轮阶段 P95 的中位数，单位为毫秒。

| 桶 / 维度 | 读取与准备 | 切片 | 分类与提取 | 聚合及候选页准备 | 最终编码 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 周桶 / 分类 | 1272.816 | 170.257 | 290.211 | 15.925 | 0.060 |
| 周桶 / 分类、应用、文档 | 1302.822 | 176.201 | 293.256 | 302.424 | 0.299 |
| 日桶 / 分类 | 1306.592 | 169.963 | 292.464 | 17.549 | 0.246 |
| 日桶 / 分类、应用、文档 | 1283.194 | 169.756 | 290.534 | 265.825 | 0.230 |

每个场景的 PrivateUsage 峰值增量如下，单位为 MiB，预算为 ≤ 20 MiB。各单元格均来自该场景独立进程的查询前基线和查询期间峰值。

| 范围 / 桶 / 维度 | 第一轮 | 第二轮 | 第三轮 | 三轮最大值 |
| --- | ---: | ---: | ---: | ---: |
| 7 日 / 日桶 / 分类 | 1.672 | 1.527 | 1.398 | 1.672 |
| 7 日 / 日桶 / 分类、应用、文档 | 1.945 | 2.668 | 1.957 | 2.668 |
| 7 日 / 周桶 / 分类 | 1.742 | 1.699 | 1.426 | 1.742 |
| 7 日 / 周桶 / 分类、应用、文档 | 3.746 | 2.996 | 3.344 | 3.746 |
| 365 日 / 周桶 / 分类 | 1.559 | 1.379 | 1.688 | 1.688 |
| 365 日 / 周桶 / 分类、应用、文档 | 3.297 | 3.645 | 3.656 | 3.656 |
| 365 日 / 日桶 / 分类 | 1.137 | 1.516 | 1.828 | 1.828 |
| 365 日 / 日桶 / 分类、应用、文档 | 2.270 | 2.277 | 2.121 | 2.277 |

七日各项 P95 相对修复前的变化均在约 1.2 ms 以内；年度各项 P95 减少约 58–230 ms。日清理带来的跨日命中损失已包含在这些耗时内。

固定应用查找参数数目、改用静态扫描类型并移除语句缓存重置后，对两个年度场景各启动一个新进程，固定 CPU 0，测量 1 轮，每项 10 次计时，另含一次不计时的查询内峰值扫描。模拟库、日历设置和内外两路内存采样方法沿用上述设置，每个进程记录 10,821 次内部观测；源库测量前后的 SHA-256 相同。10 次样本的 nearest-rank P95 为最大值，以下结果为单轮复测值。

| 范围 / 桶 / 维度 | 此前三轮 P95 中位数（ms） | 本轮 P50（ms） | 本轮 P95（ms） | 耗时预算（ms） | 结果 |
| --- | ---: | ---: | ---: | ---: | --- |
| 365 日 / 周桶 / 分类、应用、文档 | 2138.233 | 2033.950 | 2091.760 | < 500 | 超预算 |
| 365 日 / 日桶 / 分类 | 1848.431 | 1694.348 | 1728.750 | < 500 | 超预算 |

本轮 PrivateUsage 峰值增量如下，单位为 MiB。峰值取查询内观测、外部采样及计时样本前后读数中的最大值。

| 范围 / 桶 / 维度 | 查询前基线 | 查询期间峰值 | 峰值增量 | 预算 | 结果 |
| --- | ---: | ---: | ---: | ---: | --- |
| 365 日 / 周桶 / 分类、应用、文档 | 11.141 | 14.543 | 3.402 | ≤ 20 | 通过 |
| 365 日 / 日桶 / 分类 | 11.348 | 12.672 | 1.324 | ≤ 20 | 通过 |

本轮阶段 P95 如下，单位为毫秒，各列独立取样。

| 桶 / 维度 | 读取与准备 | 切片 | 分类与提取 | 聚合及候选页准备 | 最终编码 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 周桶 / 分类、应用、文档 | 1229.344 | 183.833 | 303.165 | 313.841 | 0.214 |
| 日桶 / 分类 | 1181.129 | 171.807 | 301.210 | 18.220 | 0.214 |

本轮两项 P95 相对此前三轮中位数分别减少 46.473 ms 和 119.681 ms。

八个场景的查询内存增量均满足 20 MiB 预算。七日耗时满足 100 ms 预算；年度耗时仍超过 500 ms，读取仍是最大开销。年度三维请求超过 `quartz-pipe` 的 2 秒响应期限，客户端可能超时。响应期限保持 2 秒；本次未添加 F4/F5 跨查询缓存。

七日首个响应体为 1,590–200,521 字节，年度首个响应体为 36,118–200,521 字节，均在 200 KiB 页预算内。

验证命令 `cargo fmt --all`、工作区全特性及默认特性的两次 Clippy（`--all-targets -- -D warnings`）、`cargo test --workspace --all-features --target-dir target/windows` 和 `cargo doc -p quartz-protocol --no-deps --target-dir target/windows` 均通过。工作区 792 项测试通过，3 项忽略，其中 1 项为本节显式执行的范围基准，另 2 项为原有忽略测试。测试覆盖三维与逐日指标和排行的等价性、周日首日与首尾裁剪、夏令时及 240 分钟日起点、重复 UTC 和单调时长变化、离线活动、隐私、分页、过期游标、UI 与 CLI 管道读取，以及 366 个逻辑日跨夏令时的文档引用回传。新增测试检查日切换释放提取结果、匹配计划和分类结果，全年扫描时已完成桶释放文档身份，以及跨桶分页总时长只返回一次。

文档引用的正时长 UTC 上限统一为 367 × 24 小时，日期范围沿用 1–366 个逻辑日。新增能力 `query.range.v1`；协议版本和查询语义版本分别保持 1 和 6。08 的基准说明位于第 2.1 节，本次在该节补充范围查询入口与测量方法。

## 时钟重叠扫描的索引测量（2026-11-23）

运行 `cargo.exe test -p quartz --all-features --target-dir target/windows clock_overlap_history_plan_and_index_benchmark -- --ignored --nocapture`。测试在两个临时数据库中分别保留无索引状态和创建 `(start_utc_ms, id)` 索引，每库写入 100,000 条一小时的闭合时钟段，按生产读取路径逐逻辑日重复重叠扫描。扫描耗时取九次中位数，批量插入耗时各测一次；采用 Windows 开发测试构建。

| 操作 | 无索引 | 开始时间与 ID 索引 |
|---|---:|---:|
| 插入 100,000 条 | 71.09 ms | 287.76 ms |
| 1 日扫描 | 8.59 ms | 33.75 ms |
| 7 日扫描 | 59.03 ms | 235.57 ms |
| 366 日扫描 | 3,093.23 ms | 11,913.44 ms |

无索引计划为 `SCAN clock_segments` 加 `USE TEMP B-TREE FOR ORDER BY`。候选索引计划为 `MULTI-INDEX OR`，两个分支分别搜索开始时间范围和相同开始时间下的 ID，仍需 `USE TEMP B-TREE FOR ORDER BY`。本次候选索引使扫描和写入都变慢，因此保留现有 schema。历史重复扫描的成本需要结合范围谓词、分页和索引共同优化；上表测量的是时钟读取阶段，完整查询的 P95 目标见 08 第 1.4 节。

## 2026-11-27 M4 收尾：查询基准与资源测量（含 M3 常驻 / 按需重测）

用户实机空闲时段（i7-14700HX、RTX 4060 Laptop 独显直连，接电），release `44cd144`，独立数据目录（`target/windows/m4-auto/`）。

### 查询（A3，typical 一年模拟库，99.6 MB，`bench-query`）

| 指标 | 实测 P95 | 预算（08 §1.4） | 结果 |
|---|---:|---:|---|
| `query.day`（实际分类负载） | 19.1 ms | < 100 ms | 通过 |
| `query.timeline` 单日 | 30.8 ms | — | — |
| `query.range` 7 日（日桶 / 周桶，分类或三维） | 27.6–39.8 ms | < 100 ms | 通过 |
| 七次 `query.day` 周折叠对照 | 108.0–114.1 ms | 对照项 | 周视图走 `query.range` |
| `query.timeline` 七日 | 111.3–118.5 ms | — | — |
| 文档实体详情（七日） | 80.2 ms | — | — |
| `query.range` 365 日（日桶 / 周桶，分类或三维，`--runs 10`） | 1455.7–1964.2 ms；首个冷读 4011.4 ms | < 500 ms | 超预算 |
| 查询期间核心 PrivateUsage 峰值增量 | ≤ 12.4 MiB | ≤ 20 MiB | 通过 |

年度耗时以读取全年 62.7 万行事实为主（约 1.1 s），切片与分类各约 160 ms。结论同 M4-B6：不加日结果缓存时年度预算不可达；三维年度请求 P95 接近管道 2 秒响应期限。跨查询 / 日结果缓存在 M4 收缩时推迟，2026-11-27 用户决定放宽年度预算为 P95 ≤ 2.5 s，并要求界面显示多日查询进度（进度帧同时重置管道响应期限）。

### 资源（`m4-auto/run.ps1 -Steps core-only, resident, on-demand`，稳定 30 s）

| 指标 | 实测 | 预算（08） | 结果 |
|---|---|---|---|
| 核心 10 分钟空闲 Private Bytes 中位 | 7.05 MiB | 8–12（上限 16） | 通过 |
| 核心 Private WS / 线程 | 6.1 MiB / 6 | ≤ 12 MiB / ≤ 8 | 通过 |
| 核心 10 分钟空闲 CPU | 218.8 ms（按需轮次 125.0 ms） | ≤ 150（上限 300） | 首轮超目标、在上限内；未保证全程无输入 |
| 界面开关 20 次后核心增量 | 常驻 0.02 MiB；按需 0.46 MiB | ≤ 1 MiB | 通过 |
| 按需 20 次后核心句柄 | +20 | 脚本要求不增 | 见下 |
| 常驻：点击到首帧 | P50 34 ms，P95 48 ms | P50 ≤ 1 s | 通过 |
| 按需：冷启动到首帧 | P50 506 ms，P95 4159 ms，最大 4675 ms | ≤ 4.5 s（当前机器） | P95 通过，最大值略超 |
| 隐藏常驻 UI（从未打开 / 关窗后） | 57.3 / 109.9 MiB | ≤ 100 MiB | 关窗后超，已接受偏差 |
| 打开状态 UI Private Bytes | 常驻 159.8–168.6 MiB；按需 116.5–116.9 MiB | ≤ 100 MiB | 超（M3-E 已接受，功能完整后优化） |
| 静止 UI 30 s CPU | 343.8–656.3 ms | ≤ 20 ms | 超（GPUI VSync，已接受偏差） |
| 隐藏常驻 UI 30 s CPU | 46.9–156.3 ms | ≤ 20 ms | 超（已接受偏差） |
| 核心 `nv*.dll` | 无 | 不加载 | 通过 |

按需模式的句柄增长判为脚本误报：每次打开都是新的 UI 进程并成为前台窗口，前台身份解析的进程缓存（`PROCESS_CACHE_CAPACITY` = 128）为其保留一个查询句柄，已退出进程在下一次复查时逐出；常驻模式 PID 不变，句柄保持 229。缓存有界，属采集代码，未改。`measure.ts` 的按需句柄判定未相应放宽。

打开状态 UI 内存较 M3-E 上升（常驻 ~160 MiB，按需 ~117 MiB），包含 M4 规则页、编辑器与标注界面；与 M3-E 一致，按用户决定在功能完整后统一优化。原始报告：`target/windows/m4-auto/run-20261127-101310/`，查询日志：`target/windows/m4-auto/bench-*.log`。

## 2026-11-27 M6 依赖审计（不联网）

- `cargo tree -e normal,build,dev --all-features`，目标 `x86_64-pc-windows-msvc` 与 `x86_64-unknown-linux-gnu`：`hyper`、`h2`、`rustls`、`quinn`、`reqwest`、`ureq`、`curl`、`openssl`、`native-tls`、`tungstenite` 均不在依赖图中（`Cargo.lock` 中出现的条目只属于其他平台）。
- 唯一进入构建的 HTTP 相关 crate 是 GPUI 的 `gpui-pre-http-client`（`http`、`url` 等类型与 trait），不含传输实现；Quartz 不安装 HTTP 客户端。
- tokio 的 `net` 特性只用于命名管道；源码中没有 `TcpStream`、`UdpSocket`、`TcpListener`、WinHTTP 或 WinINet 调用。
- `deny.toml` 的 `[bans].deny` 补充 `rustls`、`quinn`、`h2`、`tungstenite`，由 CI 的 `cargo deny check` 阻止今后引入。

## 2026-11-27 M6 验收：08 指标复测与 8 小时运行

用户实机空闲时段（i7-14700HX、RTX 4060 Laptop，接电），release `66deb3c`，独立数据目录（`target/windows/m6-accept/`，`run.ps1` 由 M4 模板改写，增加 `soak` 与 `reopen` 步骤）。

### 查询（typical 一年模拟库，99.6 MB，`bench-query`）

| 指标 | 实测 P95 | 预算 | 结果 |
|---|---:|---:|---|
| `query.day`（实际分类负载） | 6.2–8.7 ms | < 100 ms | 通过 |
| `query.timeline` 单日 / 七日 | 9.8–12.8 / 44.5–48.3 ms | — | — |
| 七次 `query.day` 周折叠对照 | 43.5–46.3 ms | 对照项 | — |
| 文档实体详情（七日） | 31.7–33.2 ms | — | — |
| `query.range` 7 日（日桶 / 周桶，分类或三维） | 28.3–40.2 ms | < 100 ms | 通过 |
| `query.range` 365 日（`--runs 10`） | 1540.7–2195.7 ms | ≤ 2.5 s（2026-11-27 放宽） | 通过 |
| 查询期间核心 PrivateUsage 峰值增量 | 除下项外 ≤ 12.6 MiB；365 日按日三维 26.4–27.4 MiB | ≤ 20 MiB | 年度三维超（新记录；M4 表未含 range 年度项） |

### 资源（`run.ps1 -Steps core-only, resident, on-demand`，稳定 30 s）

| 指标 | 实测 | 预算（08） | 结果 |
|---|---|---|---|
| 核心 10 分钟空闲 Private Bytes 中位 | 7.12–7.19 MiB | 8–12（上限 16） | 通过 |
| 核心 Private WS / 线程 | 6.2 MiB / 6（峰值 9） | ≤ 12 MiB / ≤ 8 | 通过（峰值瞬时） |
| 核心 10 分钟空闲 CPU | 78.1–125.0 ms | ≤ 150（上限 300） | 通过 |
| 界面开关 20 次后核心增量 | 常驻 0.02 MiB；按需 0.30 MiB | ≤ 1 MiB | 通过 |
| 按需 20 次后核心句柄 | +20 | 脚本要求不增 | 同 M4，判为脚本误报（前台进程缓存，有界） |
| 常驻：点击到首帧 | P50 34 ms，P95 90 ms | P50 ≤ 1 s | 通过 |
| 按需：冷启动到首帧 | P50 548 ms，P95 4196 ms，最大 4878 ms | ≤ 4.5 s | P95 通过，最大值略超（同 M4） |
| 隐藏常驻 UI（从未打开 / 首次关窗后） | 56.8 / 83.5 MiB | ≤ 100 MiB | 通过（M4 关窗后 109.9） |
| 打开状态 UI Private Bytes | 常驻 109.1–109.7 MiB；按需 90.2–90.4 MiB | ≤ 100 MiB | 常驻略超，按需通过（M4 为 160 / 117） |
| 静止 UI 30 s CPU | 296.9–875.0 ms | ≤ 20 ms | 超（GPUI VSync，已接受偏差） |
| 隐藏常驻 UI 30 s CPU | 46.9–93.8 ms | ≤ 20 ms | 超（已接受偏差） |
| 核心 `nv*.dll` | 无 | 不加载 | 通过 |

### 8 小时运行（`run.ps1 -Steps soak`，核心 + 常驻 UI，每 5 分钟采样，每 60 分钟经 `quartz.exe ui` 打开一次并关窗）

- 8.07 小时，96 次采样，`quartz.exe status` 全部成功；核心与 UI 日志无 ERROR。
- 核心 Private Bytes 7.46–8.36 MiB（中位 7.75），无增长趋势；CPU 合计 5.8 s（约 121 ms / 10 分钟）；线程 ≤ 10；句柄 229 → 262（增量出现在同期两次预览截图之后，为前台进程缓存，有界）。
- **常驻 UI 泄漏**：每次打开再关窗，隐藏 UI 的 Private Bytes 增加 23–27 MiB 且不回落（57 → 84 → 111 → 134 → 156 → 180 → 203 → 226 → 249 MiB，8 次）。同期核心日志每次都有 `could not grant UI foreground permission`（转发进程持有前台，核心授权失败），已由 `47bdf02` 修复；修复后复测（`-Steps reopen -ExeDir bin-fix`，5 次）授权失败为 0，但每次仍增加约 25 MiB（57 → 83 → 108 → 132 → 155 → 180 MiB）。结论：每次显示新建、关窗销毁 GPUI 窗口时约 25 MiB 未释放，与前台无关；待定位修复（M6 未关闭项）。

原始数据：`target/windows/m6-accept/run-20261127-235421/`（core-only）、`run-20261128-000548/`（resident）、`run-20261128-002344/`（on-demand）、`run-20261128-004800/soak.csv`、`run-20261128-085319/reopen.csv`，查询日志 `bench-*.log`；截图 `target/m3-screenshots/`（week、week-loading、settings*，三语言 × 五种外观，共 106 张，已人工检查）。
