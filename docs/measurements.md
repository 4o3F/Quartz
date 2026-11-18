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
