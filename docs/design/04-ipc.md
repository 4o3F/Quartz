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

I/O 线程维护 8 个连接槽位，每个槽位持续持有一个管道实例。连接结束时，先取消并等待该连接的 outbox 写入任务结束，再调用 `DisconnectNamedPipe` 清理该连接的未完成 I/O 和未读数据，随后复用同一实例等待下一位客户端。连接级错误的最后响应在 500 毫秒内完成发送与关闭等待。核心关闭时先通知槽位停止重用，再在关闭期限内发送待处理控制消息，最后取消并回收槽位任务。

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
- 一个请求等待响应超时只结束该请求，不关闭连接。客户端以已分配编号范围和在途编号集合识别已结束请求，丢弃其迟到或重复响应并仅记录编号的 debug 日志；传输或分帧边界损坏才结束连接。
- 分类和标注创建由客户端提供 `uuid`，用数据库唯一约束去重，重试返回已有实体。批量创建时每个实体独立带 UUID；删除不存在的标注也返回成功。更新按 UUID 设置最终值。规则集以 `key` 标识并按 `base_version` 并发校验（第 6.3.1 节）；历史清理以任务 UUID 去重（第 6.3.4 节）。
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
| `proto_mismatch` | 客户端与核心支持的协议版本没有交集 | `supported: {min, max}`，核心支持的协议范围；`core_version` 必填，为核心发布版本 |
| `forbidden` | 当前角色没有该操作的权限 | 无 |
| `invalid_range` | 请求的日期或时间范围无效 | 无 |
| `stale_cursor` | 查询输入版本变化，须重新分页 | 无 |
| `rule_set_stale` | 提交的规则集基础版本已过期 | `key`、`current_version`（u32）；不存在或已删除时为 0；多目标排序返回按 key 排序的首个冲突 |
| `stale_inputs` | 首次预览或比较的输入版本已变化 | `current_versions`，当前范围的查询摘要 |
| `stale_document_ref` | 文档引用依赖已变化 | 无；重新查询产生引用的范围 |
| `rule_set_not_found` | 规则集或指定历史版本不存在 | `key`、`version`（未指定时为 null） |
| `rule_set_read_only` | 请求修改内置规则集 | `key` |
| `rule_set_has_children` | 删除的规则集仍有子级 | `key` |
| `rule_set_identity` | 已有 key 的固定身份不一致 | `key`、`field`（`kind` 或 `parent`） |
| `rule_validation_failed` | 规则文本或编辑内容未通过校验 | `diagnostics`、`truncated`，第 6.3.3 节的有界诊断列表 |
| `version_exhausted` | 版本计数无法继续递增 | 无；事务未写入 |
| `preview_required` | 隐私写入或历史清理缺少预览回执 | 无 |
| `preview_expired` | 回执无法识别、已到期、被逐出或核心已重启 | 无 |
| `preview_mismatch` | 回执与本次确认不一致 | `reason`：`mutation`、`inputs`、`range` 或 `backups` |
| `deadline_exceeded` | 只读工作超过本次期限 | 无 |
| `cancelled` | 只读工作被取消 | 无 |
| `delete_scope_unsupported` | 当前里程碑不支持此删除范围 | `scope`，请求的 kind |
| `delete_job_not_found` | 清理任务不存在 | `uuid` |
| `delete_job_conflict` | 已有任务 UUID 对应另一份确认内容 | `uuid` |
| `core_offline` | host 无法连接核心 | 无 |
| `busy` | 核心暂时过载，客户端稍后重试 | 无 |
| `internal` | 核心处理请求时发生内部错误 | 无 |

结构化字段与 `code` 同级，放在 `err` 对象中。

M2 管道建立前，启动致命错误通过 `quartz.exe ui --core-error <code>` 交给 UI，核心随后返回失败退出码。当前稳定代码为 `newer_database`、`migration_failed`、`other_session`、`core_start_failed`。备份路径通过独立、可重复的 `--backup-path <路径>` 参数传递；`migration_failed` 必须带其迁移前备份路径。共享的 `CoreError` 使用 `code` 和 `backup_paths` 字段，后续管道沿用这些代码与字段。

### 3.1 版本与数值

规则修订字段 `version`、`base_version`、`parent_version`、`current_version`、`builtin_revision` 统一为 u32；已保存版本范围为 1–4,294,967,295，创建哨兵为 0。协议范围与查询语义版本为 u32；日数据、元数据和统计策略计数为 u64，持久化上限为 i64::MAX。计数递增溢出返回 `version_exhausted`。UTC 毫秒、时长与数据库实体 ID 沿用 i64。JSON 以整数编码，客户端必须无损解码，不经浮点数中转。

`classification_version`、`extractor_version`、`privacy_version` 为 64 个字符的小写十六进制 SHA-256 文本；响应 `versions` 为不透明字符串摘要，不是修订整数。它同时用于游标、文档引用与预览输入校验。

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

协议只传键、编码、数字与原始数据，不传翻译后的句子（10 第 3 节）。UI 完成握手后订阅 `state`，从首个快照的 `logical_date` 取得核心当前逻辑日期，字段见第 7 节。

提取字段、文档引用、文档排行与引用范围为增量字段，握手协议版本保持 1，旧客户端忽略未知字段；新客户端允许这些字段缺失。核心声明 `query.documents.v1`，覆盖文档排行、时间轴与原始详情的提取和引用字段，以及文档实体详情。

协议版本和数据库 schema 版本分开演进。协议版本范围没有交集时，核心返回 `proto_mismatch`，并在响应里附上自己的版本，让界面能提示"核心与界面版本不一致，请重启 Quartz"。

## 5. 角色与权限

| 角色 | 可用操作 |
|---|---|
| `ui` | 全部查询、订阅、设置、规则、标注、删除、图标、浏览器 host 管理、维护、隐私模式；发送 `ui.open`、`ui.first_frame`、`ui.hidden` |
| `host` | `hello`、`browser.*`；只能为它自己在握手里登记的 `browser_stream` 上报 |
| `cli` | `ui.open`、`settings.get`、`settings.set`；只读 `query.*`、`apps.icon`、`browser_hosts.status`、`core.status`、`rules_text.validate`、`rules_text.preview`、`rules_text.diff`、`rules_text.export`、`rule_sets.tree`、`rule_sets.get`、`rule_sets.versions`、`rules.explain`、`requests.cancel`（用于诊断、测试脚本与规则编写） |

新增功能模块使用独立的操作前缀与订阅主题（例如以后的自控协议使用 `focus.*`、`policy.*` 与 `selfreg` 主题，见 12 第 7 节），权限表逐项授权；同一前缀下的读写操作可以有不同权限。`rules.preview`、`categories.*`、`data.delete` 和 `data.delete.*` 仅允许 `ui`；CLI 使用 `rules_text.preview`。`requests.cancel` 仅允许 UI、CLI 取消本连接的只读工作。

角色在握手时声明。host 必须通过同用户、同会话及 `quartz-host.exe` 完整路径的检查，握手中的扩展 ID 还必须在已登记列表中；失败则断开。浏览器种类由扩展在 `browser.hello` 自报，host 不依赖父进程名推断。Edge 通过 `cmd.exe` 中转启动也是合法路径。

## 6. 消息清单

### 6.1 核心与界面控制

| op | 方向 | 说明 |
|---|---|---|
| `core.status` | 请求 | 参数 `{}`；返回 `recording`、`stop_reason`、`database`、`browser_streams`、`instance_id`、`core_version` |
| `ui.open` | UI、CLI 请求 | 参数 `{}`；按托盘“打开”动作打开或激活界面，入主线程队列后返回 `{accepted:true}` |
| `ui.show` | 核心 → 已登记的启动、预热或隐藏界面 | 向登记连接发送控制事件，显示主窗口 |
| `ui.activate` | 核心 → 已登记的可见界面 | 向登记连接发送控制事件，还原最小化窗口并请求前台激活 |
| `ui.shutdown` | 核心 → UI | 退出 Quartz 时广播；预热 TTL 到期或关闭隐藏常驻时，仅定向发送给对应 UI |
| `ui.hidden` | UI 请求 | 参数 `{}`，仅允许 UI 角色，返回 `{accepted:true}`；最后一个窗口关闭且常驻开启时发送，核心将登记转为 `Hidden` 并取消 TTL，打开请求通过 `ui.show` 重建窗口 |
| `ui.first_frame` | UI 请求 | 参数 `{}`；每次显示或激活请求后的首帧完成时发送，返回 `{accepted:true}`，登记连接的首帧通知使核心设为可见并清除 `show_requested` |

`core.status` 从主线程发布的不可变快照读取，不访问 SQLite。`database` 为 `ok | persistence_failed`，`stop_reason` 在记录时为 `null`，停止时为稳定枚举：`persistence_failed`、`starting`、`session_unavailable`、`capture_gap`、`suspended`、`disconnected`、`locked`、`paused`、`no_foreground`、`ended`。`awaiting_browser_context` 随 M5 浏览器扩展加入。多个条件重叠时按上述顺序选择；`ended` 表示已结束的采集运行，运行结束的历史原因保留在采集记录中。`browser_streams` 是类型化连接列表，条目包含 `browser_stream` 标识，M5 前为空。`instance_id` 每次核心启动重新生成，`core_version` 为核心的 crate 版本。

M4 能力字符串及其覆盖操作如下；客户端逐项检查能力，核心只在对应契约全部实现后声明：

| capability | 操作或字段 |
|---|---|
| `query.documents.v1` | 文档排行、提取字段、`document_ref`、document 详情 |
| `query.range.v1` / `query.unclassified.v1` / `query.annotations.v1` | 各自同名前缀的查询 |
| `categories.v1` | 第 6.3.5 节分类读写 |
| `rule_sets.v1` | 第 6.3.1 节全部规则集读写 |
| `rules.preview.v1` | `rules.preview`、`rules_text.preview` 及隐私保存回执校验 |
| `rules_text.v1` | validate、diff、import、export |
| `rules.explain.v1` | 片段解释 |
| `requests.cancel.v1` | 本连接只读请求取消 |
| `data.delete.privacy.v1` | 隐私历史 preview、执行与 status；不授予通用范围删除 |

核心只在握手能力列表中登记已实现的能力。已定义但当前核心尚未实现的操作返回 `not_implemented`；`host` 运行时实现前，`host` 握手返回 `forbidden` 并断开。

客户端发现管道和普通请求等待响应各有 2 秒上限；长预览的单次期限与取消见第 6.3.2 节。使用 `CreateFileW` 连接，繁忙时在剩余连接期限内按 10 毫秒间隔重试。普通重复启动在连接失败时记录警告并成功退出；`--autostart` 重复启动直接退出。核心在取得实例锁、打开存储后启动 I/O 线程，随后启动采集来源；关闭时先停止管道并关闭连接，再结算采集与关闭数据库。管道线程的启动等待与关闭加入上限均为 8 秒。错误响应排入队列后最多等待 500 毫秒供写任务完成及客户端读取，然后断开并补建实例。运行期间槽位故障记录错误，并按 25 毫秒起步、1 秒封顶的指数退避重建；其他槽位、采集和数据库写入继续运行。

核心发给界面的控制消息使用 `evt` 帧，`topic = "control"`，操作名放在 `body.op`：

```json
{ "t": "evt", "topic": "control", "seq": 43, "body": { "op": "ui.show" } }
```

UI 首次连接失败后等待显式操作；已连接后的管道断开使 UI 关闭窗口并退出，生命周期见 01 第 4 节。启动核心时通过命名就绪事件等待管道监听实例创建，不进行自动重连。

UI 在握手中登记当前状态；主线程取得类型化连接标识、经身份校验的进程标识和 `ui.state`。有待处理的 `show_requested` 时，`prewarming`（常驻开启时映射为核心的 `Hidden`）重放 `ui.show`，`visible` 重放 `ui.activate`，`starting` 自行创建窗口。登记连接的 `ui.first_frame` 完成显示请求；注销清除旧请求并保留进程句柄直到退出；关闭期间的新打开请求在旧进程退出后执行一次。常驻 `Hidden` 没有 TTL；关闭常驻后的预热 TTL 从握手完成时起算，显式打开优先于 TTL（01 第 4 节）。

### 6.2 查询

| op | 参数 | 返回 |
|---|---|---|
| `query.facts` | `local_date`、可选 `cursor`（本地日历日午夜至次日午夜） | 原始事实 `facts`、`versions`、`next_cursor`，按开始 UTC 毫秒、类型和主键排序 |
| `query.day` | `local_date`、可选 `cursor`（日起点偏移取自设置，见 03 第 1 节） | 状态行指标、每小时分类堆叠、排行（应用、域名、文档，完整列表分页）、未分类时长 |
| `query.timeline` | UTC 毫秒范围 `start_utc_ms` / `end_utc_ms`、`detail`（`category` / `app` / `raw`）、正整数 `ms_per_pixel`、可选 `cursor` | 时间轴片段；小于一个像素对应时长的相邻片段由核心合并，界面传入"每像素毫秒数" |
| `query.range` | `start_date`、`end_date`（不含）、`bucket`、`dimensions`、可选 `cursor` | 按天或按周的完整分组聚合，见第 6.2.1 节 |
| `query.segment_detail` | `start_utc_ms`、`end_utc_ms`，可选 `app`（ApplicationId）、`window_instance`、`cursor` | 裁剪后的未合并区间 `intervals`、`versions`、`next_cursor` |
| `query.entity_detail` | `entity: {kind:"app", id: ApplicationId}` 或 `{kind:"document", document_ref}`、`local_date`；document 另附 `reference_range`，可选 `cursor` | `daily`、`titles`、`rules`、document 的 `extraction`、`versions`、`next_cursor` |
| `query.unclassified` | `start_date`、`end_date`（不含）、`group_by`、可选 `search`、`cursor` | 未分类活跃时长、样本范围与实体引用，见第 6.2.1 节 |
| `query.annotations` | `start_utc_ms`、`end_utc_ms`、可选 `cursor` | 已保存标注的完整编辑值，见第 6.2.1 节 |
| `query.pending_away` | `local_date`、可选 `cursor`，与 `query.day` 使用相同的时区和日起点设置 | 未标注且至少 15 分钟的连续 idle 片段 |

`query.facts` 在数据库线程的查询快照中读取与 `dump_range` 相同的运行、时钟段、前台与无前台事实、可用状态、采集缺口及限时策略，保留完整存储端点及行采集等级下已存储的标题，沿用 200 KiB 版本化分页并将原始事实内容加入版本摘要。

查询使用核心执行时的当前系统时区。

查询结果按页返回，每页不超过 200 KiB。分页用不透明游标，响应和游标都绑定事实、规则、元数据与查询语义的输入版本。数据库线程用只读连接持有 WAL 快照分批计算；版本变化返回 `stale_cursor`，客户端丢弃旧分页并重查（03 第 4.1 节）。

`query.day` 返回 `metrics`（`active_ms`、`focus_ratio`、`distract_ratio`、`unclassified_ms`、`longest_focus_ms`、`switches_per_hour`）、`hours`、`applications`、`versions` 和 `next_cursor`。`metrics` 和 `hours` 仅出现在第一页，应用排行按活跃时长降序、同值按应用 ID 排序，完整列表可继续分页。专注与分心占比按分类倾向时长除以活跃时长计算，最长连续专注段按同一时钟段内首尾相接的专注活跃片段累计（03 第 5.3 节）；每小时切换次数为连续活跃片段之间的应用切换次数除以活跃小时数，活跃时长为 0 时返回 0。有效活动覆盖与离线活动同样计入活跃时长、分类堆叠和上述指标；应用排行只累计采集到的应用，离线活动不列入应用排行。计算切换次数时，每个离线活动 UUID 作为一个应用身份，同一注解的连续切片不产生切换。

`query.segment_detail` 读取点击块的半开 UTC 范围 `[start_utc_ms, end_utc_ms)`，要求正时长且不超过 `SEGMENT_DETAIL_MAX_SPAN_MS`（48 小时）。固定上限为跨 UTC 日期、夏令时和日起点偏移下的完整逻辑日留出空间；超限、逆序、空范围或无效时间戳返回 `bad_request`。它复用时间轴的原始投影、可用状态掩码和标注切片，保留每条前台事实及状态边界，不做像素合并，按时间轴的时钟段和边界顺序输出。每项为 `{start_utc_ms, end_utc_ms, duration_ms, clock_segment_id, activity, app?, window_instance?, title?, category, category_source}`，端点裁剪到请求范围，时长沿用单调时间裁剪。`app` 与 `window_instance` 都按隐私投影后可见的字段精确过滤，同时提供时取交集；与泳道无关的状态可通过省略过滤器查询。排除区间省略应用、窗口实例和标题，标题仅来自 Full 采集事实。一天内大量事实或长标题可以超过 200 KiB，因此区间列表沿用版本化游标分页。M4 为可见前台区间添加 `extraction` 与 `document_ref`（第 6.2.1 节），M5 接入网页上下文。

`query.entity_detail` 的实体枚举在 M3 仅含 `app`；`document` 在 M4、`domain` 在 M5 加入。它读取截至 `local_date` 当天的 7 个逻辑日（`ENTITY_DETAIL_DAYS`），各日都按当前系统时区和 `day.start_offset_min` 分别转换 UTC 边界，保留夏令时导致的日长度变化。第一页的 `daily: [{local_date, active_ms}]` 按日期升序返回 7 项，应用活跃时长与 `query.day` 应用排行一致，包含有效活动覆盖。未知应用的第一页返回 7 个零时长日、空标题和空规则。

`titles: [{title, duration_ms}]` 只累计这些活跃切片中 Full 级别的可见标题，按时长降序、同值按标题文本升序取前 `ENTITY_DETAIL_TOP_TITLES`（5）项；只有较低采集级别的事实时为空。五个长度上限为 4,096 个 UTF-16 单元的标题即使需要 JSON 转义，也为第一页的日统计和规则留有空间。`rules: [{rule, label, label_key, category}]` 仅包含实际为该应用活跃时间提供最终分类的规则，按修订限定规则 ID 升序去重；`rule` 为 `<规则集>/<规则>@<版本>`，`label`、`label_key` 为当前规则快照的显示字段，可为 null，`category` 使用下述分类引用。分类标注覆盖掉的规则不进入列表。规则数量没有固定上限，因此规则列表分页，`daily` 和 `titles` 仅出现在第一页，后续页省略这两个字段，客户端保留第一页的数据并拼接各页 `rules`。两种详情操作允许 UI 和 CLI，使用同一个数据库读事务中的事实、应用和分类元数据及持有的规则快照，标题不写入日志。

`query.pending_away` 只读取 idle 可用状态范围，锁屏、睡眠、断开和暂停范围不纳入；`offline_activity`、`active_override`、`note`、`dismissed_away` 覆盖的部分视为已标注，`category_override` 不计。部分覆盖时先扣除覆盖范围，再返回剩余连续片段中至少 15 分钟的部分。响应为 `{ranges: [{start_utc_ms, end_utc_ms, duration_ms}], versions, next_cursor}`，按开始时间排序，时长为 UTC 端点差，分页沿用版本化游标。

逻辑日按 `day.start_offset_min`（默认 0 分钟）加到本地日期后交给 jiff 转换，小时桶由 jiff 逐小时推进，携带 UTC 起止、本地时间标签和 `categories: [{category, duration_ms}]` 分类堆叠。`query.timeline` 使用 UTC 范围，最长 366 天；返回 `segments`、`versions`、`next_cursor`。每个片段携带 UTC 起止、按单调时间计算的 `duration_ms`、时钟段 ID、活动状态、分类及来源；`app` / `raw` 细节附应用 ID、exe 名、显示名和 AUMID，以及不透明的持久窗口实例引用 `window_instance`；客户端仅比较和回传该引用，不解释其序列化整数。`category` 细节、已排除区间和非前台活动状态省略 `window_instance`。`raw` 仅在 Full 采集级别附标题。M3 的应用泳道使用 `raw` 细节显示窗口实例和可用的原始标题。

分类引用为 `{kind:"unclassified"}` 或 `{kind:"classified", id, uuid, builtin_key, name, color, tendency}`。`builtin_key` 是稳定内置键，`name` 为用户输入或 null；名称为 null 时由界面翻译 `category.builtin.<key>`。规则来源为 `{kind:"rule", rule:"<规则集>/<规则>@<版本>"}`，注解来源为 `{kind:"annotation", annotation:"<UUID>"}`，未分类来源为 `{kind:"unclassified"}`。活动状态新增 `{kind:"active_override", annotation:"<UUID>"}` 和 `{kind:"offline_activity", annotation:"<UUID>"}`。分类元数据从查询的数据库快照读取，求值使用生成游标规则哈希的同一规则快照。

时间轴按时钟段开始时间和 ID 排列，保留各段内的边界顺序。M4 的 `app` 细节对同窗口实例、同非空文档身份的连续片段先合并，保留分类与来源边界；无身份时要求原始标题相同；合并不跨时钟段或活动状态边界。dirty 等可变提取显示字段不阻止 app 合并，合并后采用最后切片的提取显示值。`raw` 保留原始标题与提取输出边界。其余像素合并规则为：相邻片段仅在 UTC 首尾连续、时钟段相同、活动状态和当前细节级别的可见字段完全相同，且合并后的 UTC 长度严格小于 `ms_per_pixel` 时合并；`category` 细节只比较分类和活动状态，允许不同规则来源合并，注解来源与其他来源、不同注解 UUID 之间保留边界；规则来源不同时返回 `{kind:"mixed"}`；`app` 与 `raw` 保留来源边界，并要求窗口实例引用相同；`app` 与 `raw` 均要求文档身份相同（含两者皆空），`raw` 还要求标题与提取输出相同。同一窗口实例的不同前台事实满足这些条件时可合并。合并保留单调时长之和。M4 查询语义版本为 6，版本摘要包含该版本，因此此前语义生成的游标返回 `stale_cursor`。

游标是 `quartz-protocol::QueryCursor` 的 JSON 对象，私有字段保存 `versions`、类型化的 `query` 参数和下一项 `position`，客户端原样保存和回传。

版本摘要覆盖相交 UTC 日期的全部 `data_day_versions` 来源、`settings.metadata_version`、规则注册表快照中的分类、提取与隐私哈希、系统时区身份、日起点偏移、每周首日（周桶）、`stats_policy_version`、查询 UTC 边界和 `QUERY_SEMANTICS_VERSION`；缺失的元数据版本从 0 开始。响应分页计算绑定同一个完整输入摘要；query.range 按日折叠聚合，避免物化全年时间轴；游标参数不匹配或位置超出列表返回 `bad_request`，输入版本变化返回 `stale_cursor`。I/O 到数据库的查询通道容量为 `QUERY_CHANNEL_CAPACITY`（8），满时返回 `busy`。 M3 今天页客户端遇到 `stale_cursor` 时丢弃整组分页结果，最多重启 2 次；耗尽后显示行内错误。`busy` 与其他查询错误直接显示错误码和“重试”，不自动轮询。

### 6.2.1 M4 查询补充

新增列表沿用 `{versions, next_cursor}` 和 200 KiB 页上限。日期范围为 `[start_date, end_date)` 的 1–366 个逻辑日，按核心当前系统时区及日起点分别换算 UTC；响应带 `time_zone` 和 `day_start_offset_min`。M4 不接受客户端指定另一时区。

- `query.day` 增加分页 `documents: [{document_ref, app, document, project, profile, active_ms}]`，应用与文档列表分别完整分页，共用 `next_cursor`：先返回应用页，再返回文档页，每页只填充其中一个列表，另一个为空数组；第一页还带 `reference_range:{start_utc_ms,end_utc_ms}`（覆盖截至所选日的 7 个逻辑日），分页摘要也覆盖此引用范围。文档按 `active_ms` 降序，同值按 03 第 5.2 节身份分量排序（null 在空串之前，文本按 Unicode 标量序）；没有身份的事实不进入文档排行。
- 时间轴的 `app` / `raw` 与原始详情增加 `extraction: {document, project, profile, dirty, normalized_title, extractor_rule, status}` 和可空的 `document_ref`。隐藏字段为 null，`extractor_rule` 在回退时为 null。`query.timeline` 和 `query.segment_detail` 的引用覆盖本次 UTC 范围，并在第一页返回该 `reference_range`。
- B3 的引用编码为 `quartz.document.v1:<sha256>:<JSON>`，JSON 仅含可见身份、引用 UTC 范围和输入版本摘要。校验和为 SHA-256（固定前缀与原样 JSON 字节），用于检测传输或存储损坏；读取引用仍须校验当前范围与输入版本，并按当前隐私投影重算身份。编码无密钥、无服务端引用表，同范围、同版本和同身份产生相同字符串；引用内容不承担访问授权。
- `document_ref` 为不透明字符串，绑定身份及 `reference_range` 内全部查询输入版本；客户端只保存、比较和回传。`query.entity_detail` 的 document 请求附原样 `reference_range`，该范围必须包含详情所需的 7 日；核心校验引用覆盖范围与版本后按身份重算各日，`daily` 与文档排行一致，标题和最终分类规则沿用 app 详情。document 详情增加 `extraction: [{extractor_rule,status}]`，完整列出该身份在这 7 日活跃事实中实际观察到的提取规则与状态组合，回退规则为 null；按规则引用（null 在前、文本按 Unicode 标量序）、状态（extracted、fallback、empty、unknown）排序去重。共用 `next_cursor` 先返回 `rules` 页，再返回 `extraction` 页，另一列表为空；`daily` 和 `titles` 仅在第一页返回。`extraction` 为空时省略该字段。document 请求缺少范围、引用覆盖范围不足或格式无效返回 `bad_request`；引用范围为正时长且最多 367 × 24 小时，以容纳 366 个逻辑日跨 UTC 偏移回退后的范围。app 请求不得附加该范围，违反时返回 `bad_request`，依赖变化返回 `stale_document_ref`。相同 reference_range 和输入版本内，相同身份使用相同引用。document 详情的游标保留原样 `reference_range`，分页版本摘要覆盖整个引用范围；`query.day` 的版本摘要与日期订阅同样覆盖其 7 日引用范围。客户端加载今天页时，时间轴与待补记按各自范围校验分页版本，全部读取后再查询 `query.day` 首页并比较七日摘要，以检查跨查询期间的输入变化。
- `query.range` 的 `bucket` 为 `day | week`；`dimensions` 为非空且无重复的 `category | app | document` 数组。响应 `buckets: [{start_date, end_date, active_ms, groups: [{dimension, entity, active_ms}]}]`；每个 dimension 独立聚合，分母为同一活跃时长。按桶日期、请求维度顺序、时长降序和实体身份排序。周桶按有效每周首日切分，首尾桶裁剪到请求范围；每周首日来自 `ui.first_day_of_week`，`auto` 解析为 Windows 区域设置，无法解析时返回 `internal`。完整桶总时长 `active_ms` 仅在该桶首次出现时返回，续页省略该字段，分组可续页；空桶返回 `active_ms: 0` 和空分组。category entity 为 `{kind:"category",category}`（沿用分类引用），app entity 为 `{kind:"app",id,exe_name,display_name}`；document entity 为 `{kind:"document", document_ref, app, document, project, profile}`，其中 app 为 ApplicationId，引用覆盖整个请求范围，响应第一页返回其 UTC `reference_range`。周桶响应摘要在查询输入摘要上附加有效每周首日；文档引用使用不含分桶设置的输入摘要，供详情查询验证。非法日期范围、空维度、重复维度或未知请求字段返回 `bad_request`。年度三维请求的实测耗时超过 `quartz-pipe` 的 2 秒响应期限，客户端可能超时。
- `query.unclassified` 的 `group_by` 为 `app | document`。只累计最终分类为空的活跃切片（人工覆盖已应用），返回 `items: [{entity, active_ms, sample_ranges}]`，entity 沿用 query.range 的 app/document 形状；每组按时序取最多 5 个 `{start_utc_ms,end_utc_ms,clock_segment_id}`，供片段归类。缺少所选实体的事实不成组，因此分组总计可以小于全日未分类时长。排序同排行；`search` 最多 256 个 Unicode 标量，对可见名称、文档、项目、profile 做 NFKC 与大小写折叠后包含匹配，过滤和分页均在核心完成。document 引用覆盖请求范围，第一页返回其 UTC `reference_range`。
- `query.annotations` 的 UTC 范围为正时长且最多 366 天，按开始 UTC、结束 UTC、UUID 排序返回相交的 `entities`；每项包含写入响应的全部字段及创建、更新时间，保留存储端点，不裁剪。包括重叠 note 和 `dismissed_away`，不按最终活动片段过滤；归档分类仍返回 UUID 引用。UI、CLI 均可读，写入权限保持 UI-only。单实体文本超过页预算时，label/note 改用 `{text,offset_utf8,complete}` 分块值，同 UUID 可跨页重复，其他字段保持一致；客户端按字段的 UTF-8 字节偏移拼接，收齐后再显示完整编辑值，字符串与 null 仍为普通完整值。

```jsonc
// query.day 第一页的引用范围摘录；versions 和引用均按不透明值处理
{"reference_range":{"start_utc_ms":1794816000000,"end_utc_ms":1795420800000},"documents":[],"versions":"opaque"}
// query.day 文档页摘录
{"applications":[],"documents":[{"document_ref":"doc:opaque","app":7,"document":"main.rs","project":"Quartz","profile":null,"active_ms":60000}],"versions":"opaque","next_cursor":null}
// document 详情请求
{"entity":{"kind":"document","document_ref":"doc:opaque"},"local_date":"2026-11-22","reference_range":{"start_utc_ms":1794816000000,"end_utc_ms":1795420800000}}
// query.annotations 长备注实体的文本字段摘录，续页 offset_utf8 接前一块末尾
{"uuid":"00000000-0000-4000-8000-000000000001","label":null,"note":{"text":"第一部分","offset_utf8":0,"complete":false}}
```

### 6.3 规则、标注与设置

| op | 说明 |
|---|---|
| `rule_sets.tree/get/versions` | 规则集树、自身或生效内容、版本列表；第 6.3.1 节 |
| `rule_sets.edit/set_enabled/reorder/delete/revert` | 带基础版本的原子写入；第 6.3.1 节 |
| `rules.preview` / `rules_text.preview` | 编辑草稿或文本预览、隐私保存回执；第 6.3.2 节 |
| `rules_text.validate/diff/import/export` | 文本校验、比较、导入、导出；第 6.3.3 节 |
| `rules.explain` | 受隐私约束的分片求值解释；第 6.3.3 节 |
| `requests.cancel` | 取消本连接的只读工作；第 6.3.2 节 |
| `data.delete.preview` / `data.delete` / `data.delete.status` | M4 仅隐私规则历史清理；第 6.3.4 节 |
| `categories.list/create/update/reorder/archive` | 分类元数据与归档；第 6.3.5 节 |
| `annotations.create` / `update` / `delete` | 可以一次提交多个不相交的范围，核心在一个事务里写入 |
| `settings.get` / `settings.set` | 设置 |
| `tracking.pause` | ui：`{until_utc_ms}`，截止 UTC 毫秒必填且必须晚于核心接纳时刻；暂停到该时刻 |
| `tracking.resume` | ui：`{}`，取消暂停 |
| `tracking.keep_alive` | ui：`{until_utc_ms}`，截止 UTC 毫秒必填且必须在未来；保持计时并抑制空闲，按领域层 `TimedState::normalized()` 规范化；无单独取消操作 |

三个 tracking 操作与 `privacy_mode.set` 在命令进入有界采集队列后返回 `{accepted:true, requested:<规范化的请求状态>}`，生效状态通过 `state` 观察。

`requested` 使用类型化联合：暂停为 `{policy:"pause", until_utc_ms}`，恢复时 `until_utc_ms` 为 `null`；保持计时为 `{policy:"keep_timing", until_utc_ms}`；隐私模式为 `{policy:"privacy", mode:{mode:"private", until_utc_ms}}`，关闭时 `mode` 为 `null`。这些操作仅允许 `ui`，`cli` 和 `host` 返回 `forbidden`；请求缺少必填值、截止时间不在未来或时间越界时返回 `bad_request`；采集队列已满返回 `busy`。

`settings.get` 和 `settings.set` 允许 `ui` 与 `cli`。`settings.get` 参数为 `{}`；`settings.set` 参数为部分更新对象，例如 `{"language":"zh-CN","hour_cycle":"h12","first_day_of_week":"sunday"}`，省略字段保持原值。`language` 可选 `auto | zh-CN | en`，`hour_cycle` 可选 `auto | h12 | h24`，`first_day_of_week` 可选 `auto | monday | tuesday | wednesday | thursday | friday | saturday | sunday`；`prewarm` 为部分更新对象，例如 `{"prewarm":{"enabled":true,"on_battery":false}}`，两个字段均为布尔值，分别保存为 `ui.prewarm.enabled` 和 `ui.prewarm.on_battery`。`resident` 是部分更新对象，例如 `{"resident":{"enabled":true}}`，布尔值保存为 `ui.resident.enabled`，默认 `true`。无效值（包括显式 `null`）返回 `bad_request`，未知字段忽略。两者返回相同结构：

```json
{
  "ui": {"resident":{"enabled":true},"language":"zh-CN","hour_cycle":"auto","first_day_of_week":"auto","prewarm":{"enabled":true,"on_battery":false}},
  "effective": {"resident":{"enabled":true},"ui_language":"zh-CN","locale":{"hour_cycle":"h24","first_day_of_week":"monday"},"prewarm":{"enabled":true,"on_battery":false}}
}
```

`ui` 返回保存值：语言与区域设置缺失时为 `auto`，预热缺失时为启用且禁止电池预热；`effective.resident.enabled` 返回有效常驻设置，`state.ui_resident` 向 UI 推送同一布尔值，供最后一个窗口关闭时判断；`effective.prewarm` 保留该策略，实际电源在悬停时读取。`effective` 与 `state` 使用相同的已解析语言和区域值。系统区域读取失败且设置仍需要自动值时，`locale` 为 `null`，核心记录警告。写入通过数据库线程的写入队列在一个事务内提交，有效保存值变化时增加元数据版本，提交后通知主线程刷新托盘并推送 `state`，响应在提交后返回。CLI 使用 `quartz settings get` 读取，使用 `quartz settings set --prewarm-enabled true --prewarm-on-battery false` 部分更新预热策略，`quartz settings set --resident-enabled false` 关闭常驻。

标注写操作只允许 `ui`。请求体与响应体如下，可选字段 `category`、`label`、`note` 缺省为 `null`：

```json
{
  "entities": [
    {
      "uuid": "00000000-0000-4000-8000-000000000001",
      "kind": "offline_activity",
      "start_utc_ms": 1763683200000,
      "end_utc_ms": 1763686800000,
      "category": { "kind": "builtin", "key": "communication" },
      "label": "会议",
      "note": null
    }
  ]
}
```

- `annotations.create` 使用上面的 `entities` 数组，每个实体必须提供 `uuid`、`kind` 和开始、结束 UTC 毫秒数。分类引用为 `{ "kind": "uuid", "uuid": "..." }` 或 `{ "kind": "builtin", "key": "communication" }`，内置键为 `development`、`learning`、`communication`、`entertainment`、`system`。
- `annotations.update` 也使用 `entities` 数组，每项提供 `uuid` 和新的范围、分类、标签、备注；省略可选字段即清空。类型保持原值；允许附带 `kind` 断言，值与已存类型不同时返回 `bad_request`。不存在的 UUID 返回 `bad_request`。同一实体的最终值未变时返回当前实体。
- 创建、更新成功返回 `{ "entities": [...] }`，每项包含 `uuid`、`kind`、范围、分类、标签、备注、`created_utc_ms` 和 `updated_utc_ms`，按请求顺序排列。分类统一返回 UUID 引用；响应仅包含请求实体，裁剪其他行产生的剩余片段不在响应中。创建遇到已有 UUID 时返回当前已有实体，并跳过该实体的字段校验和裁剪。
- `annotations.delete` 使用 `{ "uuids": ["..."] }`，成功返回 `{ "accepted": true }`；不存在的 UUID 也成功，删除不恢复此前裁剪的范围。
- 每个请求中的 UUID 必须是规范 UUID 文本，接受大写并统一为小写。同一创建或更新请求重复提交同一 UUID 返回 `bad_request`。空数组成功且不改变数据库。
- 范围必须满足 `end_utc_ms > start_utc_ms`；同一请求中同一裁剪类型的最终范围不得重叠，违反时返回 `invalid_range`。创建重试的最终范围取已有实体。note 允许重叠，不同类型允许重叠。整个请求及其对其他行的裁剪在一个事务中完成，任一项失败全部回滚。
- `category_override` 必须有分类；`offline_activity`、`active_override` 的分类可选；`dismissed_away` 不允许分类、标签或备注；`note` 的标签或备注至少一个包含非空白文本。分类不存在或已归档、字段不符合类型要求时返回 `bad_request`。当前 `bad_request` 是无附加结构字段的错误类型。
- 写通道已满时返回 `busy`。数据库循环先提交待写采集事实，再处理标注事务；提交后发布受影响 UTC 日的版本，随后回复，`day:` 订阅据此推送变化。

### 6.3.1 规则集读取与写入

规则集以全局唯一 `key` 标识，规则以 `<key>/<id>` 标识。默认覆盖目的地分别为 `user.local.classification`、`user.local.extraction`、`user.local.privacy`，各自以同 kind 的 `builtin.*` 为父。创建后 `kind`、`parent` 固定，包括删除后恢复同一 key 的情况。内置集只读；编辑器先选择或创建用户子集。

| op | 请求 body | 响应 body |
|---|---|---|
| `rule_sets.tree` | `{kind, cursor?}` | `{kind,effective_version,nodes,versions,next_cursor}`；节点含 `key,parent,version,enabled,sibling_order,source,counts:{added,overridden,disabled},warnings:[{code,target}]` |
| `rule_sets.get` | `{key, version?, view, cursor?}`，view 为 `own \| effective` | `{key, version, parent_version, entries, versions, next_cursor}`；历史 version 可读取已删除集；省略 version 时 effective 使用当前祖先版本与启用状态，返回 key 所属根树的生效视图；显式指定历史 version 时只解析该修订及其保存的祖先链 |
| `rule_sets.versions` | `{key, cursor?}` | `{revisions:[{version,parent_version,created_utc_ms,note}], versions, next_cursor}`；包含删除前历史 |
| `rule_sets.edit` | `{key, base_version, text, preview_receipt?}`，text 为完整 `quartz-rules/1` 文本 | `{sets:[{key,version,deleted}], versions}` |
| `rule_sets.set_enabled` | `{key, base_version, enabled, preview_receipt?}` | 同上；子树的有效启用状态随之变化 |
| `rule_sets.reorder` | `{siblings:[{key,base_version}], preview_receipt?}`，完整同父同 kind 列表，数组即最终顺序 | 同上；全部版本检查与写入原子完成 |
| `rule_sets.delete` | `{key, base_version, preview_receipt?}` | 同上；仅可删除无子级的用户集，保留历史 |
| `rule_sets.revert` | `{key, base_version, version, preview_receipt?}`，version 为历史来源 | 同上；生成新的当前修订 |

tree 的 effective_version 为所选 kind 的生效规则 SHA-256，privacy 树返回的该值用作历史 scope.privacy_version。三个读取操作允许 UI、CLI，均按 200 KiB 页和不透明游标返回；`versions` 覆盖注册表全部修订、启停、排序与元数据输入。tree 按同级顺序、key 的前序遍历，versions 按版本降序，own 按文件顺序，effective 先按生效顺序列有效规则，再按树与文件顺序列被停用或覆盖的条目。entries 包含 `entry_kind`（`rule | override | disable`）、`content`（格式文档对应条目的字段）、`provenance`（定义与覆盖的 key/version/id 链）和 `state`（`effective | disabled | overridden | target_missing`）；告警使用稳定代码 `target_deleted | upstream_changed | conflict` 与目标规则引用。单条目编码超过 128 KiB 时校验报 `E-RULE-VALUE`，不产生无法分页的修订。

写入仅允许 UI。`base_version = 0` 仅用于 edit/import 创建不存在或已删除的 key；恢复已删除 key 时沿用历史身份，版本从最后修订继续递增。其余写入必须使用当前非零版本。text 中 `[set].key` 必须等于请求 key。版本检查先于写入；过期返回 `rule_set_stale`，客户端读取确认结果，保留草稿，不用新基础版本自动重放。所有影响隐私保留动作的写入（包括启停、排序、删除与回退）都适用第 6.3.2 节回执要求。

```jsonc
// 新建默认分类覆盖集；一次请求只修改一个 key
{"key":"user.local.classification","base_version":0,"text":"format = \"quartz-rules/1\"\n[set]\nkey = \"user.local.classification\"\nkind = \"classification\"\nname = \"本地分类\"\nparent = \"builtin.classification\"\n"}
// stale 响应的 err；规则版本是 u32
{"code":"rule_set_stale","key":"user.local.classification","current_version":3}
```

### 6.3.2 预览、期限与回执

`rules.preview`（UI）和 `rules_text.preview`（UI、CLI）共用请求 `{mutation, start_date, end_date, expected_versions, deadline_ms?, sample_limit?, cursor?}`。日期为 1–7 个逻辑日，界面默认截至所选日的 7 日；`expected_versions` 取同范围 `query.range` 的摘要。`mutation = {op, body}` 为拟提交写操作的完整内容，省略 `preview_receipt`；rules.preview 接受 rule_sets 写操作，rules_text.preview 接受 rules_text.import。预览在临时规则快照上求值，不保存草稿。

- `deadline_ms` 为核心接纳请求起的相对期限，默认 30,000，范围 1–120,000；同样用于 `rules_text.diff` 和 `data.delete.preview`。这些请求的客户端等待上限为 deadline_ms + 500 ms，连接发现仍限 2 秒。每个分页请求重新计时；超时、取消不返回部分成功或有效回执。
- `requests.cancel {request_id}` 取消本连接的在途只读请求，返回 `{accepted:true}`；目标已结束或不存在也成功，在途只读目标以 `cancelled` 结束；指向在途写操作返回 `bad_request`。断开连接取消其只读工作，草稿变更主动取消旧代次。计算至少每 5,000 个片段检查取消与期限并让出给采集写入；单个长任务也必须检查期限。已接纳的删除任务按第 6.3.4 节继续执行。
- 初次请求的输入摘要变化返回 `stale_inputs`；翻页变化返回 `stale_cursor`。请求字段、草稿、范围和版本均进入游标；预览与正常页面共用提取、分类、活动和标注口径。
- 响应为 `{rules, totals?, further_hidden?, samples, versions, next_cursor, preview_receipt?, expires_utc_ms?}`；全局 further_hidden 与 totals 的 active_ms 只在第一页返回，分组可续页。rules 每项含规则引用、`condition_match:{segments,duration_ms}`、`effective_contribution:{segments,active_ms}`；前者累计条件成立的保留事实切片（包括 idle、被其他规则遮蔽的切片），后者对分类规则只累计最终提供分类且未被标注覆盖的活跃切片；提取规则累计被选中提取结果的活跃切片；隐私规则累计其动作参与最终进一步降级的活跃切片，同一切片可有多条隐私来源。`totals:{before,after}` 各含 `active_ms`、`categories:[{category,active_ms}]`、`applications`、`documents`，使用与 query.day 相同的排行行形状，按各自排序完整分页。after 使用草稿快照的最终结果，等于同输入与规则快照的今天页/排行口径，包含人工标注与离线活动。
- 提取预览的 samples 附 `before` / `after` 提取输出；隐私预览另返全局及逐规则 `further_hidden:{segments,fields,duration_ms}`，只计现存事实中将进一步降级的范围，包含非活跃事实。segments 是去重后的受影响切片数，fields 按 app/window_title/web 字段动作计数；时长按切片累计一次。
- `sample_limit` 默认且最多 50，可为 0。样本为 `{start_utc_ms,end_utc_ms,clock_segment_id,rule,before,after}`，before/after 只含该 kind 的允许字段；先按 UTC、时钟段、事实主键、规则引用选取前 N 项，再分页，翻页不补选更多；单页含规则统计和样本的总编码不超过 200 KiB。只展示当前与草稿都允许显示的字段，隐私预览不借样本暴露将隐藏的原文。全部统计完成后才签发回执，第一页返回回执与到期时刻。

隐私保存回执为不透明字符串，有效期 5 分钟；每连接最多保留 8 份，超出逐出最旧项，断开或核心重启即失效。回执绑定角色与连接、确切 mutation（包括原始文本字节或编辑内容、目标 key、操作与 base_version）、父链版本、三类规则输入、元数据、事实/标注版本和日期范围。提交时原子复核；缺失、过期或任一依赖变化分别返回 `preview_required`、`preview_expired`、`preview_mismatch`。保存成功消费回执；成功响应丢失后先读取当前版本，不能凭旧回执新增版本。CLI 预览可用于诊断，但 CLI 没有保存权限。

```jsonc
// rules_text.preview body；expected_versions 从同范围 query.range 取得
{"mutation":{"op":"rules_text.import","body":{"key":"user.local.privacy","base_version":2,"text":"format = \"quartz-rules/1\"\n[set]\nkey = \"user.local.privacy\"\nkind = \"privacy\"\nname = \"本地隐私\"\nparent = \"builtin.privacy\"\n[[rule]]\nid = \"bank\"\npreset = \"title_hidden\"\nmatch = \"all\"\nwhen = [{ field = \"title.raw\", op = \"contains\", value = \"网银\" }]\n"}},"start_date":"2026-11-16","end_date":"2026-11-23","expected_versions":"opaque","deadline_ms":30000,"sample_limit":50}
// 确认后提交 rules_text.import，文本与预览完全一致
{"key":"user.local.privacy","base_version":2,"text":"format = \"quartz-rules/1\"\n[set]\nkey = \"user.local.privacy\"\nkind = \"privacy\"\nname = \"本地隐私\"\nparent = \"builtin.privacy\"\n[[rule]]\nid = \"bank\"\npreset = \"title_hidden\"\nmatch = \"all\"\nwhen = [{ field = \"title.raw\", op = \"contains\", value = \"网银\" }]\n","preview_receipt":"receipt:opaque"}
```

### 6.3.3 文本与解释

| op | 请求与返回 |
|---|---|
| `rules_text.validate` | `{key,base_version,text}` → `{valid,diagnostics,truncated}`；在已安装树与分类引用中校验，不写入；最多 100 个诊断且不超过 200 KiB |
| `rules_text.import` | `{key,base_version,text,preview_receipt?}` → 与 edit 相同；以 key 匹配，隐私写入要求对应 import 预览回执 |
| `rules_text.export` | `{key,version?,cursor?}` → `{text,versions,next_cursor}`；按 UTF-8 字符边界分页，按顺序拼接为规范 `quartz-rules/1` 文本，保留条目顺序、override/disable，分类用 UUID |
| `rules_text.diff` | `{comparison,start_date,end_date,expected_versions,deadline_ms?,sample_limit?,cursor?}` → `{changes,impact,samples,sides?,versions,next_cursor}` |
| `rules.explain` | `{at_utc_ms,clock_segment_id?,cursor?}` 或 `{start_utc_ms,end_utc_ms,clock_segment_id?,cursor?}` → `{fragments,versions,next_cursor}` |

text 为 UTF-8，单个规则集的规范导出文本与导入输入均最多 192 KiB，并仍受 256 KiB JSON 帧上限约束；超长用 `bad_request`。诊断为 `{code,path,location:{line,column},params}`，行列从 1 开始，无法定位时 location 为 null；code 使用规则格式第 4.2 节的 `E-RULE-*`，不包含翻译后的句子。validate 以 valid=false 返回诊断；其他操作的校验失败使用 `rule_validation_failed`，同样限制 100 项与 200 KiB 并带 `truncated`。

comparison 为 `{kind:"draft",mutation}`、`{kind:"versions",key,before_version,after_version}` 或 `{kind:"upstream",key,version}`。draft 比较当前与临时修改，versions 比较同一 key 两个历史修订（各用其保存父链）；upstream 的 `sides:{saved_parent,current_parent,overlay}` 各为按父链顺序排列的 `{key,version,entries}` 列表，返回旧父链、当前父链、用户差异三侧的版本与分页条目。changes 每项为 `{rule,change,paths}`，change 为 added/removed/changed，paths 为变更字段路径；按规则引用和字段路径排序。`impact:{category_deltas:[{category,before_active_ms,after_active_ms}],document_changes,further_hidden}` 返回分类活跃时长变化、提取身份变化数、隐私进一步隐藏统计；各列表完整分页，汇总只在第一页返回。samples 使用第 6.3.2 节的日期、版本、50 项与期限约束。M7 再加入判例结果。

explain 范围为正时长且最多 48 小时；单时刻解释包含该时刻的全部事实，时钟回拨产生多个结果时分别返回 clock_segment_id，客户端可用此字段缩小选择。不同结果分片返回，按时间轴边界顺序分页；片段内以规则集 → 规则 → 条件列出实际或缺失字段、条件真假、提取与分类来源、标注优先级、历史隐私归因，以及生效、遮蔽、停用、被覆盖状态。fragments 每项为 `{start_utc_ms,end_utc_ms,clock_segment_id,category,category_source,extraction,privacy,nodes}`，privacy 使用历史存储的规则 ID 与标签。nodes 为扁平树节点 `{path,kind,rule,condition_index,field,value,missing,matched,state}`，path 是从根起的整数索引数组，kind 为 set/rule/condition；不适用字段为 null。state 为 effective/shadowed/disabled/overridden，条件节点以 matched 给出真假。节点按树与条件顺序分页，续页重复所属片段的范围与来源，nodes 只返回剩余部分。保留字段上限与隐私投影沿用原始详情。

### 6.3.4 历史隐私清理

三个操作仅允许 UI；M4 的 `scope` 固定为 `{kind:"privacy_rules",privacy_version}`，对已接受的当前生效隐私快照重算保留事实。其他 kind 返回 `delete_scope_unsupported`，通用按应用、标题或日期删除留到 M6。执行与恢复语义见 [06 第 4.2 节](06-privacy.md#42-历史隐私清理)。

| op | 请求 body | 响应 body |
|---|---|---|
| `data.delete.preview` | `{scope,start_utc_ms,end_utc_ms,expected_versions,backup_ids,deadline_ms?,sample_limit?,cursor?}` | `{impact,backups,samples,versions,next_cursor,preview_receipt,expires_utc_ms}`；正时长且不超过 366 天的半开 UTC 范围，expected_versions 来自同范围 query.timeline（detail=category、ms_per_pixel=1）；impact 使用 further_hidden 口径 |
| `data.delete` | `{uuid,scope,start_utc_ms,end_utc_ms,backup_ids,preview_receipt,confirmed:true}` | `{accepted:true,job}`；持久接纳后在普通 2 秒期限内响应 |
| `data.delete.status` | `{uuid}` | `{job}`；普通 2 秒期限，只读已持久任务状态 |

backup_ids 是无重复的 ID 集合，最多 128 项，比较确认内容时忽略排列顺序。backups 为按备份 ID 排序的有界分页清单，每项 `{id,name,created_utc_ms,size_bytes,selected}`；id 由核心签发，不接受任意文件路径。首次 preview 可传空 backup_ids 查看清单，选择后重新预览。回执除第 6.3.2 节依赖外绑定 scope、UTC 范围及所选备份 ID/内容指纹，执行复核这些输入。scope.privacy_version 与当前哈希不符返回 `preview_mismatch`（reason=inputs）；输入摘要已变化使用 `stale_inputs`。预览显示完整影响计数，样本最多 50，备份清单另行完整分页；未选备份保留。

job 为 `{uuid,state,phase,processed,total,backup_ids,failure,updated_utc_ms}`。state 为 `queued | running | failed | completed`；phase 为 `barrier | facts | dictionaries | checkpoint | backups | done`，processed/total 为当前阶段的项数，阶段切换重置。failure 成功时为 null，失败时为 `{code,backup_id?}`，code 固定为 `database_write_failed | checkpoint_blocked | backup_delete_failed`。failed 保留进度与屏障，UI 显示失败阶段；同一 data.delete 请求重试可恢复。

UUID 去重先于回执校验：已存在且确认内容相同则返回原 job（failed 时从失败阶段重新排队），即使回执已消费或核心已重启；不同 scope、范围或备份选择返回 `delete_job_conflict`。未接纳时断线或超时可用同一 UUID 重试；接纳时固定清理动作与规则快照；后续规则变化不扩大本任务范围。任务独立于连接，核心重启从已持久阶段恢复。不存在取消或撤销已接纳任务的操作。completed 只在主库降级、字典与缓存清理、WAL truncate 和所选备份删除全部成功后返回。

```jsonc
// data.delete：历史任务使用独立回执，不能用规则保存回执替代
{"uuid":"00000000-0000-4000-8000-000000000004","scope":{"kind":"privacy_rules","privacy_version":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"start_utc_ms":1794787200000,"end_utc_ms":1795392000000,"backup_ids":[],"preview_receipt":"history:opaque","confirmed":true}
// data.delete.status 响应示例
{"job":{"uuid":"00000000-0000-4000-8000-000000000004","state":"running","phase":"facts","processed":2000,"total":4500,"backup_ids":[],"failure":null,"updated_utc_ms":1795392001000}}
```

### 6.3.5 分类操作

分类操作仅允许 UI；读取 `categories.list {include_archived?,cursor?}`（include_archived 默认 false）返回 `{entities,versions,next_cursor}`，按层级前序、sort_order、UUID 排序并完整分页。每页沿用 200 KiB 上限；`versions` 为 `categories.v1:<metadata_version>`，游标绑定该版本及 `include_archived`，元数据版本变化返回 `stale_cursor`，参数或位置无效返回 `bad_request`。实体为 `{uuid,builtin_key,parent_uuid,name,color,tendency,sort_order,archived}`，内置 name 可为 null，按 builtin_key 本地化。

- `categories.create {entity}`：客户端提供 uuid、parent_uuid、name、color、tendency；tendency 缺省 neutral，追加到同级末尾，返回 `{entity}`。UUID 已存在先返回已有实体。
- `categories.update {entity}`：uuid、parent_uuid、name、color、tendency 为完整最终值；返回 `{entity}`。内置分类 name=null 恢复默认名称；builtin_key 固定，不允许客户端创建内置身份。
- `categories.reorder {parent_uuid,uuids}`：给出同父分类的完整最终顺序（含归档项），原子更新，返回 `{accepted:true}`。
- `categories.archive {uuid,archived}`：归档或恢复，返回 `{entity}`；不提供 categories.delete，记录与规则、标注引用保留。只改变指定分类，子级保留父引用与各自归档状态。归档项从新规则与标注选择中隐藏；已有引用的查询行为见 03 第 5.1、5.3 节。

不存在的 UUID、空白或超过 256 个 Unicode 标量的用户名称、非法色板或倾向、无效父级及层级环返回 `bad_request`，任一失败回滚。color 为 cat.1–cat.14 或 neutral，tendency 为 focus/neutral/distraction。重复最终值不改元数据版本；真实变化同事务增加 metadata_version，刷新规则引用目录、托盘与 day 订阅。

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
| `privacy_mode.set` | ui | `{enabled, until_utc_ms?}`；启用时截止 UTC 毫秒必填且必须在未来 | 启用请求使用 `private_window` 组合（`PrivacyMode::Private`），关闭请求取消该策略；按 §6.3 返回接纳回执，采集线程触发 `policy_changed` |

备份和压缩由核心执行。临时保持计时、暂停和隐私模式及其截止时间保存在 `timed_policies`，重启后按截止时间恢复。

## 7. 订阅

```jsonc
{ "t": "req", "id": 5, "op": "subscribe", "body": { "topics": ["state", "day:2026-11-16"] } }
```

| topic | 推送内容 | 节流 |
|---|---|---|
| `state` | 当前前台应用、标题（受隐私级别约束）、是否在记录、停止原因、扩展连接状态、当前界面语言与区域设置（10 第 1 节）、当前逻辑日期 | 变化即推送，最多每 250 ms 一次，只保留最新值 |
| `day:<日期>` | 当天数据有变化的提示，带新的版本号 | 最多每 5 秒一次；界面收到后自行重新请求需要的查询 |
| `control` | `body.op` 指定的界面或浏览器控制消息 | 不节流，不合并 |

- `unsubscribe` 携带与 `subscribe` 相同的 `{topics}` 结构，幂等移除指定主题并返回 `{}`；已在写出的推送可以先于响应到达。UI 生命周期 `control` 保持隐式发送。
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
| `logical_date` | `YYYY-MM-DD`；核心按当前系统时区和有效 `day.start_offset_min`，使用与 `query.day` 相同的日历转换计算当前逻辑日期；UI 用于“今天”和禁止选择未来日期 |
| `foreground` | 未跟踪时为 `null`；否则包含已清洗 `IntervalFact` 中的 `app`、`title`，以及实际区间边界 `interval_start_utc_ms`（UTC 毫秒） |
| `foreground.app` | 被隐私规则移除时为 `null`；否则为 `{kind, value}`，`kind` 是 `exe`、`aumid` 或 `unresolved`，`value` 是字符串 |
| `foreground.title` | 隐私处理后的标题，移除时为 `null` |
| `recording`、`stop_reason` | 与 `core.status` 共用判定和优先顺序 |
| `timed_policies` | 采集线程的当前限时策略快照：`{pause_until_utc_ms, privacy_mode, keep_timing_until_utc_ms}`；未启用的策略为 `null`，截止值为 UTC 毫秒；`privacy_mode` 启用时为 `{mode:"private", until_utc_ms}` |
| `browser_streams` | 与 `core.status` 共用的类型化列表，M5 前为空 |
| `ui_resident` | 有效 `ui.resident.enabled` 布尔值，默认 `true`；UI 在最后一个窗口关闭时决定隐藏并通知核心或退出进程 |
| `ui_language` | 与托盘采用相同的 Windows 首选语言协商，生产值为 `zh-CN` 或 `en`；调试伪语言使用 `en` 的协议身份 |
| `locale` | `{hour_cycle, first_day_of_week}`，值分别为 `h12` 或 `h24`，以及 `monday` 至 `sunday` 的英文星期名；分别解析用户区域的 `LOCALE_STIMEFORMAT`、`LOCALE_IFIRSTDAYOFWEEK`；读取失败时记录警告并发送 `null` |

逻辑日的下次 UTC 起点保存在主线程 `StatusPublisher` 持有的 `LogicalDay::next_start_utc_ms`，作为 `MsgWaitForMultipleObjects` 的等待期限；到期后重新计算日期和下次起点，并仅在日期变化时通过原有 watch 发布状态。Windows 时钟变更、恢复和系统设置变更通知会清除时区缓存并重算；数据库线程在已提交版本变化中发现日起点偏移改变时通知主线程重算。已有日期后，读取时区或时钟失败时记录类型化警告，保留上次发布的日期、期限和待应用的偏移，在下一条时钟、日历、偏移事件或主线程唤醒时重试；已过期的保留期限在恢复期间不触发零等待循环。首次启动尚无日期时，初始计算失败作为启动错误返回。该期限独立于采集线程用于存储拆分的 UTC 午夜期限，无需轮询或采集状态变化。

主线程通过 watch 发布已清洗的区间和状态；有 state 订阅者时，注册表在每次变化后构造一份状态快照并立即转发到各连接的最新值槽，day 变化也立即转发到对应槽。每个连接的 outbox 从实际写入完成时计时，按上述间隔限流，并仅保留各槽的最新值。主线程将 `ui.show`、`ui.activate` 连同首个已登记 UI 的连接标识非阻塞地投递给注册表，注册表仅向该连接的 outbox 发送；后续 UI 登记不替换首个连接。预热 TTL 到期或关闭隐藏常驻时，`ui.shutdown` 仅发给对应连接；托盘退出向全部 UI 连接广播 `ui.shutdown`，停止管道前在现有关闭期限内尽力写出控制消息。控制事件不要求显式订阅 `control`。

## 8. 其他客户端的连接恢复

- 正常连接期间不发心跳。
- host 连接失败就回 `core_offline` 并退出。扩展负责 1–60 秒退避与 alarm 唤醒，见 05 第 1.3 节。

## 9. 测试

- 分帧编解码：边界长度、超限、短读、半帧、非 UTF-8、深层嵌套 JSON。
- 权限：用一个非 Quartz exe、其他用户或其他会话连接应被拒绝；未登记扩展 ID 被拒绝；`host` 调用 `data.delete` 应返回 `forbidden`。Edge 直接启动与 `cmd.exe` 中转分别验收。
- 创建已提交但响应丢失后，重启核心并以同一 UUID 重试，实体不重复；分页中修改事实或元数据，旧游标返回 `stale_cursor`。
- 慢客户端：一个连接停止读取 10 分钟，核心的采集和写入延迟不受影响，发送预算不超过 1 MiB。自动测试将此场景缩短为数秒，检查发布耗时、其他连接的状态响应延迟、预算峰值和响应洪泛后的隔离关闭；实际 10 分钟运行纳入 M2-F 验收。
