# 04 IPC 协议

核心是唯一的服务端，界面与 CLI（以及后续 M5 的 `quartz-host.exe`）均为客户端。所有 wire 消息类型、错误码、操作名与常量以 `crates/quartz-protocol/src/` 为准；本文仅记录传输与安全边界、协议不变量、角色权限和已实现操作清单。

## 1. 传输

- **管道名**：`\\.\pipe\Quartz.<当前用户 SID>.v1`。SID 仅用于区分同机不同用户；访问控制依靠 DACL，不依靠管道名。
- **模式与实例**：字节模式、双向，最多 8 个连接槽位；首个实例设置 `FILE_FLAG_FIRST_PIPE_INSTANCE` 防止抢注，所有实例设置 `PIPE_REJECT_REMOTE_CLIENTS`。
- **安全描述符**：`D:P(A;;GA;;;SY)(A;;GA;;;<用户 SID>)`，仅授予 SYSTEM 与当前用户访问权限，不继承默认 DACL。
- **双向身份校验**（实现见 `crates/quartz-pipe/` 与 `crates/quartz/src/platform/windows/ipc/`）：
  - 服务端通过 `GetNamedPipeClientProcessId` 检查对端与核心同用户、同 Windows 会话，且经 `canonicalize` 规范化并忽略大小写序数比较（`CompareStringOrdinal`）后的完整可执行路径等于核心自身的 `current_exe()`（允许 `ui` / `cli` 角色）或同目录的 `quartz-host.exe`（仅允许 `host` 角色）。
  - 客户端通过 `GetNamedPipeServerProcessId` 反向验证服务端同用户、同会话且路径为自身 `current_exe()`。
  - 路径或身份查询失败直接关闭连接，日志只记 PID 与失败原因。路径校验用于排除误连；真正的安全边界是 Windows 用户账户。
- **认证隔离**：对端身份校验在核心阻塞线程池中最多占用 1 个线程（为已认证连接的规则读取保留至少 1 个线程），并与完整 `hello` 共用连接后 5 秒期限。

## 2. 分帧

```text
u32 小端（JSON 字节长度） | UTF-8 JSON
```

- **单帧上限**：`MAX_FRAME_LEN = 1 MiB`（`crates/quartz-protocol/src/frame.rs`）。先检查长度头再分配缓冲区；超限时回发 `frame_too_large` 错误帧并断开。
- **短读与半帧**：编解码器处理任意切分；连接断开时丢弃半帧。格式与浏览器 Native Messaging 兼容（Windows 上同为 u32 小端）。
- **错误帧后关闭**：连接级错误帧排入发送后限时（500 ms）写出并刷新（最多占用 1 个临时刷新线程）再调用 `DisconnectNamedPipe` 清理并复用槽位；超时或异常则停用槽位待资源释放后恢复，不影响其余槽位、采集和数据库写入。
- **管线化与背压**：每个连接按帧到达顺序受理请求并并发等待回复，最多 64 个在途待响应请求，超出返回 `busy`。客户端单次发送写入与刷新受 2 秒期限约束，未完成即标记连接失效；已发送完成的请求若等待超时或取消，仅移除在途编号并丢弃迟到响应，连接可继续使用。

## 3. 消息信封与错误

消息分为请求（`t: "req"`，含 `id`/`op`/`body`）、响应（`t: "res"`，含 `id`/`ok` 及 `body` 或 `err`）、进度（`t: "prg"`，含 `id`/`done`/`total`，要求 `done <= total` 且 `total >= 1`）和推送（`t: "evt"`，含 `topic`/`seq`/`body`），定义见 `crates/quartz-protocol/src/envelope.rs`。

**进度帧**：多日查询作业（`query.range`、`query.unclassified`、`query.entity_detail`、`rules.preview`）仅向 `ui` 角色连接发送 `prg` 帧（`cli` 连接永不接收），在作业运行满 250 ms 后按最多每 250 ms（≤ 4 次/秒）上报已完成逻辑日数，且绝不在对应响应之后发出；经连接 outbox 非阻塞投递，发送预算满时直接丢弃（可丢弃，最终响应仍可靠）。客户端收到在途请求 ID 的 `prg` 帧时将该请求的 2 秒响应期限重置为当前时刻 + 2 秒，未知请求 ID 按协议错误处理。

**兼容性与不变量：**

- **未知字段**：响应、推送和信封忽略未知字段以向前兼容；请求体默认拒绝未知字段并返回 `bad_request`（明确允许部分更新的操作如 `settings.set` 除外）。
- **稳定错误码**：核心只发送稳定字符串 `code` 与同级结构化字段；`msg` 仅作可选日志诊断文本，UI/CLI/host 均不得按 `msg` 分支或呈现。客户端将不认识的未来错误码解码为 `Unknown`，核心永不发送 `Unknown`。
- **`id: null` 限制**：仅用于无法取得请求 ID 的连接级错误（`frame_too_large`、JSON 解码失败的 `bad_request`）；客户端收到后记录错误码并结束连接。其余响应必须携带原请求 ID。
- **幂等与并发**：`categories.create` 与 `annotations.create` 由客户端提供规范 UUID，数据库唯一约束去重；`annotations.delete` 删除不存在的 UUID 仍返回成功；规则集写入按 `key` + `base_version` 做乐观并发检查。

已实现错误码（权威定义：`crates/quartz-protocol/src/error.rs`）：

| `code` | 含义 | 结构化字段 |
|---|---|---|
| `bad_request` | 信封、字段、值或范围不符合参数约束 | 无 |
| `unknown_op` | 未识别的操作名 | 无 |
| `handshake_required` | 连接首帧不是 `hello` | 无 |
| `frame_too_large` | 帧字节数超过 1 MiB | 无 |
| `page_too_large` | `query.facts` 单条事实超过 1 MiB 帧上限 | 无 |
| `proto_mismatch` | 客户端与核心协议范围无交集 | `supported: {min, max}`、`core_version` |
| `forbidden` | 当前角色无权执行该操作 | 无 |
| `invalid_range` | 请求的日期或 UTC 时间范围无效/超限 | 无 |
| `stale_document_ref` | 文档引用绑定的查询输入已变化，需重查 | 无 |
| `rule_set_stale` | 规则集 `base_version` 过期 | `key`、`current_version`（u32，不存在/已删为 0） |
| `rule_set_not_found` | 规则集不存在 | `key` |
| `rule_set_read_only` | 试图修改内置规则集 | `key` |
| `rule_set_has_children` | 试图删除仍有活跃子集的规则集 | `key` |
| `rule_set_identity` | 已有 `key` 的 `kind` 或 `parent` 不一致 | `key`、`field` |
| `rule_validation_failed` | 规则文本或树未通过校验 | `diagnostics`、`truncated` |
| `version_exhausted` | 持久版本计数达到上限，事务回滚 | 无 |
| `busy` | 核心队列满或暂时过载，稍后重试 | 无 |
| `backup_exists` | 手动备份目标文件已存在，拒绝覆盖 | 无 |
| `host_executable_missing` | `browser_hosts.enable` 时 `quartz.exe` 同目录没有 `quartz-host.exe` | 无 |
| `internal` | 核心内部错误 | 无 |

管道建立前的核心启动致命错误通过 `quartz.exe ui --core-error <code> [--backup-path <路径>...]` 传递给 UI，代码定义在 `crates/quartz-protocol/src/startup_error.rs`（`newer_database`、`migration_failed`、`other_session`、`core_start_failed`）。

### 3.1 版本与数值

- 规则修订号（`version`、`base_version`、`parent_version`、`current_version`、`builtin_revision`）与协议/查询语义版本统一为 `u32`；已保存规则版本从 1 开始，0 仅作创建/恢复哨兵。
- 日数据、元数据与统计策略版本计数为 `u64`，SQLite 持久上限为 `i64::MAX`，递增溢出拒绝事务并返回 `version_exhausted`。
- UTC 毫秒、时长与实体主键使用 `i64`；JSON 以无损整数传输。
- 规则集生效哈希为 64 位小写 SHA-256 文本；查询响应的 `versions` 为不透明摘要字符串，仅供一致性比较与文档引用绑定。

## 4. 握手

定义见 `crates/quartz-protocol/src/handshake.rs`。

- 连接后 5 秒内第一帧必须是 `hello`，否则断开；握手完成后重复发送 `hello` 返回 `bad_request` 且保持连接。
- 双方各声明闭区间 `proto: {min, max}`（`min <= max`），协商取交集中的最高版本；无交集则返回 `proto_mismatch`（附核心 `supported` 与 `core_version`）并断开。当前协议版本为 1，与数据库 schema 版本独立演进。
- 成功响应返回协商后的 `proto`、`core_version` 及核心每次启动重新生成的 `instance_id`。
- 协议只传稳定键、编码、数字与原始数据，不传翻译后的展示句（见 10 第 3 节）。UI 握手携带 `ui.state`（`starting | prewarming | visible`）用于生命周期登记，握手后订阅 `state` 取得当前 `logical_date`。

## 5. 角色与权限

角色在 `hello` 中声明，权限按操作族与逐项白名单校验（权威定义：`crates/quartz-protocol/src/role.rs`）。未知操作先返回 `unknown_op`，再做权限检查。

| 角色 | 允许的操作 |
|---|---|
| `ui` | 全部已实现操作：`hello`、`subscribe`/`unsubscribe`、`core.*`、`ui.*`、`query.*`、`rule_sets.*`、`rules.*`、`rules_text.*`、`categories.*`、`annotations.*`、`settings.*`、`tracking.*`、`privacy_mode.*`、`data.*`、`browser_hosts.*` |
| `cli` | `hello`、`core.status`、`ui.open`、`query.*`、`settings.get`、`settings.set`、`rule_sets.tree`、`rule_sets.get`、`rules_text.validate`、`rules_text.export` |
| `browser` | `hello`、`browser.state` |

`browser` 角色只接受同目录 `quartz-host.exe` 的连接，帧上限 256 KiB，握手 `hello` 须带 `browser {browser, profile_instance_id}`（见 05-browser.md）。后续模块使用独立前缀与主题（M7 自控模块使用 `focus.*`、`policy.*` 与 `selfreg` 主题，见 12 第 7 节）。

## 6. 消息清单

所有请求与响应字段以 `crates/quartz-protocol/src/`（`op.rs`、`query.rs`、`rules.rs`、`categories.rs`、`annotations.rs`、`settings.rs`、`timed.rs`、`data.rs`、`status.rs`、`facts.rs`）为准。

### 6.1 核心与界面控制

| op | 方向 | 用途 |
|---|---|---|
| `core.status` | 请求 | 读取主线程不可变快照（不查 SQLite）：记录状态、停止原因、数据库状态、实例与版本 |
| `ui.open` | 请求（UI/CLI） | 按托盘“打开”路径显示或激活界面，入主线程队列后返回接受 |
| `ui.first_frame` | 请求（UI） | 主窗口首帧渲染完成时上报，将登记状态置为可见并清除显示请求 |
| `ui.hidden` | 请求（UI） | 常驻开启且最后窗口关闭时上报，将登记状态转为无 TTL 的 `Hidden` |
| `ui.show` | `control` 推送 | 通知启动/预热/隐藏中的已登记 UI 创建并显示主窗口 |
| `ui.activate` | `control` 推送 | 通知已可见 UI 还原最小化窗口并请求前台激活 |
| `ui.shutdown` | `control` 推送 | 退出 Quartz 时广播；预热 TTL 到期或关闭常驻时定向发送 |
| `browser.state` | 请求（`browser`） | 上报当前浏览器窗口与活动标签状态，经校验入采集队列后返回接受 |

### 6.2 查询

**通用不变量：**

- **单响应 + 上限 + 截断**：除 `query.facts` 外，所有查询在只读 WAL 快照上分批计算（批间让出给采集写入）并以单帧返回完整结果。各列表受协议显式上限约束；若整帧编码仍超过 1 MiB，核心截断最长列表并置 `truncated: true`，不报错（且截断列表不影响汇总指标 `metrics` / `hours` / 桶 `active_ms`）。
- **时区与逻辑日**：统一使用核心当前系统时区与 `day.start_offset_min`；日期范围上限 366 个逻辑日（`query.segment_detail` 上限 48 小时）。版本摘要覆盖查询/引用范围内全部输入版本及 `QUERY_SEMANTICS_VERSION`。
- **`query.facts` 位置游标**：仅原始事实导出使用简单位置游标分页（请求带 `after` 位置键与 `limit`，默认 500、上限 2,000；遇 1 MiB 帧上限提前截页并返回 `next_after`；单条超限返回 `page_too_large`），不绑定版本摘要。
- **不透明 `document_ref`**：`query.day`、`query.timeline`、`query.range`、`query.segment_detail`、`query.unclassified` 返回的文档引用绑定可见文档身份、`reference_range` 与输入版本摘要；客户端只保存、比较并原样回传给 `query.entity_detail`，依赖变化时返回 `stale_document_ref`。

| op | 用途 |
|---|---|
| `query.facts` | 按位置游标分页读取日历日原始事实（供 `quartz dump` 与诊断） |
| `query.day` | 返回单逻辑日指标、小时分类堆叠、应用/文档/网站（`websites`）排行及 7 日 `reference_range` |
| `query.timeline` | 按 `category`/`app`/`raw` 细节与 `ms_per_pixel` 合并规则返回 UTC 范围时间轴片段 |
| `query.range` | 按天/周分桶及 `category`/`app`/`document` 维度返回日期范围聚合 |
| `query.segment_detail` | 返回选定 UTC 范围（≤48 h）裁剪后未像素合并的明细区间（可按应用/窗口实例过滤） |
| `query.entity_detail` | 返回 `app` 或 `document` 截至指定逻辑日的 7 日趋势、Top 标题、生效分类规则与提取来源 |
| `query.unclassified` | 按 `app` 或 `document` 汇总日期范围内未分类活跃时长与最多 5 个样本范围（支持搜索） |
| `query.annotations` | 返回与 UTC 范围相交的已保存标注完整实体（不裁剪、含重叠 `note` 与 `dismissed_away`） |
| `query.pending_away` | 返回单逻辑日内未标注且 ≥15 分钟的连续 `idle` 片段 |

### 6.3 规则、标注与设置

| op | 用途 |
|---|---|
| `rule_sets.tree` | 返回指定 `kind` 的规则集树、生效版本哈希、增改停计数及树告警 |
| `rule_sets.get` | 读取单个活跃规则集的自身条目（`own`）或所属根树的生效条目（`effective`） |
| `rule_sets.edit` | 带 `base_version` 原子创建/恢复（0）或编辑用户规则集（`quartz-rules/1` 文本） |
| `rule_sets.set_enabled` | 带 `base_version` 启停用户规则集及其整个子树 |
| `rule_sets.delete` | 带 `base_version` 软删除无活跃子级的用户规则集（保留历史版本） |
| `rules.preview` | 在 1–7 个逻辑日只读快照上预览规则变更草稿的命中、有效贡献、文档/隐私变化与脱敏样本（仅 UI） |
| `rules_text.validate` | 在已安装树与分类目录中上下文校验规则文本，不写入；已安装内置 key 允许只读校验 |
| `rules_text.import` | 带 `base_version` 导入规范 `quartz-rules/1` 规则文本（与 `rule_sets.edit` 共用语义） |
| `rules_text.export` | 单帧导出指定活跃规则集的规范 `quartz-rules/1` 文本（分类写 UUID） |
| `categories.list` | 按层级前序列出分类（可选含归档） |
| `categories.create` | 客户端带 UUID 幂等新建用户分类 |
| `categories.update` | 按 UUID 整体更新分类名称、颜色、倾向或父级（内置分类 `name: null` 恢复默认名） |
| `categories.archive` | 非破坏性归档或恢复分类（保留历史与引用，不提供删除） |
| `categories.reorder` | 原子更新同一父级下子分类的完整顺序 |
| `annotations.create` | 事务内批量幂等创建标注，按类型执行同类型裁剪（见 03 第 5.1 节） |
| `annotations.update` | 事务内按 UUID 整体更新标注（`kind` 创建后固定，省略可选字段即清空） |
| `annotations.delete` | 按 UUID 批量删除标注（不存在亦成功，不恢复既往裁剪） |
| `settings.get` | 读取已保存与已解析生效的界面/区域/常驻/预热设置、标题保留期（`title_retention`），以及实时读取注册表得到的开机自启状态 |
| `settings.set` | 部分更新界面设置、标题保留期（`forever`/`30d`/`90d`/`365d`，拒绝 `null`；变更提交后立即触发一次清理）或开机自启注册表项（省略字段保持原值，忽略未知字段，提交后刷新托盘与 `state`） |
| `tracking.pause` | 设置暂停采集至未来指定 UTC 毫秒（经采集队列接纳，状态由 `state` 观察） |
| `tracking.resume` | 立即取消暂停 |
| `tracking.keep_alive` | 设置保持计时并抑制空闲至未来指定 UTC 毫秒 |
| `privacy_mode.set` | 启停限时全局隐私模式（`private_window` 组合，触发 `policy_changed`） |
| `data.info` | 返回主库路径、主库与 `-wal` 总字节数、日志/备份目录路径及按最新优先排序的迁移/手动备份列表（仅 UI） |
| `data.backup` | 以 `VACUUM INTO` 写出不覆盖的 `backups\manual-<UTC yyyymmddThhmmssZ>.db` 并返回该备份条目（仅 UI） |
| `data.compact` | 在写连接执行 `VACUUM` 与 `PRAGMA wal_checkpoint(TRUNCATE)`，返回整理前后字节数（仅 UI） |
| `data.delete_range` | 按半开逻辑日期范围 `[start_date, end_date)` 原子删除并裁剪事实、清理无引用字典与截断 WAL，返回已删前台时长与更新后版本摘要（仅 UI） |
| `browser_hosts.status` / `.enable` / `.disable` | 读取或写入 Chrome、Edge、Firefox 的 Native Messaging 注册（manifest + HKCU 键），返回三个浏览器的注册状态与连接状态（仅 UI，见 05 第 2.1 节） |

**规则文本上限与校验**：单规则集导入/导出文本上限 192 KiB（`MAX_RULE_TEXT_BYTES`），保证 JSON 编码后稳妥落入 1 MiB 帧上限；校验诊断最多 100 条且 ≤200 KiB，超限时置 `truncated: true`。

**数据维护前提与耗时**：`data.delete_range` 要求 `start_date < end_date <= 当前逻辑日`（仅允许已结束的完整逻辑日，不可删除今天或未来）且跨度 ≤ 366 天（`RANGE_MAX_DAYS`），不满足返回 `bad_request`；`data.*` 在数据库写线程串行执行，`data.compact` 与大范围 `data.delete_range` 在大库上可能阻塞其他写入数秒（采集写入队列会缓冲并按忙重试，不丢数据）。历史隐私清理留待以后；M5 浏览器扩展协议见 [05-browser.md](05-browser.md)。

## 7. 订阅

`subscribe` 与 `unsubscribe` 接受 `{topics}` 并返回 `{}`（权威定义：`crates/quartz-protocol/src/topic.rs`）；非法主题返回 `bad_request`。

| `topic` | 推送内容 | 节流与合并 |
|---|---|---|
| `state` | 已清洗的前台状态、记录/停止原因、限时策略、逻辑日期、语言与区域设置、`ui_resident` | 订阅后立即推快照；此后每连接 ≥250 ms，仅保留最新值 |
| `day:<YYYY-MM-DD>` | 指定逻辑日的数据版本摘要 `{date, versions}` | 订阅后立即推快照；此后每日期 ≥5 s，仅保留最新值 |
| `control` | `body.op` 指定的界面生命周期控制事件 | 不节流、不合并；对已握手 `ui` 连接隐式发送 |

**慢客户端与 Outbox 不变量**（实现见 `crates/quartz/src/ipc/outbox.rs`、`registry.rs`）：

- **发送预算**：每个连接固定 1 MiB 总发送预算（含 4 字节帧头、排队帧与正在写出的帧）。
- **可靠与可覆盖分离**：请求响应与 `control` 共用 FIFO 可靠队列，入队前预留预算；可靠消息超出预算时直接断开该慢连接。`state` 与每个已订阅 `day:` 各自使用未编码的最新值槽，新值覆盖旧值；仅在可靠队列为空时编码发送，并按实际写入完成时刻起算 250 ms / 5 s 间隔，避免恢复后突发补发。
- **隔离保证**：连接注册表扇出仅做非阻塞投递，慢客户端绝不阻塞采集线程、数据库线程或其他连接；无订阅者时不构造推送载荷。
- **顺序与轻量**：`subscribe` 的 `{}` 响应先于首个快照写出；客户端根据 `state` 的区间起点自行推算已持续时长，核心不发秒级推送。

## 8. 连接恢复

- 正常连接期间不发送心跳。
- UI 首次连接失败后展示错误界面等待用户显式重试或启动核心；已连接后的管道断开触发 UI 关闭退出，不做静默自动重连（见 01 第 4 节）。
- M5 的 `quartz-host.exe` 连接失败即回报 `core_offline` 并退出，由浏览器扩展退避重连（见 05 第 1.3 节）。

## 9. 测试

- **分帧编解码**：边界长度（0 与 1 MiB）、超限拒绝不预分配、短读与半帧、非 UTF-8、深层嵌套 JSON。
- **身份与权限**：非 Quartz 可执行文件、跨用户/跨会话连接被拒；`cli` 调用写操作返回 `forbidden`；未知操作先于权限返回 `unknown_op`。
- **幂等与并发**：UUID 创建提交后断线重试不重复；`rule_sets.*` 过期 `base_version` 返回 `rule_set_stale`。
- **慢客户端隔离**：单一连接停止读取时发送预算不超过 1 MiB，可靠消息溢出后仅断开该连接，采集写入与其他连接响应不受影响。
