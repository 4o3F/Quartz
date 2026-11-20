# 04 IPC 协议

核心是唯一的服务端。界面进程和 host 进程都是客户端。所有消息类型定义在 `quartz-protocol` crate 里。

## 1. 传输

- 管道名：`\\.\pipe\Quartz.<当前用户 SID>.v1`。SID 用来区分同一台机器上的不同用户；访问控制靠 DACL，不靠管道名。
- 字节模式，双向。最多 8 个实例。
- 安全描述符：`D:P(A;;GA;;;SY)(A;;GA;;;<用户 SID>)`，只授予 SYSTEM 和当前用户访问权限，不继承默认 DACL 条目。
- 设置 `PIPE_REJECT_REMOTE_CLIENTS`。首个实例设置 `FILE_FLAG_FIRST_PIPE_INSTANCE`。
- 服务端用 `GetNamedPipeClientProcessId` 取得客户端 PID，然后检查：
  - 客户端与核心属于同一用户、同一个 Windows 会话；
  - 客户端 exe 的完整路径是核心自己的 `current_exe()`（界面、命令行），或同一安装目录中的 `quartz-host.exe`（host）；握手角色必须与该路径对应。
- 客户端连接后，用 `GetNamedPipeServerProcessId` 反向检查服务端与自己同用户、同会话，且完整路径等于自己的 `current_exe()`（生产环境为同一安装目录中的 `quartz.exe`）。

路径比较先用 `std::fs::canonicalize` 解析两端路径，统一为 Windows 扩展绝对路径并解析目录联接、符号链接和 `.` / `..`，再用 `CompareStringOrdinal(..., true)` 对 UTF-16 路径进行忽略大小写的序数比较。路径解析或身份查询失败时关闭连接，服务端只记录 PID 和失败原因。可执行文件路径决定允许的角色集合：核心自身路径允许 `ui`、`cli`，同安装目录的 `quartz-host.exe` 只允许 `host`。

路径检查用来排除意外连接的程序。本地同一账户下的恶意程序可以绕过这个检查，所以真正的安全边界是用户账户本身。

## 2. 分帧

```text
u32 小端，JSON 字节长度 | UTF-8 JSON
```

- 单帧最大 256 KiB。读到长度后先检查上限，再分配缓冲区。超过上限就回一个错误帧并断开连接。
- 处理短读和半帧。连接断开时丢弃半帧。
- 这和 Native Messaging 的格式相同（Windows 上也是小端），所以 host 可以直接转发帧，只需校验长度和 JSON 结构。

## 3. 消息信封

```jsonc
// 请求
{ "t": "req", "id": 17, "op": "query.day", "body": { ... } }
// 响应
{ "t": "res", "id": 17, "ok": true, "body": { ... } }
{ "t": "res", "id": 17, "ok": false, "err": { "code": "invalid_range", "msg": "..." } }
// 推送
{ "t": "evt", "topic": "state", "seq": 42, "body": { ... } }
```

- `id` 由客户端分配，在一个连接里递增，用来匹配响应。host 为自己的核心 `hello` 分配 ID，转发浏览器请求时重映射并恢复响应 ID，避免与扩展请求冲突。
- 创建类操作由客户端在实体中提供 `uuid`。annotations、分类和各类规则用数据库唯一约束去重，重试遇到已有 UUID 时返回已有实体。批量创建时每个实体独立带 UUID；删除不存在的实体也返回成功。更新按实体 UUID 设置最终值，重试保持相同结果。
- 错误码是稳定的字符串枚举，定义在 `quartz-protocol`。核心只发送代码与结构化字段，`quartz-ui` 按代码本地化并呈现；host 与 UI 均不得按错误文本分支。信封中的 `msg` 若存在，仅为日志诊断文字。
- 未知字段一律忽略，以便向前兼容。
- `id: null` 只用于无法取得请求 ID 的连接级错误：帧超限返回 `frame_too_large`，JSON 无法解码返回 `bad_request`。成功响应和已知请求 ID 的错误响应必须带原请求 ID。
- 客户端将无法识别的错误码解码为 `Unknown`。核心不发送 `Unknown`。

| code | 含义 | 结构化字段 |
|---|---|---|
| `bad_request` | 信封或操作参数不符合格式 | 无 |
| `unknown_op` | 不认识请求操作名 | 无 |
| `not_implemented` | 操作已定义，但当前核心尚未实现 | 无 |
| `handshake_required` | 连接首帧不是 `hello` | 无 |
| `frame_too_large` | 帧的 JSON 字节长度超过 256 KiB | 无 |
| `proto_mismatch` | 客户端与核心支持的协议版本没有交集 | `supported: {min, max}`，核心支持的协议范围 |
| `forbidden` | 当前角色没有该操作的权限 | 无 |
| `invalid_range` | 请求的日期或时间范围无效 | 无 |
| `stale_cursor` | 查询输入版本变化，须重新分页 | 无 |
| `rule_set_stale` | 提交的规则集基础版本已过期 | `current_version`，当前规则集版本号（u64） |
| `core_offline` | host 无法连接核心 | 无 |
| `busy` | 核心暂时过载，客户端稍后重试 | 无 |
| `internal` | 核心处理请求时发生内部错误 | 无 |

结构化字段与 `code` 同级，放在 `err` 对象中。

M2 管道建立前，启动致命错误通过 `quartz.exe ui --core-error <code>` 交给 UI，核心随后返回失败退出码。当前稳定代码为 `newer_database`、`migration_failed`、`other_session`、`core_start_failed`。备份路径通过独立、可重复的 `--backup-path <路径>` 参数传递；`migration_failed` 必须带其迁移前备份路径。共享的 `CoreError` 使用 `code` 和 `backup_paths` 字段，后续管道沿用这些代码与字段。

## 4. 握手

连接后 5 秒内未收到完整的 `hello` 帧即断开。

连接后第一帧必须是 `hello`，否则断开：

```jsonc
// 客户端 → 核心
{ "t": "req", "id": 1, "op": "hello", "body": {
    "proto": { "min": 1, "max": 1 },
    "role": "ui",                      // ui | host | cli
    "client_version": "0.1.0",
    "ui": { "state": "prewarming" }     // role = ui：starting | prewarming | visible
    // role = host 时改为 browser 字段，内容来自 browser.hello，见 05 第 1.2 节
} }
// 核心 → 客户端
{ "t": "res", "id": 1, "ok": true, "body": {
    "proto": 1,
    "core_version": "0.1.0",
    "instance_id": "8c1e...",          // 核心每次启动重新生成
    "capabilities": ["browser.v1", "annotations.v1"]
} }
```

协议只传键、编码、数字与原始数据，不传翻译后的句子（10 第 3 节）。

协议版本和数据库 schema 版本分开演进。协议版本范围没有交集时，核心返回 `proto_mismatch`，并在响应里附上自己的版本，让界面能提示"核心与界面版本不一致，请重启 Quartz"。

## 5. 角色与权限

| 角色 | 可用操作 |
|---|---|
| `ui` | 全部查询、订阅、设置、规则、标注、删除、图标、浏览器 host 管理、维护、隐私模式；发送 `ui.open`、`ui.first_frame` |
| `host` | `hello`、`browser.*`；只能为它自己在握手里登记的 `browser_stream` 上报 |
| `cli` | `ui.open`；只读 `query.*`、`apps.icon`、`browser_hosts.status`、`core.status`、`rules_text.validate`、`rules_text.preview`、`rules_text.diff`、`rules_text.export`、`rule_sets.tree`、`rule_sets.versions`、`rules.explain`（用于诊断、测试脚本与规则编写） |

新增功能模块使用独立的操作前缀与订阅主题（例如以后的自控协议使用 `focus.*`、`policy.*` 与 `selfreg` 主题，见 12 第 7 节），权限表按前缀授权。

角色在握手时声明。host 必须通过同用户、同会话及 `quartz-host.exe` 完整路径的检查，握手中的扩展 ID 还必须在已登记列表中；失败则断开。浏览器种类由扩展在 `browser.hello` 自报，host 不依赖父进程名推断。Edge 通过 `cmd.exe` 中转启动也是合法路径。

## 6. 消息清单

### 6.1 核心与界面控制

| op | 方向 | 说明 |
|---|---|---|
| `core.status` | 请求 | 参数 `{}`；返回 `recording`、`stop_reason`、`database`、`browser_streams`、`instance_id`、`core_version` |
| `ui.open` | UI、CLI 请求 | 参数 `{}`；按托盘“打开”动作打开或激活界面，入主线程队列后返回 `{accepted:true}` |
| `ui.show` | 核心 → 预热中的界面 | 显示主窗口 |
| `ui.activate` | 核心 → 界面 | 把已有窗口提到前台 |
| `ui.shutdown` | 核心 → 界面 | 用户退出 Quartz 时通知界面关闭 |
| `ui.first_frame` | UI 请求 | 参数 `{}`；每次显示或激活请求后的首帧完成时发送，返回 `{accepted:true}`，核心清除 `show_requested` |

`core.status` 从主线程发布的不可变快照读取，不访问 SQLite。`database` 为 `ok | persistence_failed`，`stop_reason` 在记录时为 `null`，停止时为稳定枚举：`persistence_failed`、`starting`、`session_unavailable`、`capture_gap`、`suspended`、`disconnected`、`locked`、`paused`、`no_foreground`、`ended`。`awaiting_browser_context` 随 M5 浏览器扩展加入。多个条件重叠时按上述顺序选择；`ended` 表示已结束的采集运行，运行结束的历史原因保留在采集记录中。`browser_streams` 是类型化连接列表，条目包含 `browser_stream` 标识，M5 前为空。`instance_id` 每次核心启动重新生成，`core_version` 为核心的 crate 版本。

核心只在握手能力列表中登记已实现的能力。已定义但当前核心尚未实现的操作返回 `not_implemented`；`host` 运行时实现前，`host` 握手返回 `forbidden` 并断开。

客户端发现管道和等待响应各有 2 秒上限，使用 `CreateFileW` 连接，繁忙时在剩余连接期限内调用 `WaitNamedPipeW`。普通重复启动在连接失败时记录警告并成功退出；`--autostart` 重复启动直接退出。核心在取得实例锁、打开存储后启动 I/O 线程，随后启动采集来源；关闭时先停止管道并关闭连接，再结算采集与关闭数据库。I/O 线程加入使用现有的 8 秒关闭上限。错误响应排入队列后最多等待 500 毫秒供写任务完成及客户端读取，然后断开并补建实例。运行期间槽位故障记录错误，并按 25 毫秒起步、1 秒封顶的指数退避重建；其他槽位、采集和数据库写入继续运行。

核心发给界面的控制消息使用 `evt` 帧，`topic = "control"`，操作名放在 `body.op`：

```json
{ "t": "evt", "topic": "control", "seq": 43, "body": { "op": "ui.show" } }
```

UI 在握手中登记当前状态；核心重放 `show_requested`，直到收到 `ui.first_frame`。预热 TTL 从握手完成时起算，显式打开优先于 TTL（01 第 4 节）。

### 6.2 查询

| op | 参数 | 返回 |
|---|---|---|
| `query.day` | `local_date`、可选 `cursor`（日起点偏移取自设置，见 03 第 1 节） | 状态行指标、每小时分类堆叠、排行（应用、域名、文档，完整列表分页）、未分类时长 |
| `query.timeline` | UTC 毫秒范围 `start_utc_ms` / `end_utc_ms`、`detail`（`category` / `app` / `raw`）、正整数 `ms_per_pixel`、可选 `cursor` | 时间轴片段；小于一个像素对应时长的相邻片段由核心合并，界面传入"每像素毫秒数" |
| `query.range` | 起止逻辑日期、时区、维度 | 按天或按周聚合 |
| `query.segment_detail` | 时间点或片段 ID | 原始区间、标题、网页上下文、分类来源（规则 ID 或标注 ID）、提取结果 |
| `query.entity_detail` | 应用、域名或文档 | 近 7 天每日时长、常见标题、命中的规则 |
| `query.unclassified` | 时间范围 | 未分类的条目，按时长降序 |
| `query.pending_away` | 时间范围 | 超过 15 分钟、还没有标注的离开时段；排除 `dismissed_away` 覆盖范围 |

查询使用核心执行时的当前系统时区。

查询结果按页返回，每页不超过 200 KiB。分页用不透明游标，响应和游标都绑定事实、规则、元数据与查询语义的输入版本。数据库线程用只读连接持有 WAL 快照分批计算；版本变化返回 `stale_cursor`，客户端丢弃旧分页并重查（03 第 4.1 节）。

`query.day` 返回 `metrics`（`active_ms`、`focus_ratio`、`distract_ratio`、`unclassified_ms`、`longest_focus_ms`、`switches_per_hour`）、`hours`、`applications`、`versions` 和 `next_cursor`。`metrics` 和 `hours` 仅出现在第一页，应用排行按活跃时长降序、同值按应用 ID 排序，完整列表可继续分页；域名和文档排行字段分别在 M5、M4 接入。专注与分心占比、最长连续专注时长按分类倾向计算；每小时切换次数为连续活跃片段之间的应用切换次数除以活跃小时数，活跃时长为 0 时返回 0。

逻辑日按 `day.start_offset_min`（默认 0 分钟）加到本地日期后交给 jiff 转换，小时桶由 jiff 逐小时推进，携带 UTC 起止、本地时间标签和 `categories: [{category, duration_ms}]` 分类堆叠。`query.timeline` 使用 UTC 范围，最长 366 天；返回 `segments`、`versions`、`next_cursor`。每个片段携带 UTC 起止、按单调时间计算的 `duration_ms`、时钟段 ID、活动状态、分类及来源；`app` / `raw` 细节附应用 ID、exe 名、显示名和 AUMID，`raw` 仅在 Full 采集级别附标题。

时间轴按时钟段开始时间和 ID 排列，保留各段内的边界顺序。相邻片段仅在 UTC 首尾连续、时钟段相同、活动状态和当前细节级别的可见字段完全相同，且合并后的 UTC 长度严格小于 `ms_per_pixel` 时合并；`raw` 还要求来自同一前台事实。合并保留单调时长之和。

游标是 `quartz-protocol::QueryCursor` 的 JSON 对象，私有字段保存 `versions`、类型化的 `query` 参数和下一项 `position`，客户端原样保存和回传。

版本摘要覆盖相交 UTC 日期的全部 `data_day_versions` 来源、`settings.metadata_version`、规则注册表快照中的分类哈希、日起点偏移、查询 UTC 边界和 `QUERY_SEMANTICS_VERSION`；缺失的元数据版本从 0 开始。每次请求在一个读事务中计算完整结果再选取一页；游标参数不匹配或位置超出列表返回 `bad_request`，输入版本变化返回 `stale_cursor`。I/O 到数据库的查询通道容量为 `QUERY_CHANNEL_CAPACITY`（8），满时返回 `busy`。

### 6.3 规则、标注与设置

| op | 说明 |
|---|---|
| `rule_sets.tree` | 某类规则的规则集树：每个规则集的 key、父级、版本、启用状态、差异统计（新增、覆盖、停用）与告警（目标已删除、上游已变更、冲突） |
| `rule_sets.get` / `rule_sets.versions` | 规则集某版本的内容；版本列表 |
| `rule_sets.set_enabled` / `rule_sets.reorder` / `rule_sets.delete` / `rule_sets.revert` | 启停（连同子树）、同级排序、删除（有子规则集时拒绝）、回退到某个旧版本（生成新版本） |
| `rule_sets.edit` | 界面编辑器提交对某个规则集的一次修改，生成新版本；编辑内置规则时写入指定的用户覆盖规则集 |

`rule_sets.set_enabled`、`reorder`、`delete`、`revert`、`edit` 与 `rules_text.import` 都是写操作，请求必须携带 `base_version`（客户端读取时看到的规则集版本；`reorder` 携带同级各规则集的版本）。核心把写操作作为命令交给规则注册表串行处理（01 第 6.2 节）；版本不一致时返回错误码 `rule_set_stale` 与当前版本号，客户端重新读取，必要时显示三方对比后再提交。
| `rules.preview` | 预览一条规则草稿：过去 7 天命中多少段、多少小时，列出前 50 个样本 |
| `rules.explain` | 解释一个时刻或片段：按规则集树列出每条规则、每个条件的求值结果与实际字段值（受隐私约束），标出生效规则与被遮蔽的规则 |
| `rules_text.validate` | 校验一份 `quartz-rules/1` 文本，返回结构化错误（路径、错误码、参数），不写入 |
| `rules_text.preview` | 校验并预览文本中的每条规则，不写入 |
| `rules_text.diff` | 与同一 key 的当前版本（或指定两个版本）对比：规则级差异，以及过去 N 天结果会改变的片段（分类、提取、隐私级别、判例放行） |
| `rules_text.import` | 校验通过且已预览后导入；同一 `id` 视为新版本。判例与隐私规则必须携带对应预览的版本号，否则拒绝 |
| `rules_text.export` | 把选中的规则导出为 `quartz-rules/1` 文本 |
| `categories.*` | 分类的增删改、排序 |
| `annotations.create` / `update` / `delete` | 可以一次提交多个不相交的范围，核心在一个事务里写入 |
| `settings.get` / `settings.set` | 设置 |
| `data.delete` | 按日期范围、应用、域名、标题关键词或隐私规则删除，见 03 第 6 节 |
| `tracking.pause` / `tracking.resume` / `tracking.keep_alive` | 暂停、恢复、临时保持计时 |

### 6.4 浏览器

| op | 方向与角色 | 参数 | 返回或影响 |
|---|---|---|---|
| `browser.hello` | 扩展 → host | profile、会话、epoch、协议版本、浏览器种类（05 第 1.2 节） | host 据此向核心 `hello`，映射其响应 |
| `browser.state` | host 请求 | epoch、seq、at_ms、focused、window、tab、full | 持久化后响应 `{accepted:true}`，另发 `browser.ack` |
| `browser.gap` | host 请求 | epoch、from_seq、to_seq、from_ms、to_ms | 持久记录缺失范围，响应 `{accepted:true}` 后可推进连续 ack |
| `browser.ack` | 核心 → host → 扩展 | epoch、upto_seq | control 推送，仅确认连续已处理或明确丢弃的序号 |
| `browser.policy` | 核心 → host → 扩展 | excluded_domain_suffixes、privacy_mode_until_ms | control 推送；清洗队列并更新落盘策略 |
| `browser.purge` | 核心 → host → 扩展 | purge_id、from_ms、to_ms | control 推送；清理相交队列 |
| `browser.purge_ack` | host 请求 | purge_id | 扩展清理完成确认，返回 `{accepted:true}` |
| `browser.query_site_today` | host 请求 | `{}` | 核心按设置时区取今日，只返回本 stream 当前域名的 `{local_date,time_zone,domain,duration_ms}`；域名由核心取。被策略隐藏时 domain 为 null、duration_ms 为 0 |

握手、状态与 control 消息的完整示例见 05 第 1.2、1.5 和 2.3 节。

### 6.5 图标、浏览器管理与维护

| op | 角色 | 参数 | 返回 |
|---|---|---|---|
| `apps.icon` | ui、cli | app_id、size_px | PNG 的 base64；不存在时返回空图标 |
| `browser_hosts.status` | ui、cli | `{}` | 各浏览器注册状态、连接 stream 列表 |
| `browser_hosts.enable` | ui | browser | 写入 manifest 和注册表后的状态 |
| `browser_hosts.disable` | ui | browser | 移除注册后的状态 |
| `maintenance.backup` | ui | `{}` | 备份文件名、完成时间 |
| `maintenance.compact` | ui | `{}` | 压缩后的数据库字节数 |
| `privacy_mode.set` | ui | enabled、until_utc_ms（启用时必填） | 当前模式与截止时间；触发 `policy_changed` |

备份和压缩由核心执行。临时保持计时、暂停和隐私模式的截止时间保存在 `settings`，重启后按截止时间恢复。

## 7. 订阅

```jsonc
{ "t": "req", "id": 5, "op": "subscribe", "body": { "topics": ["state", "day:2026-11-16"] } }
```

| topic | 推送内容 | 节流 |
|---|---|---|
| `state` | 当前前台应用、标题（受隐私级别约束）、是否在记录、停止原因、扩展连接状态、当前界面语言与区域设置（10 第 1 节） | 变化即推送，最多每 250 ms 一次，只保留最新值 |
| `day:<日期>` | 当天数据有变化的提示，带新的版本号 | 最多每 5 秒一次；界面收到后自行重新请求需要的查询 |
| `control` | `body.op` 指定的界面或浏览器控制消息 | 不节流，不合并 |

- `subscribe` 返回类型化的空对象 `{}`，响应先于该订阅的首个快照写出；重复订阅按连接追加，已有主题保持不变且不重复登记。
- 新增 `state` 订阅后立即推送当前快照，此后的变化每个连接最多每 250 毫秒推送一次，只保留最新值。
- `control` 的界面生命周期消息隐式发送给所有已握手的 `ui` 连接，无需显式订阅；显式订阅 `control` 不会重复推送。M5 的浏览器控制消息按登记的 stream 路由。
- `day:<日期>` 使用核心当前系统时区和日起点偏移定义逻辑日。新增订阅在响应之后推送 `{date, versions}`；随后由数据库线程在提交后通过 watch 发布 UTC 日期版本变化，注册表匹配相交日期并合并最新版本，每个日期最多每 5 秒推送一次。
- 界面根据 `state` 里的区间开始时间自行推算"当前已持续多久"，核心不发送秒级推送。
- 没有订阅者时，核心不计算推送内容。
- 每个连接的发送预算为 1 MiB，包含四字节帧头、邮箱中尚未写出的帧和正在写入的帧。响应和控制消息共用先进先出队列，入邮箱前预留预算，写入完成或取消后释放；控制消息按最大序号宽度预留，实际编码长度只会更小。加入可靠消息超出预算时断开该连接。
- `state` 使用一个未编码的最新值槽，较新快照替换尚未取出的旧快照；队列为空时才编码并写入这个槽，编码后同样占用发送预算。outbox 也按实际写入完成时间保证两次状态写入至少间隔 250 毫秒，避免积压恢复后连续补发。每个已订阅日期使用独立的 `day` 最新值槽，outbox 按实际写出完成时间保证该日期两次推送至少间隔 5 秒。持续的可靠消息优先，可能延后状态快照；慢客户端不能阻塞采集和写入。

`state` 消息体如下，字段名使用 snake_case：

| 字段 | 类型与来源 |
|---|---|
| `foreground` | 未跟踪时为 `null`；否则包含已清洗 `IntervalFact` 中的 `app`、`title`，以及实际区间边界 `interval_start_utc_ms`（UTC 毫秒） |
| `foreground.app` | 被隐私规则移除时为 `null`；否则为 `{kind, value}`，`kind` 是 `exe`、`aumid` 或 `unresolved`，`value` 是字符串 |
| `foreground.title` | 隐私处理后的标题，移除时为 `null` |
| `recording`、`stop_reason` | 与 `core.status` 共用判定和优先顺序 |
| `browser_streams` | 与 `core.status` 共用的类型化列表，M5 前为空 |
| `ui_language` | 与托盘采用相同的 Windows 首选语言协商，生产值为 `zh-CN` 或 `en`；调试伪语言使用 `en` 的协议身份 |
| `locale` | `{hour_cycle, first_day_of_week}`，值分别为 `h12` 或 `h24`，以及 `monday` 至 `sunday` 的英文星期名；分别解析用户区域的 `LOCALE_STIMEFORMAT`、`LOCALE_IFIRSTDAYOFWEEK`；读取失败时记录警告并发送 `null` |

主线程通过 watch 发布已清洗的区间和状态；有 state 订阅者时，注册表在每次变化后构造一份状态快照并立即转发到各连接的最新值槽，day 变化也立即转发到对应槽。每个连接的 outbox 从实际写入完成时计时，按上述间隔限流，并仅保留各槽的最新值。托盘退出先向注册表投递 `ui.shutdown`，停止管道前在现有关闭期限内尽力写出控制消息；显示与激活的 UI 连接选择留待 M3。

## 8. 重连

- 核心重启后 `instance_id` 会变。客户端发现连接断开时，以 0.5 秒起步、30 秒封顶的指数退避加随机扰动重连。重连成功后重新握手、重新订阅、重新请求快照。
- 正常连接期间不发心跳。
- host 连接失败就回 `core_offline` 并退出。扩展负责 1–60 秒退避与 alarm 唤醒，不套用 UI 的重连周期，见 05 第 1.3 节。

## 9. 测试

- 分帧编解码：边界长度、超限、短读、半帧、非 UTF-8、深层嵌套 JSON。
- 权限：用一个非 Quartz exe、其他用户或其他会话连接应被拒绝；未登记扩展 ID 被拒绝；`host` 调用 `data.delete` 应返回 `forbidden`。Edge 直接启动与 `cmd.exe` 中转分别验收。
- 创建已提交但响应丢失后，重启核心并以同一 UUID 重试，实体不重复；分页中修改事实或元数据，旧游标返回 `stale_cursor`。
- 慢客户端：一个连接停止读取 10 分钟，核心的采集和写入延迟不受影响，发送预算不超过 1 MiB。自动测试将此场景缩短为数秒，检查发布耗时、其他连接的状态响应延迟、预算峰值和响应洪泛后的隔离关闭；实际 10 分钟运行纳入 M2-F 验收。
