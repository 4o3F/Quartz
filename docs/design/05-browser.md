# 05 浏览器接入

浏览器扩展上报当前活动标签。网站使用时长由核心与前台事实求交得到，扩展不判断空闲和焦点时长。

```text
扩展 ──connectNative──▶ quartz-host.exe ──命名管道──▶ 核心
```

**2026-11-27 精简（YAGNI）**：网站时长只在与前台事实相交时累计，而前台事实只在核心运行时产生，因此扩展端不做持久缓冲、重放、epoch/seq、缺口、回执去重、`browser.policy` 与 `purge` 下发，也不做 popup 今日时长。扩展始终发送完整的当前状态；断线后重连再发一次完整状态即可恢复。隐私规则只在核心收到消息时执行。`ingest_receipts` 表保留但不使用。

## 1. 扩展

### 1.1 范围

- Chrome、Edge 使用同一份 MV3 service worker 代码；Firefox 使用单独的 manifest 与 event page，共用同一份逻辑模块。
- 最低版本 Chrome/Edge 120、Firefox 128。
- 权限：`tabs`、`nativeMessaging`、`alarms`、`storage`。不申请 `<all_urls>`，不注入内容脚本。
- manifest 设 `incognito: "not_allowed"`，扩展不进入无痕窗口（06 第 3.4 节）。
- Chrome/Edge 的 manifest 带 `key` 固定扩展 ID（`hmhnlfjafbcngnjgafhphcmljgldbakm`）；Firefox 用 `browser_specific_settings.gecko.id` 固定 ID（`quartz@4o3f.dev`）。
- `storage.local` 只保存首次运行随机生成的 `profile_instance_id`，不存任何浏览内容。

### 1.2 握手与上报

连接后第一条消息为 `browser.hello { profile_instance_id, browser: chrome|edge|firefox, proto {min,max} }`，随后立即发送一条 `browser.state`。

扩展监听 `tabs.onActivated`、`tabs.onUpdated`（URL、标题、audible）、`tabs.onRemoved`、`windows.onFocusChanged`、`windows.onRemoved`，任一变化都重新读取并发送完整状态：

```json
{ "t": "req", "id": 2, "op": "browser.state", "body": {
    "focused": true,
    "window": { "id": 7, "state": "normal" },
    "tab": { "id": 91, "domain": "github.com", "title": "4o3F/quartz",
             "title_status": "ok", "audible": false }
} }
```

- 完整 URL 不离开扩展：发送前只取域名；内部页与本地文件只报 `(internal)`、`(local-file)`。
- 失焦时 `focused: false`、`tab: null`；最小化时 `window.state = minimized`。
- 标题上限 4,096 个 UTF-16 单元，超限截断并设 `title_status = truncated`。
- 状态时间以核心收到消息的时刻为准，扩展不带时间戳。
- 内容未变化的状态不重复发送；不做定时心跳。

### 1.3 重连与状态展示

- host 因核心离线退出或端口断开时，badge 显示 `!`；host 未注册（`connectNative` 立即失败）显示 `?`；已连接时 badge 为空。
- 重连：后台存活时按 1 秒起步、60 秒封顶的指数退避；同时注册 1 分钟的 `alarms` 作为后台被回收后的唤醒。后台启动时同步注册全部监听器并立即尝试连接。任何标签/窗口事件也会在未连接时触发一次连接尝试。

## 2. Native Messaging host

### 2.1 注册

核心在每次启动时（数据目录与命名管道就绪后，在后台线程执行，不阻塞主线程或管道就绪）自动、幂等地为 Chrome、Edge、Firefox 注册 Native Messaging host：在 `%LOCALAPPDATA%\Quartz\native-hosts\` 写出 manifest，并写入当前用户 HKCU 注册表键：

| 浏览器 | 注册表键（默认值为 manifest 绝对路径） | 授权字段 |
|---|---|---|
| Chrome | `HKCU\Software\Google\Chrome\NativeMessagingHosts\dev.4o3f.quartz` | `allowed_origins: ["chrome-extension://<固定 ID>/"]` |
| Edge | `HKCU\Software\Microsoft\Edge\NativeMessagingHosts\dev.4o3f.quartz` | 同上 |
| Firefox | `HKCU\Software\Mozilla\NativeMessagingHosts\dev.4o3f.quartz` | `allowed_extensions: ["quartz@4o3f.dev"]` |

Chrome/Edge 共用 `dev.4o3f.quartz.chrome.json`，Firefox 使用 `dev.4o3f.quartz.firefox.json`。manifest 的 `path` 指向与当前 `quartz.exe` 同目录的 `quartz-host.exe`：

- **幂等与路径修复**：仅当 manifest 内容或注册表值与当前路径不一致时才改写，重复启动无多余写入；移动程序目录后下次启动自动修复路径。
- **缺省与故障隔离**：若同目录缺少 `quartz-host.exe`，仅记一次警告日志并跳过，不向用户报错；单个浏览器的注册表或文件失败时记警告日志并继续处理其余浏览器，绝不触碰其他 host 名称的键。
- **只读状态页**：设置页“浏览器”分组通过 `browser_hosts.status` 展示各浏览器状态（已连接、未连接（在浏览器中安装 Quartz 扩展后自动连接）、注册失败（查看日志）或缺少 `quartz-host.exe`），不提供启用/停用按钮。

### 2.2 行为

`quartz-host.exe` 是独立 console 子系统程序，不链接 GPUI，浏览器传入的参数不用于路由。

1. 读取首条 stdio 消息，必须是 `browser.hello`；连接核心管道并以 `role = browser` 完成核心 `hello`，附带扩展自报的浏览器种类与 profile。
2. 核心检查同用户、同会话与对端 exe 为同目录的 `quartz-host.exe`（不检查父进程，Edge 经 `cmd.exe` 中转同样可行）。
3. 之后逐帧校验 256 KiB 上限、JSON 结构，且只允许 `browser.*` 操作，再双向转发；stdout 只写协议帧，日志不写 stdout/stderr。
4. 连不上核心时向扩展发送 `{"t":"evt","topic":"control","seq":1,"body":{"op":"core_offline"}}` 后退出。host 不拉起核心、不落盘、不缓冲；浏览器关闭 stdin 或核心断开时退出。

## 3. 核心的关联与求交

### 3.1 当前上下文

- 核心按连接维护一个 stream：`browser_streams` 行（浏览器种类 + `profile_instance_id`，首次出现时创建，更新 `last_seen_utc_ms`）与内存中的当前状态。状态经采集队列送到采集线程，与前台观测按同一顺序处理。
- 连接断开即清除该 stream 的状态：关闭其开放网页行到最后确认点，未知尾段不延长。
- 网页行（`web_intervals`）由采集线程与前台区间在同一批次、同一事务中开关和推进确认点；崩溃恢复同前台事实（03 第 4.3 节）。

### 3.2 与前台事实求交

只有同时满足以下条件才开启关联网页行并累计网站时长：

- 前台应用为 `chrome.exe`、`msedge.exe` 或 `firefox.exe`，并与 stream 自报的浏览器种类一致。
- 该 stream 报告聚焦且未最小化，且第 3.3 节选出唯一候选。
- 隐私动作允许：`private_window` 与 `excluded` 禁止关联；`web = none` 不写网页行；`web = domain_only` 丢弃 Windows 标题与标签标题。

可用状态在查询时统一应用。网页行只在关联的前台区间内计时，使用该区间的时钟段；前台区间切段（午夜、时钟跳变、策略变化）时网页行同步切段，不跨时钟段。今日页通过 `query.day` 的 `websites` 排行展示按域名汇总的当日网站活跃时长。

### 3.3 匹配

浏览器窗口 ID 不是 HWND。首版只在内存中比较前台窗口标题与各聚焦 stream 的活动标签标题：去掉已知浏览器后缀（如 ` - Google Chrome`、` - Microsoft Edge`、` — Mozilla Firefox` 及其本地化变体与 profile 后缀）后相等，或一方为另一方前缀。恰好一个 stream 匹配时为 `linked`；零个或多个为 `ambiguous`，不计网站时长，时间轴显示“浏览器（无法确定标签）”。

前台或标题变化、标签变化、失焦、最小化、断线和规则变化都会重新求值；结果变化时以 `policy_changed` 切段。

### 3.4 隐私

- 浏览器有已连接 stream 时，该浏览器的前台窗口在 `linked` 之前一律 `title_hidden`；`linked` 后把窗口标题、标签标题、域名与 profile 一起按隐私规则求值，再以 `policy_changed` 切段恢复允许保留的字段。不做等待缓冲：最初不到一秒的片段没有标题，但时长照常统计。
- 浏览器没有已连接 stream 时沿用 `builtin.browser_private` 标题模式；若存在依赖 `web.*` 或 `browser.profile_label` 的隐藏规则，则直接丢弃该浏览器窗口标题。
- 扩展消息中的域名与标签标题只在内存中参与求值；被丢弃的字段不入库、不推送、不进托盘与日志。

## 4. 测试场景

- 三个浏览器：最低版本安装、注册、首次连接、浏览器重启、核心重启、扩展更新、后台被回收后由 alarm 唤醒；Edge 直接启动与 `cmd.exe` 中转。
- 两个 profile 打开相同标题（ambiguous）；仅一个聚焦 stream 但标题不匹配；普通与无痕窗口切换。每片最多一个 linked，ambiguous 的网站时长为零。
- 断线、浏览器退出、核心崩溃后恢复：网页行只到最后确认点。
- 中文、emoji、4,096 UTF-16 单元边界与超长标题。
- 依赖 `web.domain` 的隐藏规则、域名后缀规则、隐私模式：数据库、日志、IPC 与托盘中无被丢弃字段；无痕窗口标题从不出现。
- 其他用户、其他会话、非同目录 host 均不能以 `browser` 角色接入。
