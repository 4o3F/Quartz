# 05 浏览器接入

浏览器扩展上报活动标签与状态边界。网站使用时长由核心与前台事实求交得到，扩展不判断空闲和焦点时长。

```text
扩展 ──connectNative──▶ quartz-host.exe ──命名管道──▶ 核心
 ▲                                                       │
 └──────── browser.ack（影响已持久可恢复的连续序号）─────┘
```

## 1. 扩展

### 1.1 范围

- Chrome、Edge 使用同一份 MV3 service worker 代码；Firefox 使用单独的 manifest 与 event page。
- 项目最低版本为 Chrome/Edge 120、Firefox 128。Chrome 120 起支持 30 秒 alarm，Firefox 128 已支持 alarms 与 event page；最低版本安装与唤醒在 M5 实测（[Chrome 生命周期](https://developer.chrome.com/docs/extensions/develop/concepts/service-workers/lifecycle)、[Firefox alarms](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/alarms)、[Firefox 后台页](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/manifest.json/background)）。
- 权限：`tabs`、`nativeMessaging`、`storage`、`alarms`。不申请 `<all_urls>`，不注入内容脚本。
- manifest 设 `incognito: "not_allowed"`。Windows 层的无痕窗口由 M1 交付的 `builtin.browser_private` 处理，见 06 第 3 节。
- Chrome/Edge 的 manifest 带 `key` 固定扩展 ID；Firefox 用 `browser_specific_settings.gecko.id` 固定 ID。

### 1.2 握手与上报

扩展发出的第一条消息是 `browser.hello`。host 读取后，才以 `role = host` 向核心发送 `hello`，将这些字段和浏览器启动参数提供的 `extension_id` 放入 `body.browser`：

```json
{ "t": "req", "id": 1, "op": "browser.hello", "body": {
    "profile_instance_id": "a286737e-3855-43cb-98ee-c366344372d0",
    "browser_session_id": "be5a458b-e2b2-417a-b236-ab5491e2ec4d",
    "epoch": 12,
    "proto": { "min": 1, "max": 1 },
    "browser": "edge"
} }
```

`profile_instance_id` 首次运行生成，存于 `storage.local`；`browser_session_id` 每次浏览器会话重新生成。`epoch` 每次扩展后台启动递增并持久化。传输请求 `id` 在连接内递增，与持久消息序号 `seq` 分开；重放分配新请求 ID，保留原 epoch 与 seq。host 负责重映射核心连接的请求 ID（04 第 3 节）。

扩展监听 `tabs.onActivated`、`tabs.onUpdated`（URL、标题、audible）、`tabs.onRemoved`、`windows.onFocusChanged`、`windows.onRemoved`。状态变化时发送：

```json
{ "t": "req", "id": 2, "op": "browser.state", "body": {
    "epoch": 12,
    "seq": 345,
    "at_ms": 1763280000123,
    "focused": true,
    "window": { "id": 7, "state": "normal" },
    "tab": { "id": 91, "navigation_id": 4, "domain": "github.com",
             "title": "4o3F/quartz", "title_status": "ok", "audible": false },
    "full": false
} }
```

- 发送前从 URL 取出域名，完整 URL 不离开扩展。内部页与本地文件只上报 `(internal)`、`(local-file)`，不带路径。
- `navigation_id` 每次顶层导航重新分配。失焦时上报 `focused: false`；最小化时 `window.state = minimized`。
- 标签标题上限为 4,096 个 UTF-16 单元，超限保留截断部分并设 `tab.title_status = truncated`；持久化到核心时对应 `tab_title_status`。隐私与正则使用同一上限。
- 重连顺序固定为：hello → 接收当前 `browser.policy` → 重放缓冲 → 发送新的 `full: true` 完整状态。完整状态开启新范围，不回填未知尾段。

### 1.3 有界短缓冲与重连

内存队列保存未确认消息，标签标题只存在这里。`storage.local` 的持久缓冲只存域名与状态边界，不存标签标题；重放持久条目时标题为空，`title_status = unreadable`，不能用于标题匹配。持久写入合并处理，最多每 2 秒一次。

核心在握手后与策略变化时下发 `browser.policy`：排除的域名后缀列表，以及当前隐私模式截止 UTC。扩展写持久缓冲前丢弃命中域名的内容，仅保留无域名状态边界；隐私模式期间也只保存无域名状态边界。策略未知时按同样的保守方式处理。更新策略时先清洗已有内存与持久队列；已清除内容不恢复。

缓冲上限为 500 条或 10 分钟，以先到者为准。超限丢弃最旧消息；凡是移除带序号的条目（超限、策略清洗或 purge），都保留不含内容的缺口摘要，重连时按序重放 browser.gap，发送明确缺失范围：

```json
{ "t": "req", "id": 3, "op": "browser.gap", "body": {
    "epoch": 12, "from_seq": 300, "to_seq": 320,
    "from_ms": 1763279400000, "to_ms": 1763279460000
} }
```

`from_seq`、`to_seq` 含两端，时间为 `[from_ms,to_ms)`。核心将这些序号持久记录为明确丢弃；遇到 gap、非连续 seq 或新 epoch 时，旧上下文只保留到最后确认点，随后等待完整状态。`upto_seq` 只确认连续已持久处理或明确丢弃的序号；收到 ack 后，扩展删除相应 epoch 中这些序号及之前的条目。

连接断开后按 1 秒起步、60 秒封顶的指数退避加随机扰动重连，退避状态存在 `storage.local`。长退避使用 `chrome.alarms`，以最低支持版本的 30 秒为调度下限；Firefox 使用对应的 `browser.alarms`。短于 30 秒的重试仅在后台仍存活时用定时器优化。每次后台启动先同步注册全部监听器，检查并重建重连 alarm，再读取持久队列。不能依赖 alarm 跨浏览器重启自动保留。

### 1.4 状态展示

- 已连接时 badge 为空；核心未运行显示 `!`；host 未注册显示 `?`。
- popup 用 `browser.query_site_today` 查询本 stream 当前域名的今日时长，显示连接状态。

### 1.5 核心控制消息

浏览器控制消息使用 `evt` 帧，`topic = control`，操作名在 `body.op`：

```json
{ "t": "evt", "topic": "control", "seq": 10, "body": {
    "op": "browser.ack", "epoch": 12, "upto_seq": 345
} }
```

```json
{ "t": "evt", "topic": "control", "seq": 11, "body": {
    "op": "browser.policy",
    "excluded_domain_suffixes": ["private.example"],
    "privacy_mode_until_ms": 1763283600000
} }
```

`privacy_mode_until_ms = null` 表示模式关闭；到期后仅新观测按当前策略处理。域名后缀按小写、punycode 与域边界匹配。

```json
{ "t": "evt", "topic": "control", "seq": 12, "body": {
    "op": "browser.purge", "purge_id": "85b04297-0fc7-440a-911b-544a6821faec",
    "from_ms": 1763279400000, "to_ms": 1763280000000
} }
```

扩展清除内存与持久队列中与该时间范围相交的条目，再请求 `browser.purge_ack {purge_id}`。重复 purge 可安全再次执行；断线期间由核心删除屏障拦截重放，重连后补发未确认的 purge。

## 2. Native Messaging host

### 2.1 注册

设置页为每个浏览器显示未注册、已注册未连接、已连接。用户调用 `browser_hosts.enable` 后，核心在 `%LOCALAPPDATA%\Quartz\native-hosts\` 写 manifest，并写入以下当前用户注册表：

| 浏览器 | 注册表键（默认值为 manifest 绝对路径） | 授权字段 |
|---|---|---|
| Chrome | `HKCU\Software\Google\Chrome\NativeMessagingHosts\dev.4o3f.quartz` | `allowed_origins: ["chrome-extension://<固定 ID>/"]` |
| Edge | `HKCU\Software\Microsoft\Edge\NativeMessagingHosts\dev.4o3f.quartz` | 同上，Edge 扩展 ID 单列 |
| Firefox | `HKCU\Software\Mozilla\NativeMessagingHosts\dev.4o3f.quartz` | `allowed_extensions: ["quartz@4o3f.dev"]` |

Chrome/Edge 共用 `dev.4o3f.quartz.chrome.json`，Firefox 使用 `dev.4o3f.quartz.firefox.json`。manifest 的 `path` 指向 `quartz-host.exe` 绝对路径；核心每次启动检查并在程序目录移动后改写。`browser_hosts.disable` 移除该浏览器注册，`browser_hosts.status` 返回注册及连接状态。

### 2.2 入口

`quartz-host.exe` 是独立的 console 子系统程序，不链接 GPUI。Chrome/Edge 传入 `chrome-extension://<ID>/`，可能另带 `--parent-window=<N>`；Firefox 传入 manifest 绝对路径和扩展 ID。这些参数由浏览器定义，不用于入口路由；扩展调用权限由 manifest 的 `allowed_origins`、`allowed_extensions` 控制。

### 2.3 行为

1. 读取并校验首条 `browser.hello`，再连接核心管道并发送核心 `hello`。浏览器种类由扩展自报，扩展 ID 随核心 `hello` 登记。
2. 核心检查同用户、同会话、exe 路径与登记扩展 ID，不检查直接父进程。Edge 经 `cmd.exe` 中转启动也须通过验收。
3. 随后逐帧校验 256 KiB 上限、JSON 结构与 `browser.*` 操作，再双向转发。stdout 只写协议帧；日志不写 stdout/stderr。
4. 连不上核心时发送下列消息并退出。host 不拉起核心，不落盘，不缓冲；浏览器关闭 stdin 时退出。

```json
{ "t": "evt", "topic": "control", "seq": 1, "body": { "op": "core_offline" } }
```

## 3. 核心的关联与求交

### 3.1 当前上下文与持久化

核心按 stream 维护活动标签、聚焦、最小化与开始时间。每条 `browser.state` 按以下顺序处理：

1. 用 `(stream_id, epoch, seq)` 查回执。重复消息不再处理，只重发连续 ack。
2. 检查缺口、epoch、删除屏障与当前隐私策略。网页规则与前台标题的合流见 06 第 5 节。
3. 在同一事务关闭旧 `web_intervals`，把新上下文与前台事实相交并持久化为开放行（`end_utc_ms = NULL`、`confirmed_utc_ms`），写回执；无交集或被过滤也持久记录丢弃结果。
4. 提交后返回响应并发 `browser.ack`。ack 表示该消息影响已持久可恢复。

当前网页随前台确认点推进，前台切换或结束时同步关闭旧网页行；新上下文只有已验证部分才推进。查询合入开放行的已确认部分。崩溃恢复同前台事实（03 第 4.3 节）。断线立即撤销聚焦与 audible 例外，旧行只关闭到最后确认点；未知尾段不延长，重连完整状态只开新范围。

### 3.2 与前台事实求交

只有同时满足以下条件才累计网站时长：

- 前台应用对应浏览器 exe：`chrome.exe`、`msedge.exe` 或 `firefox.exe`。
- 扩展报告聚焦且未最小化，并且第 3.3 节选出了唯一候选。
- 字段动作允许保留应用，`web != none`。`title_hidden` 本身不禁止网页关联；`private_window` 与 `excluded` 禁止关联。`web = domain_only` 同时丢弃该范围的 Windows 标题与标签标题。

可用状态在查询时统一应用。每条网页行只在关联的前台事实范围内计时，并使用该前台事实所属时钟段计算长度。时钟跳变时用旧段锚点映射关闭旧网页行，再在新段重新关联，网页行不跨时钟段。

### 3.3 多窗口和多 profile

浏览器窗口 ID 不是 HWND，service worker 的 `--parent-window` 也不能提供可靠对应。首版只在内存中用前台窗口标题和活动标签标题进行匹配，再按隐私动作丢弃字段。

每个前台时间片最多一个已确认候选。即使只有一个 stream 报告聚焦，也须验证其活动标签标题与前台窗口标题匹配：去掉已知浏览器后缀后相等或互为前缀。零个或多个匹配都保持 `ambiguous`，不计网站时长，时间轴显示“浏览器（无法确定标签）”。扩展未安装或未连接时仍记录允许保留的前台事实。

关联仅在当前已读取的前台状态与 stream 连接范围内有效；前台或标题变化、标签变化、失焦、最小化、断线、gap、epoch 变化和策略变化都使旧关联失效，必须重新确认。持久重放缺少标题时无法确认，保留 ambiguous。

M5 测量 linked 覆盖率。信号不足时继续保持 ambiguous；如需 UI Automation 读取地址栏等信号，另立原型评估权限、性能与对用户窗口的影响。

### 3.4 迟到消息

迟到消息先按 `deletion_barriers` 和当前隐私策略过滤；隐私只允许进一步收紧。与已有前台事实相交的部分入库并更新对应日期版本；核心离线部分记为 `discarded_no_overlap`，不补造前台时间。缺失 seq 或新 epoch 不允许把旧上下文延长到下一条消息。

## 4. 测试场景

- 三个浏览器：最低版本安装、注册、首次连接、浏览器重启、核心重启、扩展更新、后台被回收后由 alarm 唤醒。
- Edge 直接启动与 `cmd.exe` 中转；Firefox 实际启动链。
- 两个 profile 打开相同标题，多个候选都匹配；仅一个聚焦 stream 但标题不匹配；普通与无痕窗口切换。每片最多一个 linked，ambiguous 的网站时长为零。
- 重复重放、确认后核心立即崩溃、稳定标签持续数小时、断线与浏览器退出、seq 缺口、新 epoch。确认结果可恢复，未知尾段不累计。
- 中文、emoji、4,096 UTF-16 单元边界与超长标题，核对截断状态和隐私求值。
- 隐私模式、域名后缀排除、策略更新与 purge，检查 storage.local 无标签标题、敏感域名或被清除范围。
- 网页规则消息延迟超过 2 秒、扩展离线、迟到重放和历史删除，检查数据库、IPC 与托盘无被丢弃标题。
- 其他用户、其他会话、未登记扩展 ID 均不能以 host 接入。
