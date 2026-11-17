# 资源测量

## 2026-11-16 M0-B2：无系统标题栏的空白 GPUI 窗口与独立 UI 进程

构建基于 `55dfa4f1fa218cad5e97787394900e0d7b659163`（HEAD）加 `uncommitted M0-B2 working tree`。使用 Rust 1.100.0、`gpui-kit = "=0.10.0"`，关闭默认功能，启用 `component` 与 `assets`，通过 `icon_assets!` 仅嵌入最小化、最大化、还原和关闭四个图标；使用仓库的 release 配置和静态 C 运行库。没有引入 spike 的 WARP 补丁。

UI 使用 gpui-component `TitleBar` 与 `TitleBar::window_options()`，通过 `appears_transparent` 隐藏系统标题栏。960 × 640 逻辑像素的窗口居中打开，32 px 自绘标题栏与内容区共用 `bg.base` 背景。标题采用本地化应用名称、`type.caption` 的 12/16 px Regular 与 Segoe UI Variable Small，左侧间距为 `space.m`（12 px）；令牌和亮暗主题位于嵌入的 `crates/quartz-ui/themes/fluent.json`。标题使用组件 `Label`，布局方向按 Quartz 已协商的语言设置；启动与系统外观变更时同步主题。拖动区域与窗口按钮复用 GPUI 原生命中区域，最后一个窗口关闭后调用 `cx.quit()`。M0 的 `ui --prewarm` 成功退出且不开窗；D3D 预热、握手与生存时间控制保留为 `TODO(M3)`。

### 测量方法

在 Windows 桌面会话中执行以下命令。核心稳定 30 秒后记录 CPU 起点，空闲 600 秒后采样；UI 稳定 30 秒后采集 30 秒空闲 CPU。随后通过向 `Quartz.Core` 隐藏窗口投递 `AppMessage::TrayCallback`（`WM_APP + 1`），在 `lParam` 低 16 位传入 `NIN_SELECT`，打开并用 `WM_CLOSE` 关闭 UI 共 20 次。每次等待 UI 退出并确认没有剩余 UI 子进程，最后再稳定 30 秒并测量 30 秒核心空闲 CPU。表格同时记录初始空闲、首次 UI 关闭后及每 5 次循环的核心指标；20 次循环的增量以首次 UI 关闭后为起点，首次 UI 带来的增量单独列出。

```powershell
./scripts/measure.ps1 -Exe ./target/release/quartz.exe -SettleSeconds 30 -IdleSeconds 600
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
| Windows `scripts/measure.ps1 -Exe ./target/release/quartz.exe -SettleSeconds 30 -IdleSeconds 600` | 运行完成，退出码 0；预算偏差见测量结果 |
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
