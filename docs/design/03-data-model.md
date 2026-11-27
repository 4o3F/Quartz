# 03 数据模型

核心原则：**事实只记一次，不改写；派生结果都可以从事实与规则重建。**

数据存放在单文件 SQLite 数据库 `%LOCALAPPDATA%\Quartz\quartz.db`，仅核心的数据库线程持有写连接。完整列定义、约束与索引以 `crates/quartz/migrations/` 和 `crates/quartz/src/store/schema.rs` 为准；本文只记录分层决策、非显而易见的语义与不变量。

## 1. 约定

- **时间与区间**：所有时间戳均为 UTC 毫秒整数（`_utc_ms`），区间统一左闭右开 `[start, end)`。采集事实允许零时长但不计入统计；标注强制 `end_utc_ms > start_utc_ms`。
- **时钟段与单调时长**：前台事实的 `duration_ms = end_mono_ms − start_mono_ms`（开放行以确认点单调时间计算），基于 `QueryUnbiasedInterruptTime`。系统校时、休眠恢复或时钟漂移开启新的 `clock_segments`：统计在各时钟段内独立累计，UTC 裁剪仅在段内执行，跨段重叠的墙上时间不相互抵消，时间轴标明时钟异常。
- **UTC 午夜拆分与逻辑日**：已结束的前台事实与可用状态都在 UTC 午夜拆分，保证单行最长 24 小时，由 `session_key` 串联同一连续会话。逻辑日由本地时区加 `day.start_offset_min`（默认 0，范围 0–720 分钟）定义，属于查询语义，存储层与偏移无关。
- **确认点语义**：开放行（`end_utc_ms IS NULL`）必须有非空的 `confirmed_utc_ms`（及 `confirmed_mono_ms`）；行关闭时确认点置 `NULL`，由表级 `CHECK ((end_* IS NULL) = (confirmed_* IS NOT NULL))` 强制约束，避免已结束行重复存时间戳。
- **整数枚举与视图**：闭集枚举存小整数（从 1 起，只追加不改号不复用），名称登记在 `enum_labels`，人工排查使用 `*_v` 视图（第 2.9 节）；开放取值（浏览器种类、色板键）存文本。
- **标识与幂等**：实体表主键为 `INTEGER PRIMARY KEY`。`categories` 与 `annotations` 由客户端提供规范 `uuid`（`UNIQUE`）实现幂等重试；`rule_sets` 以全局唯一 `key` 标识并按 `base_version` 乐观并发写入（见 04 第 6.3 节）。
- **JSON 与共用条件引擎**：规则条件等结构化配置存 JSON 文本；条件的解析、规范化与匹配由 `quartz-domain` 的独立条件引擎承担，供分类、隐私及未来 M7 判例/国策复用。

## 2. 表结构

所有表与外键见 `crates/quartz/src/store/schema.rs`，原始 DDL、CHECK 与部分索引见 `crates/quartz/migrations/`。下表仅说明各组表的职责与非显而易见的列语义：

### 2.1 运行与时钟（`capture_runs`、`clock_segments`）

- 采集核心使用运行内逻辑编号，持久化外壳维护每运行的逻辑编号到数据库主键的映射，事务提交成功后才发布映射更新，并在引用全部落盘后清理已关闭的时钟段映射。
- `capture_runs.confirmed_utc_ms` 与 `clock_segments.confirmed_*` 在运行/时钟段开放期间周期推进，崩溃恢复时作为截断边界。

### 2.2 身份（`applications`、`window_instances`、`titles`、`domains`）

- `applications.identity_key` 唯一标识应用，形态为 `exe:<路径>`、`aumid:<AUMID>` 或 `unresolved:<类名>`；`exe_name` 存小写文件名便于规则匹配。
- `window_instances` 以 `(run_id, hwnd, generation)` 唯一区分同一运行内 HWND 复用，记录 `window_class` 供提取与分类查询使用。
- `titles.text` 最多存 4,096 个 UTF-16 单元，保留 `UNIQUE` 文本索引（用户决策 9），截断状态记录在引用行的 `title_status`。
- `domains.host` 存小写、无端口的 Punycode ASCII 域名。

### 2.3 前台事实（`intervals`）

- **隐私只在采集时生效**：写入前已完成隐私求值，已丢弃的内容永不落盘、不可因放宽规则恢复。`capture_level` 取 `full`、`title_hidden`、`excluded`（见 06 第 2 节）。
- **降级解释不可变**：降级行保存带修订号的 `privacy_rule_id`（`<规则集>/<规则>@<版本>` 或识别器 ID 如 `builtin.capture_excluded@1`）及标签快照（内置存 `privacy_label_key` 按当前语言翻译，用户存 `privacy_label` 文本快照）；`full` 时相关列为 `NULL`。
- **排除与标题状态**：`excluded` 行不写 `app_id`、`window_instance_id`、`title_id`，仅保留时间边界与标签，不计活跃时长。`title_status` 区分 `ok`、`empty`、`unreadable`、`truncated`、`redacted`（隐私丢弃）与 `purged`（保留期清理）。
- `sequence` 由单写者按运行内顺序递增分配（不建唯一索引以省体积），`session_key` 关联同一连续会话的午夜拆分切片。

### 2.4 可用状态（`availability_intervals`）

- 记录 `idle`、`locked`、`disconnected`、`suspended`、`paused` 五类状态；`last_input_utc_ms`、`detected_utc_ms`、`threshold_ms`、`exception`（`keep_alive` / `app_exempt` / `audible`）仅对 `idle` 有意义。
- 开放可用状态的周期确认只推进上次确认点至本次确认点所覆盖 UTC 日的版本，结构关闭或午夜拆分才更新完整范围。

### 2.5 浏览器（`browser_streams`、`web_intervals`、`ingest_receipts`，M5）

- 一条扩展消息可被前台区间切分为多段 `web_intervals`，去重键为 `ingest_receipts(stream_id, epoch, message_seq)`。
- `web_level = none` 不写入 `web_intervals`；当前已关联网页以开放行 + `confirmed_utc_ms` 持久化。`browser.ack` 仅在消息结果（含丢弃原因）持久写入 `ingest_receipts` 后发出。

### 2.6 规则与配置（`categories`、`rule_sets`、`rule_set_versions`、`annotations`、`settings`、`timed_policies`）

- **`categories`**：内置分类有稳定 `builtin_key`，未改名时 `name` 为 `NULL`（按 `builtin_key` 翻译，见 10 第 3 节）；`color` 存色板键 `cat.1`–`cat.14` 或 `neutral`（不存 RGB）；只提供非破坏性 `archived`，不提供物理删除。
- **`rule_sets` 与 `rule_set_versions`**：按 `parent_id` 组成同 `kind` 无环树，子集只存相对父级的 `rule` / `override` / `disable` 差异；语法、字段、操作符与生效顺序以 [规则格式文档](../rules/rule-format.md) 为准。
  - `key` 创建后 `kind` 与 `parent_id` 永久固定（包括软删除 `deleted_utc_ms` 后以同 `key` 恢复）；停用某集即停用其整棵子树；内置规则集（`source = builtin`）只读，用户编辑写入覆盖子集（默认 `user.local.{classification,extraction,privacy}`）。
  - `rule_set_versions.content` 为规范化 JSON（分类统一用 UUID 引用，改名不影响规则；新修订包含可选 `set` 头部元数据，读取拒绝未知顶层字段），`content_hash` 为整段存储 JSON 的 SHA-256。
  - 运行期全部规则编译为不可变内存快照常驻内存，并发与写入顺序见 01 第 6.2 节。
- **`annotations`**：`kind` 创建后固定（`category_override`、`offline_activity`、`active_override`、`dismissed_away`、`note`）；`label_preset`（`1 = rest`）仅允许用于 `note`，且 `note` 须有预设或非空白的 `label`/`note`；`dismissed_away` 禁止附带分类、标签或备注；`category_override` 必填未归档分类。同类型裁剪与确定性右段 UUIDv8 生成见第 5.1 节。
- **`settings` 与 `timed_policies`**：五种限时策略（`pause`、`privacy`、`keep_timing`、`app_exemption`、`audible`）的状态与截止 UTC 统一保存在 `timed_policies`，与采集边界同事务写入，不在 `settings` 重复存储；其余配置与持久化截止时间存 `settings`。

### 2.7 版本与失效（`data_day_versions`）

- `data_day_versions(source, utc_date)` 追踪 `intervals`（含时钟段边界变化）、`availability`、`web`、`annotations`、`gaps` 五类来源的逐 UTC 日版本号。
- `classification_version`、`extractor_version`、`privacy_version` 为对应类型**生效规则列表**的内容 SHA-256 哈希，规则改动自然改变哈希；`settings.metadata_version` 在应用显示名、浏览器 profile 标签或分类元数据变化时同事务递增；`stats_policy_version` 随统计口径变更递增。
- 规则修订号统一为 `u32`（已保存从 1 起，0 为创建哨兵）；日数据与元数据计数为 `u64`、持久上限 `i64::MAX`，溢出直接拒绝事务不回绕（wire 约定见 [04 第 3.1 节](04-ipc.md#31-版本与数值)）。
- 当前不建持久派生表。

### 2.8 删除屏障与采集缺口（`deletion_barriers`、`capture_gaps`）

- `deletion_barriers` 至少保留 14 天，用于拦截已删除/降级范围的迟到写入；`capture_gaps` 在时钟段边界拆分，不保存应用、标题或域名。

### 2.9 枚举编码与排查视图（`enum_labels`、`*_v`）

- Rust 枚举通过 `IntegerCode` 显式声明编码与标签，单元测试校验其与迁移写入的 `enum_labels` 完全一致；读到未知编码直接报错。
- 迁移提供的 `intervals_v`、`availability_intervals_v`、`web_intervals_v`、`capture_gaps_v`、`annotations_v` 仅作人工排查只读视图，核心代码不读取视图。

## 3. 索引与扫描边界

完整索引定义见 `crates/quartz/migrations/`。核心不变量：

- **单开放行约束**：`intervals`（按 `run_id`）、`availability_intervals`（按 `run_id, reason`）与 `web_intervals`（按 `stream_id`）均用 `WHERE end_utc_ms IS NULL` 部分唯一索引保证同一时刻至多一条开放记录。
- **有界扫描**：已结束的前台与可用状态均在 UTC 午夜拆分（单行 ≤24 h），范围查询可使用 `start_utc_ms >= :from - 86_400_000 AND start_utc_ms < :to AND end_utc_ms > :from` 命中时间索引；开放行单独读取并截断至 `confirmed_utc_ms`。标注、删除屏障和采集缺口使用完整相交条件 `start_utc_ms < :to AND end_utc_ms > :from`。

## 4. 写入与恢复

### 4.1 连接设置

数据库线程持有两个 diesel `SqliteConnection`：写连接，以及 `file:…?mode=ro` + `query_only = ON` 的只读连接。

- 关键 PRAGMA：`journal_mode = WAL`、`synchronous = NORMAL`（崩溃不丢已提交事务，接受断电丢失最近事务的取舍）、`foreign_keys = ON`、`secure_delete = ON`、`cache_size = -2048`、`temp_store = MEMORY`、`busy_timeout = 2000`。
- 只读连接在读事务内持有 WAL 快照分批计算，批间让出给写连接；查询响应单帧返回并受上限与 `truncated` 约束（见 [04 第 6.2 节](04-ipc.md#62-查询)）。

### 4.2 批量写入

- 前台切换的“关闭旧区间 + 开启新区间”在同一事务内完成；数据库线程每次最多批取 64 条命令或等待最多 1 秒合批提交，同事务推进受影响 UTC 日的 `data_day_versions`。
- 确认点每 59 秒调度（预留 1 秒批量窗口），提交优先级高于普通查询，统一推进全部开放记录的确认点；逾期无法确认的范围转入 `capture_gaps`（01 第 6 节）。

### 4.3 启动恢复

1. 在单个事务内将旧运行全部未关闭的前台、可用状态、网页、时钟段与运行记录以各自 `confirmed_*` 结束，置 `end_reason = crash_recovered` 并清空确认点列，前台按单调时间补算 `duration_ms`。
2. 开启新运行与新时钟段，复核会话与当前前台；旧网页聚焦与 audible 不恢复。

### 4.4 维护

- 进入 `idle` 或 `locked` 5 分钟后执行 `PRAGMA optimize`，每天至多一次。
- 依赖 SQLite WAL 自动 checkpoint，不在每次写入后 `TRUNCATE`；不自动 `VACUUM`，由设置页按需触发。

### 4.5 访问层（diesel）

- 使用 `diesel`（SQLite + `returning_clauses_for_sqlite_3_35`）与 `diesel_migrations`，静态链接 `libsqlite3-sys`（`bundled`），启动校验 `sqlite_version() >= 3.51.3`。
- `store/schema.rs` 由 `diesel print-schema` + `crates/quartz/schema.patch` 生成提交，行模型标注 `#[diesel(check_for_backend(Sqlite))]`。
- 写事务统一使用 `immediate_transaction` 避免升级写锁冲突；字典表用 `on_conflict().do_update()` + `RETURNING id`；原始 SQL 集中在 `store::raw` 并由测试校验。
- diesel 仅限 `quartz::store` 模块，不泄漏到 `quartz-domain` / `quartz-protocol`。`store` 只负责持久化与类型转换（以及作为写入约束的 `store::annotations` 校验与裁剪），不持有业务规则判断；备份通过 `VACUUM INTO` 生成一致紧凑副本。

### 4.6 容量估算

依据 2026-11-16 模拟测量与用户决策 9（采用去掉 `UNIQUE(run_id, sequence)`、整数枚举、关闭行确认点置 `NULL` 三项优化，保留 `titles.text` 唯一索引便于直读），一年数据量估算为：**轻度 44 MiB / 典型 150 MiB / 重度 442 MiB**（工作日实际约 70%）。`quartz gen-data` 可复现三档参数用于基准测试，实测记录见 [测量记录](../measurements.md)。

## 5. 查询管线与缓存

### 5.1 管线

所有聚合、时间轴、详情与规则预览共用同一查询管线（实现见 `crates/quartz/src/query/`）：

```text
1. 一致 WAL 读快照读取事实、可用状态、已关联网页、缺口、标注及所属时钟段（开放行止于确认点）
2. 在各时钟段内按查询边界及全部事实/状态/标注端点切片，保留 idle 掩码
3. 文档提取 → 分类求值（附命中来源）
4. 应用人工标注优先级：category_override > active_override > 规则；offline_activity 仅填补非活跃/非排除空隙
5. 计算每片 Activity 与有效时长
6. 聚合或投影为时间轴 / 排行 / 详情 / 预览
```

**标注写入裁剪与求值优先级：**

- `category_override`、`offline_activity`、`active_override`、`dismissed_away` 采用同类型后写覆盖先写：全覆盖删旧行，部分覆盖裁剪，严格包含拆为左右两段（左段保留旧 UUID，右段按前缀 `quartz.annotations.right.v1` + 旧/新 UUID 与端点经 SHA-256 确定性生成 RFC 9562 UUIDv8，实现见 `store::annotations::clipping`）。`note` 允许任意重叠，不同类型间保留重叠；删除不恢复既往裁剪。
- **活动状态**：`Activity` 取 `Active`、`Idle`、`ActiveOverride(id)`、`OfflineActivity(id)`、`NoForeground`、`Excluded`、`Stopped(reasons)`、`Gap(reason)`。
- **活跃时长公式**：活跃时长 = 允许计时窗口的非空闲部分 + `active_override` 恢复的 `idle` 窗口 + `offline_activity` 填补的非活跃/非排除范围（时钟段内按单调时长并集累计，段外离线活动或跨段休眠停止按 UTC 端点差计）。`dismissed_away` 只隐藏待确认离开提示，不改变时长。
- **分类覆盖优先级**：`category_override` 仅作用于 `Active` / `ActiveOverride` 且优先于 `active_override` 的可选分类；未指定分类的 `active_override` 沿用规则分类；`offline_activity` 未指定分类则为未分类。标注引用已归档或不存在的分类时按未分类处理并记录诊断。

### 5.2 文档身份提取

提取按以下顺序取第一条匹配：

1. 提取规则集树的生效规则（按 [规则格式文档](../rules/rule-format.md) 1.2 节优先级与树深度排序；`builtin.extraction` 首版覆盖 VS Code、JetBrains、Office、浏览器 profile 后缀、Windows Terminal）；
2. 通用回退：仅剥离 `" - <应用显示名>"` 后缀作为 `document`，不做激进数字/括号清洗；应用特定前缀（如 VS Code `● `）只在对应规则中剥离并置 `dirty = true`。

**文档身份不变量**：身份元组为 `(app_id, document, project, profile)`，文本分量按 NFC 规范化并保留大小写，严格区分 `NULL` 与空串；`document` 缺失/为空、`status` 为 `empty`/`unknown` 或隐私隐藏应用/标题时不生成身份；`dirty`、`normalized_title`、`extractor_rule` 不参与身份。对外仅暴露绑定查询输入版本与 `reference_range` 的不透明 `document_ref`（见 [04 第 6.2 节](04-ipc.md#62-查询)）。

### 5.3 分类

- 生效分类规则按 `priority` 降序 → 树深度降序 → 前序/文件顺序取第一条命中；无命中为未分类。
- 隐私可见性：`TitleHidden` 只暴露应用与已保留窗口类名，标题与 `doc.*` 缺失；`Excluded` 不参与分类。第一条命中规则若指向已归档或缺失分类，结果回退为未分类并保留诊断。
- 默认内置分类（稳定 `builtin_key`，未分类不建表行）：

| `builtin_key` | 倾向 | 色板键 | 排序 |
|---|---|---|---|
| `development` | `focus` | `cat.8` | 0 |
| `learning` | `focus` | `cat.1` | 1 |
| `communication` | `neutral` | `cat.3` | 2 |
| `entertainment` | `distraction` | `cat.13` | 3 |
| `system` | `neutral` | `cat.10` | 4 |

- 专注/分心占比以活跃时长为分母（零活跃时为 0）；最长连续专注段为同一时钟段内首尾相接且倾向为 `focus` 的活跃片段单调时长之和（可跨应用与专注分类，段外连续专注离线活动按 UTC 累计，出入时钟段会中断）。

### 5.4 缓存与失效

当前仅实现**单次查询内内存缓存**与**基于版本的失效通知**（实现见 `crates/quartz/src/query/classification.rs`、`versions.rs`）：

- **查询内缓存**：单次查询的 `Classifier` 持有固定的规则快照与数据库读快照（从而固定规则哈希与 `metadata_version`），在内存中维护：
  - 提取缓存 `HashMap<(app_id, title_id, capture_level), Arc<ExtractedTitle>>`；
  - 分类匹配缓存 `HashMap<MatchKey, ClassificationId>`，其中 `MatchKey` 仅包含当前生效规则实际读取的字段（如纯应用规则不带 `title_id` / `window_class`，按应用预求值一次 `PreparedClassifier` 后可跨标题复用）；`Excluded` 不入缓存。
  - 查询结束立即释放，不跨查询常驻内存。
- **版本失效**：`data_day_versions`、`settings.metadata_version`、规则生效 SHA-256 哈希、时区、日起点偏移、`stats_policy_version` 与 `QUERY_SEMANTICS_VERSION` 组成查询版本摘要（按周范围查询额外绑定生效首工作日），驱动 `day:<日期>` 订阅、托盘摘要按需刷新与 `document_ref` 过期检查；系统时区变化复位时区缓存后主动触发版本重算，托盘摘要同步跟随最新时区与日起点偏移。
- 跨查询 LRU 或持久派生缓存暂不启用，待真实基准表明需要时再独立引入。

### 5.5 正则安全

用户正则在编译期强制设置 `size_limit = 1 MiB` 与 `dfa_size_limit = 1 MiB`，匹配输入统一截断至 4,096 个 UTF-16 单元；规则预览按批定期检查取消标志。

## 6. 保留与删除

| 数据 | 默认保留期 | 到期处理 |
|---|---|---|
| 前台区间、可用状态、采集缺口、域名、标注 | 永久 | —（标注不受自动保留期影响） |
| 窗口标题与网页标签标题 | 永久（可配置如 90 天） | 置空 `title_id`/`tab_title_id`、写 `purged`，删除无引用 `titles` |
| `ingest_receipts`、`deletion_barriers` | 14 天 | 按时间清理已到期且不再被任务占用的记录 |
| 迁移前备份 | 最近 2 份 | 自动删除更旧迁移备份；手动备份由用户管理 |

**按日期范围删除与数据维护（`data.*`）：**

- **范围约束**：`data.delete_range` 仅接受已结束的完整逻辑日半开区间（`start_date < end_date <= 当前逻辑日`，跨度 ≤ 366 天），按与查询一致的 `day.start_offset_min` 与当前系统时区换算为 UTC 范围。
- **单事务裁剪与字典清理**：在一个即时写事务内处理 `intervals`、`availability_intervals`、`capture_gaps`、`web_intervals`、`annotations`（含 `note`）五张事实表——完全落入范围的行直接删除，跨越任一边界的行裁剪保留范围外部分，横跨整个范围的行拆分为前后两段（保持各表时钟段、开放行 ID、UTC 午夜切分、外键与唯一约束不变量）；随后删除不再被引用的 `titles` 与 `domains`，并仅对受影响的 UTC 日与数据源推进 `data_day_versions`。
- **物理覆写与 WAL 截断**：全局连接已启用 `PRAGMA secure_delete = ON`；删除提交后立即执行 `PRAGMA wal_checkpoint(TRUNCATE)` 并刷新已订阅日期版本与托盘摘要。
- **备份隔离**：手动备份（`backups\manual-<UTC yyyymmddThhmmssZ>.db`，不覆盖已有文件且永不自动清理）与迁移前备份均不随删除改写，可能仍包含已删除数据，由界面在删除前明确提示用户。
- **不写 `deletion_barriers`**：由于仅允许删除已结束的完整逻辑日，正常采集不会再向该范围写入；仅系统时钟大幅回拨时才可能写入已删范围，该极端情况予以接受。
- **执行时长**：`data.compact`（`VACUUM` + `wal_checkpoint(TRUNCATE)`）与大范围删除在数据库写线程串行执行前先等待在途只读快照结束，大库上可能阻塞其他写入数秒；采集写入队列会继续缓冲并在忙时重试，不丢数据。

历史隐私清理仍留待以后（见 [06 第 4.2 节](06-privacy.md#42-历史隐私清理)）。

## 7. 版本升级与兼容

| 对象 | 版本来源 | 演进策略 |
|---|---|---|
| 数据库 schema | diesel 迁移 | 只向前，迁移前备份，拒绝降级（第 8 节） |
| 内置规则集 | 修订号 `builtin_revision` | 按修订升级，不改动用户子集（第 8.5 节） |
| 隐私解释 ID | `<规则集>/<规则>@<版本>` | 历史事实自带 ID 与标签快照，升级后仍可解释 |
| JSON 配置列 | serde 默认值 + 忽略未知字段 | 增量字段免迁移，破坏性变更走 Rust 数据迁移（第 8.6 节） |
| IPC / 扩展协议 | `hello` / `browser.hello` 版本范围 | 与 schema 独立演进（04 第 4 节、05） |

## 8. schema 迁移

### 8.1 迁移文件

- 迁移位于 `crates/quartz/migrations/<时间戳>_<名称>/up.sql`，由 `crates/quartz/src/store/migrations.rs` 嵌入 exe；不提供 `down.sql`（不支持降级）。
- 已发布迁移不可修改，`crates/quartz/migrations.lock` 记录 SHA-256 供 CI 校验；`store/schema.rs` 通过 `deno task check-schema` 校验与空库迁移后 `diesel print-schema` 逐字节一致。

### 8.2 启动流程

取得数据目录锁后、启动采集前执行：

1. 只读检查已应用迁移版本：若含 exe 不认识的迁移（数据库更新于程序），拒绝写入与启动，以 `newer_database` 启动 UI 错误窗口并退出。
2. 无待执行迁移则正常启动。
3. 有待执行迁移时，先用 `VACUUM INTO` 写出 `backups\pre-migrate-<旧版本>-<时间>.db`（备份失败即中止迁移），再顺序执行迁移；全部成功后仅保留最近 2 份迁移备份。
4. 迁移失败时停止启动，以 `migration_failed` 及备份路径启动 UI 错误窗口；用户可在核心停止后运行 `quartz restore-backup <文件> --yes`（不带 `--yes` 仅提示并失败退出，执行时持数据目录锁校验排他锁、清 WAL/SHM 并原子替换主库）。

### 8.3 编写约束

- 迁移默认在单事务内运行；需重建表时优先 `PRAGMA defer_foreign_keys = ON` + 末尾 `PRAGMA foreign_key_check`，确需关闭外键检查的迁移通过 `metadata.toml` 设 `run_in_transaction = false` 并单独评审。
- 迁移只依赖 schema 与库内数据，不依赖运行期规则或设置；需要 Rust 逻辑的数据迁移实现 diesel `Migration` trait 并入同一 `MigrationSource` 序列；预计超过 10 秒的迁移须分批并报进度。

### 8.4 测试

- 为每个发布版本保留小型样本库验证升级路径，校验迁移后 `sqlite_schema` 与空库直迁一致，并覆盖“库新于程序”、迁移中途失败与备份恢复。

### 8.5 内置规则集升级

启动时先补齐缺失的 5 个内置分类（固定 UUID、`name = NULL`，不覆盖已有用户修改，有插入则推进 `metadata_version`），再按 `builtin_revision` 安装内置规则集：

1. 仅当内嵌修订高于库中修订时写入新版本（保留旧版本）；新增内置集直接创建。
2. 重新解析规则集树：用户 `override` / `disable` 目标在新版本中不存在时标为“目标已删除”且暂不生效；绝不自动修改用户规则集。生效规则哈希变化后，相关版本摘要自然刷新。

### 8.6 JSON 配置列

`settings.value`、`rule_set_versions.content`、`deletion_barriers.scope` 等 JSON 列通过 serde 默认值与忽略未知字段向前兼容；字段改名、语义变化或删除时编写 Rust 数据迁移改写旧数据，不在读取路径堆叠多版本兼容逻辑。

## 9. 导入导出（后续）

- 完整导出使用带格式版本的 JSON Lines（含事实、规则、标注），CSV 仅用于报表导出。
- 外部历史导入需记录来源与原始主键保证幂等，并在本地时间存在时区或夏令时歧义时显式确认。
