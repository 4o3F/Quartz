# 08 资源预算与验收

## 1. 预算

以下都在 Windows release 构建、实机（i7-14700HX、RTX 4060 Laptop、MUX 独显直连）上测量。

### 1.1 核心（界面关闭、无浏览器连接、运行 10 分钟后）

| 指标 | 目标 | 上限 | 说明 |
|---|---|---|---|
| Private Bytes | 8–12 MiB | 16 MiB | `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage` |
| Private Working Set | ≤ 12 MiB | 16 MiB | 任务管理器"内存"列。对照 clash-nyanpasu 关窗后的 14.8 MB |
| 线程数 | ≤ 8 | 12 | 包括系统和依赖创建的线程 |
| 无事件时的定时唤醒 | ≤ 0.1 次/秒 | 0.25 次/秒 | 用 ETW 统计，不能靠数代码里的 sleep |
| 空闲 CPU | 10 分钟 ≤ 150 ms | 300 ms | |
| 活跃使用时 CPU | 正常办公 1 小时 ≤ 3 s | 10 s | 包含标题变化频繁的场景 |
| 界面开关 20 次后的核心增量 | ≤ 1 MiB | 2 MiB | 检查是否持续增长 |
| GPU | 不创建 D3D 设备 | — | 核心和 host 都不能加载 NVIDIA 用户态驱动（`nvwgf2umx.dll` 等） |

**关于同一 exe 静态导入 D3D 的影响**（用户决策 5）：`d3d11.dll`、`dxgi.dll`、`dcomp.dll`、`dwrite.dll` 会在核心进程里加载，但只要不创建设备，NVIDIA 用户态驱动就不应加载。M0 测量这一项。静态导入造成的超预算记录偏差，作为后续优化，不阻塞里程碑。后续可评估 `/DELAYLOAD` 这些 DLL，再考虑拆成 `quartz.exe` 和 `quartz-ui.exe`。

### 1.2 host（`quartz-host.exe`，每个浏览器连接一个）

独立的 console 子系统程序，不链接 GPUI，也不创建 GPU 设备。

| 指标 | 上限 |
|---|---|
| Private Bytes | 4 MiB |
| 线程数 | 4 |
| 空闲 CPU | 10 分钟 ≤ 50 ms |

### 1.3 界面

| 指标 | 目标 |
|---|---|
| 打开状态 Private Bytes | ≤ 100 MiB（spike 为 84 MB） |
| 冷启动到首帧 | 当前机器接受 ≤ 4.5 s；常驻隐藏或有预热时，用户点击到首帧 P50 ≤ 1 s |
| 热启动到首帧 | ≤ 0.8 s |
| 静止时 CPU | 30 秒内 ≤ 20 ms |
| 时间轴拖动 | 全屏下 ≥ 60 fps |
| 隐藏常驻 UI Private Bytes | ≤ 100 MiB；在 M3-G 测量 |
| 隐藏常驻 UI 空闲 CPU | 30 秒内 ≤ 20 ms，与静止界面相同；待 M3-F 优化，在 M3-G 测量 |
| 关闭常驻后的关窗 | 进程在 1 秒内退出 |

### 1.4 数据与查询

| 指标 | 目标 |
|---|---|
| typical 一年模拟数据（工作日 2,000 个切片、550 个新标题，周末减半）的日查询 P95 | < 100 ms |
| 周查询 P95 | < 100 ms |
| 年度汇总 | P95 ≤ 2.5 s（2026-11-27 用户放宽，原 < 500 ms；冷读首次可达 4 s）；超过 250 ms 的多日查询须在界面显示按日进度 |
| 查询期间核心 Private Bytes 峰值 | 比空闲时多 ≤ 20 MiB；日结果缓存总计 ≤ 8 MiB，单日超过 2 MiB 不缓存 |
| 写入延迟（策略允许输出后到事务提交） | P95 < 1.5 s；网页隐私合流另有最多 2 秒等待 |
| 崩溃丢失 | ≤ 60 s |

## 2. 测量方法

### 2.1 工具

- `deno task measure --Exe ./target/release/quartz.exe --SettleSeconds 30 --IdleSeconds 600`：要求 Deno 2.9.8 与 Windows 桌面会话，先稳定再采样核心及界面进程，输出 Private Bytes、Private WS、线程数、句柄数、CPU 时间，并报告两个可执行文件的体积。`--ScreenshotPath <文件.png>` 可保存界面截图；稳定与空闲秒数均允许 1–3600。任务显式授予 FFI、进程启动和文件读写权限。
- 通过 Deno FFI 调用 `GetProcessMemoryInfo`、`GetProcessTimes`、`GetProcessHandleCount` 与进程快照接口采样。WMI 只在采样前读取机器信息。
- `EmptyWorkingSet` 不作为优化手段，也不在验收时调用。
- 模块检查：测量任务通过 `EnumProcessModulesEx` 列出核心与界面加载的模块，检查核心没有 `nv*.dll`。
- 唤醒和线程活动：用 WPR 的 CPU 和 Generic Events profile 录制 10 分钟空闲，在 WPA 里看各线程的唤醒次数和原因。
- 查询性能：M1 使用 `quartz gen-data --db <新文件> [--profile light|typical|heavy] [--seed N] [--days N]` 生成模拟数据，参数见 03 第 4.6 节；`quartz bench-query --db <模拟库> [--runs N]` 从最早运行的开始时间分别读取 1、7、365 天内的前台、可用状态和缺口。各范围默认 100 次，包含首次读取，输出按排序后第 `ceil(次数 × 百分位)` 个样本计算的 P50、P95、最大耗时及各类返回行数，计时包含取出事实行和构造读取结果。已结束前台与可用状态使用 `start_utc_ms >= from - 86_400_000` 的索引条件，开放行和缺口使用完整相交条件；连接为只读，每次读取使用一个快照事务。`bench-query` 另外测量 `query.day`、应用细节的日与七日 `query.timeline`，使用同一只读快照管线，包含切片、聚合、分页选择和第一页编码；时间轴采用 60,000 毫秒/像素。逻辑日取最早运行所在的当前系统本地日期，沿用 `day.start_offset_min`；后续页使用同样的管线重新计算并检查版本。Windows 基准在独立的临时快照副本上安装内置分类与规则集，先测量原模拟应用，再把 `editor`、`terminal`、`player`、`browser` 分别映射为 `code.exe`、`windowsterminal.exe`、`vlc.exe`、`msedge.exe`，测量包含实际分类命中的负载，最后运行原始事实读取。两组管线共用首次查询前的 `PrivateUsage` 空闲基线；每次查询前后用 `GetProcessMemoryInfo` 采样，输出采样峰值及增量。副本准备、内置数据安装和应用映射不计入查询耗时。`--scope pipeline` 只测量查询管线；`--input prepared` 只读复用已安装内置分类规则的模拟库，供版本间在同一文件上比较。管线分别输出读取、边界扫描与切片、分类、聚合与合并、分页与编码的耗时。另保留连续七个逻辑日的 `query.day` 作为周折叠对照，输出 `pipeline.week.fold`；该测量包含七次日查询与首个响应页编码，日请求各自重新构建查询内缓存。`--scope range` 使用 `query.range` 测量 7 日的日桶与周桶，以及 365 日的周桶与日桶；每种分别请求 `[category]` 和 `[category, app, document]`，计时包含逐日读取、查询内缓存、分页选择和第一页编码；确定下一页位置后停止后续日期读取，版本绑定完整请求范围。Windows 可通过 release 测试 `bench_query::tests::range_benchmark_from_simulation` 执行同一测量函数，环境变量 `QUARTZ_RANGE_BENCH_DB` 指向模拟库，`QUARTZ_RANGE_BENCH_SCENARIO` 选择单个场景。每个场景、每轮启动一个新测试进程并固定逻辑处理器 0，共 3 轮；七日每轮 100 次，年度每轮 10 次，报告三轮 P95 的中位数。先用一次不计时查询在各有界步骤后采样 PrivateUsage，外部同时采样进程 PrivateUsage；以首个查询前的值为基线，记录场景内的最大增量。其他平台输出 `not_measured`。M4 验收完整管线。 M1-E1 的 typical 档 30 日实测结果见 [2026-11-18 测量记录](../measurements.md)。

### 2.2 场景

| 场景 | 时长 | 关注 |
|---|---|---|
| 空闲 | 10 分钟不操作，屏幕不锁 | 唤醒次数、CPU |
| 正常办公 | 1 小时脚本化切换应用和标签 | CPU、写入延迟、内存稳定 |
| 高频与长标题 | 一年模拟数据含标题风暴、4096 UTF-16 单元标题、多网页切片 | 查询 P95、8 MiB 日缓存预算、单日超 2 MiB 时分页计算的峰值内存 |
| 写入故障 | 注入数据库错误与队列过载后恢复 | 托盘异常、10 分钟内存保留上限、capture_gaps 边界 |
| 长时间运行 | 8 小时 | 内存是否增长、句柄是否泄漏 |
| 锁屏与睡眠 | 各 10 次 | 恢复后状态正确、无多余区间 |
| 界面开关 | 20 次 | 核心增量、界面进程是否都已退出 |
| 浏览器 | 3 个浏览器同时连接 1 小时 | host 资源、核心资源 |

### 2.3 回归

每个里程碑结束时跑一次 2.2 的空闲和界面开关场景，把结果追加到 `docs/measurements.md`，记录日期、提交和机器状态。超出上限的指标要么修复，要么写明原因并由用户确认后调整预算。静态导入 D3D 的专项偏差按第 1.1 节记录，不阻塞里程碑。
