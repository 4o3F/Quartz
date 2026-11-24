# 03 数据模型

原则：**事实只记一次，不改写；派生结果都可以从事实和规则重建。**

数据库是一个 SQLite 文件 `%LOCALAPPDATA%\Quartz\quartz.db`，只有核心的数据库线程写入。

## 1. 约定

- 时间：UTC 毫秒整数，列名以 `_utc_ms` 结尾。区间统一为左闭右开 `[start, end)`。采集事实允许零时长，但零时长区间不计入统计；标注要求 `end_utc_ms > start_utc_ms`。
- 时长：前台事实的 `duration_ms = end_mono_ms − start_mono_ms`；开放行用确认单调时间代替结束时间。统计按各时钟段中的实际长度累计，UTC 裁剪仅在同一时钟段内做。跨时钟段的重复 UTC 不相互抵消，时间轴标明时钟异常。`suspended` 的墙上时间边界见 02 第 5 节。
- 取值固定的枚举列存小整数，编码从 1 开始，列注释写明编码。编码只追加，不改号、不复用；废弃的取值保留编码。名称记录在 `enum_labels` 表，排查时使用 `_v` 视图（第 2.9 节）。取值开放的列（浏览器种类、色板键）仍存文本。
- 确认点列（`confirmed_utc_ms`、`confirmed_mono_ms`）只在开放行有值；行结束时置 NULL，因为此时它与结束时间相同。各表用 CHECK 约束保证“结束列为 NULL”与“确认点列非 NULL”同时成立。
- 实体表用 `INTEGER PRIMARY KEY`。回执、版本与键值配置使用下文列出的复合键或文本键。分类和标注的创建带客户端生成的 `uuid TEXT NOT NULL UNIQUE`，重试返回已有实体；规则集以全局唯一 `key` 标识，创建与版本冲突见 04 第 6.3.1 节。
- 规则条件这类结构化配置存为 JSON 文本，由核心解析。条件的解析、规范化与匹配是 `quartz-domain` 中的独立组件“条件引擎”，分类规则、隐私规则与以后的判例、国策评估（12）共用。
- 逻辑日：“今天”与日统计按本地日期加设置 `day.start_offset_min`（默认 0，范围 0–720 分钟）切分。例如偏移 240 时，凌晨 3 点仍属于前一个逻辑日。偏移属于查询语义，进入日缓存键；存储层仍按 UTC 午夜拆分，与偏移无关。

## 2. 表结构

### 2.1 运行与时钟

```sql
CREATE TABLE capture_runs (
  id               INTEGER PRIMARY KEY,
  device_id        TEXT NOT NULL,          -- 安装时生成的 UUID
  win_session_id   INTEGER NOT NULL,
  app_version      TEXT NOT NULL,
  started_utc_ms   INTEGER NOT NULL,
  confirmed_utc_ms INTEGER,             -- 运行中有值；结束后为 NULL
  ended_utc_ms     INTEGER,             -- 运行中或崩溃后为 NULL，下次启动补为最后确认点
  end_reason       INTEGER,                 -- 1 shutdown | 2 user_exit | 3 crash_recovered | 4 update
  CHECK ((ended_utc_ms IS NULL) = (confirmed_utc_ms IS NOT NULL))
);

CREATE TABLE clock_segments (
  id               INTEGER PRIMARY KEY,
  run_id           INTEGER NOT NULL REFERENCES capture_runs(id),
  start_utc_ms     INTEGER NOT NULL,
  start_mono_ms    INTEGER NOT NULL,    -- QueryUnbiasedInterruptTime / 10_000
  confirmed_utc_ms INTEGER,             -- 开放时有值，结束后为 NULL
  confirmed_mono_ms INTEGER,
  end_utc_ms       INTEGER,
  end_mono_ms      INTEGER,
  end_reason       INTEGER,                 -- 1 time_change | 2 resume | 3 drift | 4 run_end | 5 crash_recovered
  CHECK ((end_utc_ms IS NULL) = (confirmed_utc_ms IS NOT NULL)),
  CHECK ((end_mono_ms IS NULL) = (confirmed_mono_ms IS NOT NULL))
);
```

采集核心使用运行内逻辑编号。`run_id` 由外壳按数据库已有最大运行编号加一分配；interval 的 `sequence` 保存核心的逻辑行号，`session_key` 保存午夜片段的共同逻辑编号。时钟段、窗口实例和可用状态的逻辑编号由持久化外壳持有的每运行映射表解析为数据库主键。每次写事务使用映射表副本，提交成功后才发布副本。启动恢复根据持久行重建旧运行的映射，并为恢复新建的午夜片段分配大于旧 interval sequence 的逻辑行号。

### 2.2 身份

```sql
CREATE TABLE applications (
  id               INTEGER PRIMARY KEY,
  identity_key     TEXT NOT NULL UNIQUE,   -- exe:<路径> | aumid:<AUMID> | unresolved:<类名>
  kind             INTEGER NOT NULL,       -- 1 exe | 2 aumid | 3 unresolved
  exe_path         TEXT,
  aumid            TEXT,
  exe_name         TEXT,                   -- 规则匹配常用，小写
  display_name     TEXT NOT NULL,
  first_seen_utc_ms INTEGER NOT NULL
);

CREATE TABLE window_instances (
  id               INTEGER PRIMARY KEY,
  run_id           INTEGER NOT NULL REFERENCES capture_runs(id),
  app_id           INTEGER NOT NULL REFERENCES applications(id),
  pid              INTEGER NOT NULL,
  process_created_utc_ms INTEGER,
  hwnd             INTEGER NOT NULL,
  generation       INTEGER NOT NULL,    -- 同一运行、HWND 内递增
  retired_utc_ms   INTEGER,
  window_class     TEXT,
  first_seen_utc_ms INTEGER NOT NULL,
  last_seen_utc_ms  INTEGER NOT NULL,
  UNIQUE (run_id, hwnd, generation)
);

CREATE TABLE titles (
  id               INTEGER PRIMARY KEY,
  text             TEXT NOT NULL UNIQUE    -- 最多 4096 UTF-16 单元；截断状态由引用行保存
);

CREATE TABLE domains (
  id               INTEGER PRIMARY KEY,
  host             TEXT NOT NULL UNIQUE    -- 小写，去掉端口；IDN 存 punycode
);
```

### 2.3 前台事实

```sql
CREATE TABLE intervals (
  id               INTEGER PRIMARY KEY,
  run_id           INTEGER NOT NULL REFERENCES capture_runs(id),
  sequence         INTEGER NOT NULL,    -- 运行内递增，单写者分配；用于排序，不建唯一约束
  clock_segment_id INTEGER NOT NULL REFERENCES clock_segments(id),
  kind             INTEGER NOT NULL,       -- 1 window | 2 no_foreground
  no_foreground_reason INTEGER,             -- 1 empty | 2 unknown_desktop；window 时 NULL
  app_id           INTEGER REFERENCES applications(id),
  window_instance_id INTEGER REFERENCES window_instances(id),
  title_id         INTEGER REFERENCES titles(id),
  title_status     INTEGER NOT NULL,       -- 1 ok | 2 empty | 3 unreadable | 4 truncated | 5 redacted | 6 purged
  capture_level    INTEGER NOT NULL,       -- 1 full | 2 title_hidden | 3 excluded（见 06）
  privacy_rule_id  TEXT,                   -- 带修订号；full 时为 NULL
  privacy_label_key TEXT,                  -- 内置规则的标签键（10 第 3 节）；full 时为 NULL
  privacy_label    TEXT,                   -- 用户规则的标签文本快照；full 时为 NULL
  session_key      INTEGER NOT NULL,    -- 午夜拆分出的区间共享同一值
  start_utc_ms     INTEGER NOT NULL,
  start_mono_ms    INTEGER NOT NULL,
  end_utc_ms       INTEGER,             -- 进行中为 NULL
  end_mono_ms      INTEGER,
  confirmed_utc_ms INTEGER,             -- 开放时有值，结束后为 NULL
  confirmed_mono_ms INTEGER,
  duration_ms      INTEGER,
  end_reason       INTEGER,                -- 1 switch | 2 title | 3 no_foreground | 4 locked | 5 suspended | 6 paused | 7 disconnected | 8 shutdown | 9 crash_recovered | 10 clock_jump | 11 midnight_split | 12 policy_changed | 13 capture_gap（含义见 02 第 4.2 节）
  CHECK (end_utc_ms IS NULL OR end_utc_ms >= start_utc_ms),
  CHECK (end_mono_ms IS NULL OR end_mono_ms >= start_mono_ms),
  CHECK ((end_utc_ms IS NULL) = (confirmed_utc_ms IS NOT NULL)),
  CHECK ((end_mono_ms IS NULL) = (confirmed_mono_ms IS NOT NULL))
);
```

`privacy_label` 是降级时保存的标签快照。规则改名、删除或内置修订更新后，历史仍使用该标签。规则 ID 格式为 `<规则集 key>/<规则 id>@<规则集版本>`，例如 `builtin.privacy/browser-private-edge@3`、`user.bank/bank@2`；系统信号识别器（06 第 3.2 节）为 `builtin.capture_excluded@1` 这样的识别器 ID；同等严格程度下，用户规则优先于内置规则，同来源按 `sort_order` 小者优先。前台 capture_level 为 full 时，两列均为 NULL；网页行按自身 web_level 判断，full 时两列均为 NULL。

窗口标题被丢弃时，`title_id = NULL`、`title_status = redacted`；网页 domain_only 时，`tab_title_id = NULL`、`tab_title_status = redacted`。保留期清理使用 purged，读取失败使用 unreadable，空文本使用 empty。

`capture_level = excluded` 的区间不记录 `app_id`、`window_instance_id` 和标题，只留下时间段、规则 ID 和标签快照。时间轴显示“已排除 25 分钟”，不计活跃时长。

### 2.4 可用状态

```sql
CREATE TABLE availability_intervals (
  id               INTEGER PRIMARY KEY,
  run_id           INTEGER NOT NULL REFERENCES capture_runs(id),
  clock_segment_id INTEGER NOT NULL REFERENCES clock_segments(id),
  reason           INTEGER NOT NULL,       -- 1 idle | 2 locked | 3 disconnected | 4 suspended | 5 paused
  start_utc_ms     INTEGER NOT NULL,
  start_mono_ms    INTEGER NOT NULL,
  end_utc_ms       INTEGER,
  end_mono_ms      INTEGER,
  confirmed_utc_ms INTEGER,             -- 开放时有值，结束后为 NULL
  confirmed_mono_ms INTEGER,
  end_reason       INTEGER,                -- 1 resumed | 2 shutdown | 3 crash_recovered | 4 clock_jump | 5 midnight_split | 6 policy_changed
  -- 以下只对 idle 有意义
  last_input_utc_ms INTEGER,
  detected_utc_ms  INTEGER,
  threshold_ms     INTEGER,
  exception        INTEGER,                -- 例外导致提前结束时记录原因：1 keep_alive | 2 app_exempt | 3 audible
  policy_version   INTEGER NOT NULL,
  CHECK (end_utc_ms IS NULL OR end_utc_ms >= start_utc_ms),
  CHECK (end_mono_ms IS NULL OR end_mono_ms >= start_mono_ms),
  CHECK ((end_utc_ms IS NULL) = (confirmed_utc_ms IS NOT NULL)),
  CHECK ((end_mono_ms IS NULL) = (confirmed_mono_ms IS NOT NULL))
);
```

### 2.5 浏览器

```sql
CREATE TABLE browser_streams (
  id               INTEGER PRIMARY KEY,
  browser          TEXT NOT NULL,          -- chrome | edge | firefox | ...
  profile_instance_id TEXT NOT NULL UNIQUE,-- 扩展首次运行时生成并存在 storage.local
  extension_id     TEXT NOT NULL,
  label            TEXT,                   -- 用户可编辑的 profile 名
  first_seen_utc_ms INTEGER NOT NULL,
  last_seen_utc_ms  INTEGER NOT NULL
);

CREATE TABLE web_intervals (
  id               INTEGER PRIMARY KEY,
  stream_id        INTEGER NOT NULL REFERENCES browser_streams(id),
  interval_id      INTEGER NOT NULL REFERENCES intervals(id), -- 关联到的前台区间
  start_utc_ms     INTEGER NOT NULL,
  end_utc_ms       INTEGER,             -- 当前网页为开放行
  confirmed_utc_ms INTEGER,             -- 开放时有值，结束后为 NULL
  end_reason       INTEGER,                -- 1 browser_state | 2 foreground_end | 3 disconnected | 4 gap | 5 policy_changed | 6 crash_recovered | 7 shutdown
  domain_id        INTEGER REFERENCES domains(id),
  tab_title_id     INTEGER REFERENCES titles(id),
  tab_title_status INTEGER NOT NULL,       -- 与 intervals.title_status 相同
  tab_id           INTEGER,
  browser_window_id INTEGER,
  navigation_id    INTEGER,             -- 扩展为每次导航分配，区分同域名的不同访问
  audible          INTEGER NOT NULL DEFAULT 0,
  link_status      INTEGER NOT NULL,       -- 1 linked | 2 ambiguous
  web_level        INTEGER NOT NULL,       -- 1 full | 2 domain_only（见 06）；none 时不写入本表
  privacy_rule_id  TEXT,                   -- 带修订号；full 时为 NULL
  privacy_label_key TEXT,                  -- 内置规则的标签键
  privacy_label    TEXT,                   -- 用户规则的标签文本快照
  CHECK (end_utc_ms IS NULL OR end_utc_ms >= start_utc_ms),
  CHECK ((end_utc_ms IS NULL) = (confirmed_utc_ms IS NOT NULL))
);

CREATE TABLE ingest_receipts (
  stream_id        INTEGER NOT NULL REFERENCES browser_streams(id),
  epoch            INTEGER NOT NULL,    -- 扩展每次启动加一
  message_seq      INTEGER NOT NULL,
  received_utc_ms  INTEGER NOT NULL,
  outcome          INTEGER NOT NULL,       -- 1 stored | 2 discarded_no_overlap | 3 discarded_privacy | 4 discarded_gap | 5 discarded_barrier
  PRIMARY KEY (stream_id, epoch, message_seq)
) WITHOUT ROWID;
```

一条浏览器消息可能被前台区间切成多段 `web_intervals`，去重键是 `ingest_receipts` 的 `(stream_id, epoch, message_seq)`。当前已关联上下文以 `end_utc_ms = NULL` 和 `confirmed_utc_ms` 持久化，随前台确认点推进；查询只读取开放行的已确认部分。

切换时，同一事务关闭旧网页行、打开新网页行并写回执。`browser.ack` 表示消息影响已经持久可恢复。无交集、隐私过滤或缺口消息也要持久写明丢弃结果后才能确认。

### 2.6 规则与配置

```sql
CREATE TABLE categories (
  id               INTEGER PRIMARY KEY,
  uuid             TEXT NOT NULL UNIQUE,
  parent_id        INTEGER REFERENCES categories(id),
  builtin_key      TEXT UNIQUE,            -- 内置分类的稳定键；用户分类为 NULL
  name             TEXT,                   -- 用户输入的名称；内置分类未改名时为 NULL，按 builtin_key 翻译（10 第 3 节）
  color            TEXT NOT NULL,          -- 色板键 cat.1–cat.14 | neutral（11 第 3.3 节），不存 RGB
  tendency         INTEGER NOT NULL,       -- 1 focus | 2 neutral | 3 distraction
  sort_order       INTEGER NOT NULL,
  archived         INTEGER NOT NULL DEFAULT 0,
  CHECK (name IS NOT NULL OR builtin_key IS NOT NULL)
);

CREATE TABLE rule_sets (
  id               INTEGER PRIMARY KEY,
  key              TEXT NOT NULL UNIQUE,   -- builtin.classification | user.study-sites ...
  kind             INTEGER NOT NULL,       -- 1 classification | 2 extraction | 3 privacy | 4 precedent | 5 policy_check
  parent_id        INTEGER REFERENCES rule_sets(id), -- 同 kind；不成环
  source           INTEGER NOT NULL,       -- 1 builtin | 2 user
  builtin_revision INTEGER,                -- 仅内置规则集保存发布修订号
  name             TEXT NOT NULL,
  enabled          INTEGER NOT NULL DEFAULT 1, -- 0 停用 | 1 启用；其他值在加载时拒绝；停用时整个子树不生效
  deleted_utc_ms   INTEGER,                -- NULL = 活跃；删除保留所有版本
  sibling_order    INTEGER NOT NULL,       -- 同一父级下的顺序
  current_version  INTEGER NOT NULL,
  chain_id         INTEGER,                -- kind = precedent 时所属的链（12，M7）
  created_utc_ms   INTEGER NOT NULL,
  CHECK ((source = 1 AND builtin_revision IS NOT NULL) OR
         (source <> 1 AND builtin_revision IS NULL)),
  CHECK (builtin_revision IS NULL OR builtin_revision > 0)
);
CREATE INDEX rule_sets_active_parent_order
  ON rule_sets(parent_id, sibling_order) WHERE deleted_utc_ms IS NULL;

CREATE TABLE rule_set_versions (
  set_id           INTEGER NOT NULL REFERENCES rule_sets(id),
  version          INTEGER NOT NULL,       -- 每次导入或编辑加一
  content          TEXT NOT NULL,          -- 规范化 JSON：rule / override / disable 条目；分类以 uuid 引用
  content_hash     TEXT NOT NULL,          -- content 的 SHA-256
  parent_version   INTEGER,                -- 保存时父规则集的版本，用于三方对比
  note             TEXT,
  created_utc_ms   INTEGER NOT NULL,
  PRIMARY KEY (set_id, version)
) WITHOUT ROWID;

CREATE TABLE annotations (
  id               INTEGER PRIMARY KEY,
  uuid             TEXT NOT NULL UNIQUE,
  kind             INTEGER NOT NULL,       -- 1 category_override | 2 offline_activity | 3 active_override | 4 dismissed_away | 5 note
  start_utc_ms     INTEGER NOT NULL,
  end_utc_ms       INTEGER NOT NULL,
  category_id      INTEGER REFERENCES categories(id),
  label            TEXT,
  note             TEXT,
  created_utc_ms   INTEGER NOT NULL,
  updated_utc_ms   INTEGER NOT NULL,
  CHECK (end_utc_ms > start_utc_ms)
);

CREATE TABLE settings (
  key              TEXT PRIMARY KEY,
  value            TEXT NOT NULL           -- JSON
) WITHOUT ROWID;
```

标注的类型在创建后固定。`category_override` 必须指定分类，`label` 与 `note` 可选；`offline_activity` 与 `active_override` 的分类、标签和备注均可选；`dismissed_away` 的分类、标签和备注必须全部为空；`note` 的标签或备注至少有一个包含非空白文本。任何指定的分类都必须存在且未归档，支持用 UUID 或内置键引用。更新按最终值覆盖这些可选字段，省略即清空。`created_utc_ms`、`updated_utc_ms` 由数据库循环注入的时钟提供；裁剪保留行的创建时间，拆分出的右侧新行使用本次写入时间作为创建和更新时间。

标注写入通过 `Store::write` 在一个立即事务中执行。每个新增、修改、裁剪、拆分或删除行的旧、新范围都通过 `UtcDays` 更新 `data_day_versions` 的 `annotations` 来源（编码 4）；版本变化在提交后发布，成功响应在发布后发送。创建 UUID 重试、最终值未变的更新和删除不存在的 UUID 不改变行或版本。

`settings` 只保存配置值和其他需要持久化的调度截止时间。五种限时策略的状态和截止时间统一保存在 `timed_policies`，不在 `settings` 重复保存。

```sql
CREATE TABLE timed_policies (
    kind INTEGER PRIMARY KEY CHECK (kind IN (1, 2, 3, 4, 5)),
    privacy_mode INTEGER CHECK (privacy_mode IN (1, 2, 3)),
    application_id INTEGER REFERENCES applications(id),
    started_utc_ms INTEGER NOT NULL,
    until_utc_ms INTEGER NOT NULL,
    CHECK ((kind = 2 AND privacy_mode IS NOT NULL) OR
           (kind <> 2 AND privacy_mode IS NULL)),
    CHECK ((kind IN (4, 5) AND application_id IS NOT NULL) OR
           (kind NOT IN (4, 5) AND application_id IS NULL)),
    CHECK (until_utc_ms >= started_utc_ms)
);
```

`kind` 依次为 `pause`、`privacy`、`keep_timing`、`app_exemption`、`audible`；`privacy_mode` 依次为 `normal`、`private`、`no_titles`，标签登记在 `enum_labels`。应用作用域引用 `applications`，授权时按应用身份插入或复用该行。

schema 版本由 diesel 的 `__diesel_schema_migrations` 表记录，不另建版本表，见第 8 节。

所有规则（分类、提取、隐私，以及 M7 的判例与国策检查）都以**规则集**组织，规则集通过 `parent_id` 组成树，子规则集只保存相对父级的差异（新增 `rule`、覆盖 `override`、停用 `disable`）。规则集树的语义、生效规则的计算顺序、对外文本格式 `quartz-rules/1`、全部字段、操作符与校验错误码，以 [规则格式文档](../rules/rule-format.md) 为准；`rule_set_versions.content` 是该文本的规范化 JSON 形式，分类改用 uuid 引用，因此分类改名不影响规则。

- 规则数量在数百条以内。核心启动或规则变化时，把每类规则的规则集树解析为生效规则列表并编译（正则、规范化后的条件值），组成不可变的规则快照常驻内存；查询与采集线程只读快照。规则的修改、快照发布与并发控制由规则注册表负责，见 01 第 6.2 节：注册表是规则表的唯一写者，修改先编译、后提交、再发布，并按 `base_version` 拒绝过期修改。
- 每条生效规则带来源：定义它的规则集 key 与版本、规则 id，以及覆盖它的规则集。这份来源用于解释（`rules.explain`）、命中预览与隐私解释 ID。
- 界面中“编辑内置规则”不修改内置规则集，而是写入用户选定的覆盖规则集（默认分别为 `user.local.classification`、`user.local.extraction`、`user.local.privacy`，以同 kind 的内置规则集为父）；编辑用户规则则生成该规则集的新版本。
- 规则集创建后 kind 与父级身份固定，删除后重新导入同一 key 也沿用原身份；需要换类型或父级时新建规则集。存储更新将 kind、父级、来源与当前修订共同作为写入前置条件，不更新这三个身份列；条件不符时回滚事务。历史基线只由 `rule_set_versions.parent_version` 沿固定父级链追溯。
- 历史版本全部保留（体积很小），用于版本 diff、回退与三方对比。回退内容使用来源历史版本的 `database:<key>@<version>` 合成位置，不沿用当前文本跨度；编译检查规则正文与头部 kind 一致。整树身份、祖先和目标校验只执行一次，再分别计算各类型的生效规则；每项树诊断只发布一次。分类规则的条件字段有：`app.identity_key`、`app.exe_name`、`app.display_name`、`window.class`、`title.raw`、`title.normalized`、`doc.document`、`doc.project`、`doc.profile`、`web.domain`、`web.tab_title`、`browser.profile_label`。判例与国策检查另可使用 `category`、`category.tendency`、`activity`。隐私规则另用 06 第 4 节的字段白名单，保存时拒绝 `doc.*` 与 `title.normalized`。

操作符有：`equals`、`contains`、`starts_with`、`ends_with`、`regex`、`domain_suffix`。文本比较的规范化、大小写与全半角处理、IDN 域名转换见 10 第 4.1 节；匹配结果与界面语言无关。域名子串操作保留片段的首尾及连续分隔点，完整域名操作才移除根点，具体转换见规则格式第 2.2 节。求值语义改变时递增现有 `QUERY_SEMANTICS_VERSION`；新文本的规范化内容变化同时反映在规则内容哈希中。

### 2.7 版本与缓存

```sql
CREATE TABLE data_day_versions (
  source           INTEGER NOT NULL,       -- 1 intervals | 2 availability | 3 web | 4 annotations | 5 gaps
  utc_date         INTEGER NOT NULL,    -- 自 1970-01-01 起的天数
  version          INTEGER NOT NULL,
  PRIMARY KEY (source, utc_date)
) WITHOUT ROWID;
```

`classification_version`、`extractor_version`、`privacy_version` 是对应类型**生效规则列表**的内容哈希（由各规则集当前版本的 `content_hash` 按解析顺序组合而成），不是手动递增的计数器：任何导入、编辑、启停、调整顺序都会自然改变哈希，回退到旧状态时哈希也恢复，可以复用缓存。`stats_policy_version` 仍保存在 `settings` 中，修改统计口径的事务里加一。

`intervals` 来源也覆盖时钟段范围的建立、结束和确认点推进，使尚无前台事实的采集范围变化能使查询与日订阅失效。

`settings.metadata_version` 在应用显示名、浏览器 profile 标签、分类创建、名称、颜色、倾向、层级、排序或归档状态变化时，在同一事务加一。日缓存、托盘摘要和 `day` 订阅同时失效。限时策略的 UTC 截止时间由核心的截止时间调度器管理，重启后检查是否到期（01 第 6.1 节）。

规则修订号（含 `base_version`、`parent_version`、内置修订号）统一为 u32，已保存版本从 1 开始，0 仅表示创建；日数据与元数据计数使用 u64，SQLite 持久值不超过 i64::MAX。递增溢出拒绝事务，不回绕。规则内容哈希为 SHA-256，查询 `versions` 是不透明摘要；wire 约定见 [04 第 3.1 节](04-ipc.md#31-版本与数值)。

首版不建立持久的派生表，派生结果放在内存缓存里（见第 5 节）。

### 2.8 删除屏障与采集缺口

限时策略状态保存在 `timed_policies`，授权、到期或取消与采集边界在同一事务写入或删除。

```sql
CREATE TABLE deletion_barriers (
  id               INTEGER PRIMARY KEY,
  start_utc_ms     INTEGER NOT NULL,
  end_utc_ms       INTEGER NOT NULL,
  scope            TEXT NOT NULL,          -- JSON：筛选条件与 delete / 字段降级动作
  created_utc_ms   INTEGER NOT NULL,
  CHECK (end_utc_ms >= start_utc_ms)
);

CREATE TABLE capture_gaps (
  id               INTEGER PRIMARY KEY,
  run_id           INTEGER NOT NULL REFERENCES capture_runs(id),
  clock_segment_id INTEGER NOT NULL REFERENCES clock_segments(id),
  start_utc_ms     INTEGER NOT NULL,
  end_utc_ms       INTEGER NOT NULL,
  start_mono_ms    INTEGER NOT NULL,
  end_mono_ms      INTEGER NOT NULL,
  reason           INTEGER NOT NULL,       -- 1 event_overload | 2 write_queue_overload | 3 database_error
  CHECK (end_utc_ms >= start_utc_ms),
  CHECK (end_mono_ms >= start_mono_ms)
);
```

删除屏障至少保留 14 天，与采集回执期限相同；仍在执行或失败待恢复的清理任务所需屏障继续保留，任务完成后才可按期限清理。入库前按屏障与当前隐私策略过滤迟到消息。缺口在时钟段边界拆分，不保存应用、标题或域名。

### 2.9 枚举编码与排查视图

```sql
CREATE TABLE enum_labels (
  enum_name        TEXT NOT NULL,          -- 如 'intervals.end_reason'；共用的枚举用类型名，如 'title_status'
  code             INTEGER NOT NULL,
  label            TEXT NOT NULL,          -- 与列注释、Rust 枚举变体名一致
  deprecated       INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (enum_name, code)
) WITHOUT ROWID;
```

- Rust 侧使用普通枚举定义，显式写出编码（例如 `Shutdown = 1`），并通过 `IntegerCode` 实现列名、全部变体、编码和标签映射；宏只为具体枚举生成 `ToSql`/`FromSql<Integer, Sqlite>` 实现，转发到泛型转换函数。测试检查迁移写入的 `enum_labels` 与全部 Rust 枚举的列名、编码和标签集合完全一致。
- 新增取值时，同一个迁移插入对应的 `enum_labels` 行。读到未知编码返回错误；数据库来自更新版本时核心已拒绝启动（第 8.2 节），正常运行不会遇到。
- 迁移为主要事实表建只读视图：`intervals_v`、`availability_intervals_v`、`web_intervals_v`、`capture_gaps_v`、`annotations_v`。视图把枚举编码连接成名称，把 UTC 毫秒转换成本地时间文本，并连接应用名、标题与域名。直接打开数据库排查时查询视图即可。
- 视图只供人工排查，核心代码不读视图。

## 3. 索引

```sql
CREATE INDEX intervals_time      ON intervals(start_utc_ms);
CREATE INDEX intervals_app_time  ON intervals(app_id, start_utc_ms);
CREATE INDEX intervals_window    ON intervals(window_instance_id);
CREATE UNIQUE INDEX intervals_one_open ON intervals(run_id) WHERE end_utc_ms IS NULL;

CREATE INDEX availability_time   ON availability_intervals(start_utc_ms);
CREATE UNIQUE INDEX availability_one_open
  ON availability_intervals(run_id, reason) WHERE end_utc_ms IS NULL;

CREATE INDEX web_intervals_time  ON web_intervals(start_utc_ms);
CREATE INDEX web_intervals_fg    ON web_intervals(interval_id);
CREATE INDEX web_intervals_domain_time ON web_intervals(domain_id, start_utc_ms);

CREATE UNIQUE INDEX web_one_open ON web_intervals(stream_id) WHERE end_utc_ms IS NULL;
CREATE INDEX receipts_received   ON ingest_receipts(received_utc_ms);

CREATE INDEX annotations_time    ON annotations(start_utc_ms, end_utc_ms);
CREATE INDEX deletion_barriers_time ON deletion_barriers(start_utc_ms, end_utc_ms);
CREATE INDEX deletion_barriers_created ON deletion_barriers(created_utc_ms);
CREATE INDEX capture_gaps_time   ON capture_gaps(start_utc_ms, end_utc_ms);
CREATE INDEX capture_gaps_clock  ON capture_gaps(run_id, clock_segment_id);
```

已结束的前台与可用状态都在 UTC 午夜拆分，单行最长 24 小时，查询可用 `start_utc_ms >= :from - 86_400_000 AND start_utc_ms < :to AND end_utc_ms > :from`。开放行另外读取，用 `confirmed_utc_ms` 截止；跨多日的开放可用状态按完整相交条件读取并虚拟拆分。

标注、删除屏障和采集缺口使用完整相交条件：`start_utc_ms < :to AND end_utc_ms > :from`。网页行受关联前台边界限制，开放部分只到确认点。

## 4. 写入与恢复

### 4.1 连接设置

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA secure_delete = ON;
PRAGMA cache_size = -2048;   -- 约 2 MiB
PRAGMA temp_store = MEMORY;
PRAGMA busy_timeout = 2000;
```

`synchronous = NORMAL` 的取舍：系统断电时可能丢失最近已提交的事务，但应用崩溃不会丢。对个人记录来说，这个代价可以接受。设置里不提供 `FULL` 选项，有需要再加。

数据库线程持有两个 diesel `SqliteConnection`：写连接，以及用 `file:…?mode=ro` URI 打开并设置 `query_only = ON` 的只读连接。只读连接一次查询在读事务中取得 WAL 快照后分批计算，批间让出给写连接。整个响应及分页游标绑定输入版本；版本变化返回 `stale_cursor`，客户端重新查询。

### 4.2 批量写入

- 前台切换产生的"结束旧区间 + 开始新区间"在同一事务里完成。
- 数据库线程从通道一次最多取 64 条命令，或者等待最多 1 秒，然后在一个事务里写完。
- 同一个事务里同时更新所涉及 UTC 日期的 `data_day_versions`；capture_gaps 写入推进 gaps 来源的版本。前台边界同步关闭关联网页行，新的已确认上下文才开新行。
- 确认点每 59 秒调度，预留最多 1 秒批量等待；提交期限优先于普通查询，事务更新前台、可用状态、网页、运行与时钟段全部开放记录。无法按期确认的范围进入缺口处理（01 第 6 节）。

### 4.3 启动恢复

1. 同一事务找到旧运行所有开放的前台、可用状态和网页记录，以各自 `confirmed_utc_ms` 结束，`end_reason = crash_recovered`；有单调边界的记录同时写入 `end_mono_ms = confirmed_mono_ms`，前台据此计算 `duration_ms`。结束后确认点列置 NULL。
2. 用旧运行的 `confirmed_utc_ms` 补 `ended_utc_ms` 并清空确认点，运行结束原因为 `crash_recovered`。旧时钟段用自身 UTC 与单调确认点关闭，结束原因同为 `crash_recovered`。
3. 开始新运行和新时钟段，复核会话与前台；旧网页聚焦与 audible 不恢复，等待新的完整状态。

### 4.4 维护

- 空闲时（进入 `idle` 或 `locked` 后 5 分钟）执行 `PRAGMA optimize`，每天最多一次。
- 保持 WAL 自动 checkpoint，不在每次写入后执行 `TRUNCATE`。
- 不自动执行 `VACUUM`。设置里提供"压缩数据库"按钮。

### 4.5 访问层（diesel）

数据库访问使用 `diesel 2.4.0`（`sqlite`、`returning_clauses_for_sqlite_3_35` feature）与 `diesel_migrations 2.4.0`。SQLite 由 `libsqlite3-sys` 的 `bundled` feature 静态链接，版本固定在 diesel 允许的范围内，启动时检查 `sqlite_version()` 不低于 3.51.3。

- **schema 与模型。** `store/schema.rs` 由 `diesel print-schema` 从迁移生成并提交。SQLite 的时间戳、时长与实体标识映射为 Rust `i64`；`diesel.toml` 使用 `schema.patch` 修正 SQLite `INTEGER` 的默认 32 位推断，保持 SQL 列定义不变。行类型使用 `Queryable`、`Selectable`、`Insertable`、`AsChangeset`，并加 `#[diesel(check_for_backend(Sqlite))]`，让列类型不匹配在编译期报错。
- **枚举列。** 整数编码与名称映射见第 2.9 节。
- **写入。** 批量写入使用 `immediate_transaction`，避免读后升级写锁时出现 `SQLITE_BUSY`。字典表（titles、domains、applications）用 `on_conflict(...).do_update()` 加 `RETURNING id` 一次取得 ID。批量插入按 SQLite 绑定参数上限分块。
- **只读快照。** 跨批次持有读事务时，用 diesel `TransactionManager` 的 `begin_transaction` / `commit_transaction` 包装成一个守卫类型，不使用闭包式 `transaction`。
- **原始 SQL。** 只用于 PRAGMA、`wal_checkpoint`、`VACUUM INTO`、`PRAGMA optimize` 和 diesel DSL 难以表达的少量查询（`sql_query` + `QueryableByName`，值一律绑定）。原始 SQL 集中在 `store::raw`，测试在迁移后的空库上逐条准备，确认表名与列名有效。
- **范围。** diesel 只出现在 `quartz` crate 的 `store` 模块。`quartz-domain` 与 `quartz-protocol` 不依赖 diesel；store 负责行类型与领域类型的转换。
- **只做持久化。** 标注的字段校验与同类裁剪作为写入约束放在独立的 `store::annotations` 中，其中范围运算不访问数据库。其他业务判断留在领域层。业务核心（规则注册表、采集状态机等，01 第 5 节）以领域层的写入描述（如 `RuleSetWrite`）表达要写什么；store 提供执行这些描述的方法，在调用方给定的事务中把它们转换为 SQL。业务核心不定义仓储接口，也不持有连接；读取由外壳调用 store 完成后把领域值交给核心。

diesel 不暴露 SQLite 原生句柄，所以备份不用 Backup API，改用 `VACUUM INTO '<路径>'`：它在一个读事务内写出一致且紧凑的副本，不阻塞写连接。

### 4.6 容量估算

2026-11-16 用本章 schema（含全部索引）在 SQLite 中生成一年模拟数据并测量，`VACUUM` 后的文件大小如下。模拟假设每天都有 10 小时活跃（偏高估计），标题平均 60 字节 UTF-8（中英混合），每天 5% 的新标题会在之后反复出现。

| 档位 | 每天前台区间 | 每天网页区间 | 每天新标题 | 一年大小 | 每天 |
|---|---|---|---|---|---|
| 轻度 | 600 | 250 | 160 | 55 MiB | 0.15 MiB |
| 典型 | 2,000 | 900 | 550 | 187 MiB | 0.51 MiB |
| 重度（终端、播放器等标题频繁变化） | 6,000 | 2,500 | 1,700 | 551 MiB | 1.5 MiB |

典型档的构成：

| 对象 | 大小 | 占比 |
|---|---|---|
| `intervals` 表 | 70.6 MiB | 38% |
| `intervals` 的 4 个索引（含 `UNIQUE (run_id, sequence)`） | 39.6 MiB | 21% |
| `web_intervals` 表与 3 个索引 | 41.9 MiB | 22% |
| `titles` 表 | 13.5 MiB | 7% |
| `titles.text` 的 UNIQUE 自动索引（完整复制一份标题文本） | 13.5 MiB | 7% |
| 其他（窗口实例、可用状态、回执等） | 约 8 MiB | 5% |

按每年约 250 个工作日计，实际大小约为上表的 70%。前台区间行连同索引约 150 字节，其中枚举文本（`'window'`、`'full'`、`'title_changed'` 等）约 25 字节，已结束行的确认点与结束时间重复约 12 字节。

体积优化候选（同一模拟测得四项合计使典型档从 187 MiB 降到 141 MiB，−25%）：

1. `titles` 用 64 位哈希列加普通索引去重，替代 `text` 上的 UNIQUE。插入时按哈希查找并比较文本，节省约 10 MiB/年。
2. 去掉 `UNIQUE (run_id, sequence)`。单写者按顺序分配 `sequence`，不需要唯一约束，节省约 9.5 MiB/年。
3. 枚举列存小整数，Rust 侧枚举显式指定数值，另提供只读视图把整数映射为文本，供排查使用。节省约 25 MiB/年。
4. 已结束行的 `confirmed_*` 置 NULL，因为结束后它与结束时间相同。

决定（用户决策 9）：采用第 2、3、4 项，已写入本章 schema；不采用第 1 项，标题保留 UNIQUE 文本索引，便于直接读库，代价约 10 MiB/年。采用后的模拟结果：

| 档位 | 一年大小 | 变化 |
|---|---|---|
| 轻度 | 44 MiB | −20% |
| 典型 | 150 MiB | −20% |
| 重度 | 442 MiB | −20% |

M1 用真实使用数据重测，结果记入 `docs/measurements.md`；`gen-data` 复现以上三档参数。

`quartz gen-data --db <新文件> --profile light|typical|heavy --seed <整数> --days <天数>` 默认使用 typical、seed 1 和 365 天，以 2025-01-01 为固定日期起点。工作日使用表中的前台切片数和新标题数，活跃 10 小时；周末均减半，活跃 5 小时。切片包括无前台事实，UTC 午夜拆分会增加存储行，少量切片由采集缺口替代。每 200 个切片的前 40 个按一秒间隔改变播放器或终端标题，其余时间分配给普通切片。每天包含一个 4,096 UTF-16 单元的标题，其他标题从 24、36、48、72 字节的长度档取样并调整填充，使当天新标题平均约 60 字节 UTF-8；保留唯一前缀，每天 5% 的新标题加入后续日期的复用池。

生成器按天通过 `CaptureWrite` 和采集存储事务写入，包含 idle、locked、suspended、UTC 午夜片段及少量额外时钟段和采集缺口；睡眠期间单调时钟不推进。当前采集写入器尚未支持网页行，因此三档的 250、900、2,500 个网页切片参数会输出为待生成数量，`web_intervals` 保持为空。结束时输出各表行数、数据库文件字节数、生成速度，并对首日、中间日和末日执行存储层时长守恒检查。生成路径必须是生产数据目录外的新文件。 typical 档 30 日数据库的生成速度、体积与行数见 [2026-11-18 测量记录](../measurements.md)。

## 5. 查询管线与缓存

### 5.1 管线

所有页面（今天、本周、时间轴、排行、规则预览）都走同一条管线，保证结果一致：

```text
1. 从一致快照读取前台、可用状态、已关联网页、缺口和标注；开放行只取已确认部分。按事实引用补载所属时钟段，闭合段保留自身端点
2. 在各时钟段内按查询范围与全部事实、状态、标注边界切片，保留 idle 掩码
3. 提取文档身份；分类并附命中来源
4. 应用人工分类覆盖和有效活动覆盖；离线活动只补没有有效前台活动的范围
5. 计算每片的活动状态与有效时长
6. 按应用、文档、域名、分类、小时聚合，或输出时间轴片段
```

`category_override`、`offline_activity`、`active_override` 与 `dismissed_away` 采用同类型后写入替换先写入：完全覆盖的旧标注删除，部分覆盖的旧标注裁剪；旧标注严格包含新范围时拆成左右两段。`note` 允许任意重叠，写入时不裁剪其他 note。不同类型之间保留重叠。范围使用左闭右开语义，端点相接不算重叠；同一请求内同一裁剪类型的最终范围不得重叠，违反时整个事务返回 `invalid_range`。

裁剪保留原内容；仅有一个剩余片段时保留旧 UUID，拆成两段时左段保留旧 UUID，右段的 UUID 从旧标注 UUID、本次写入实体 UUID，以及拆分当时两行各自的范围确定生成。具体为 SHA-256 的输入依次拼接 UTF-8 前缀 `quartz.annotations.right.v1`、一个零字节、旧 UUID 的规范小写文本、写入实体 UUID 的规范小写文本，再依次拼接旧行的开始与结束 UTC 毫秒数、写入行的开始与结束 UTC 毫秒数（四个值均为小端序 i64 的 8 字节表示），取前 16 字节，按 RFC 9562 设置 UUIDv8 版本位与变体位，再编码为小写 UUID。批量写入按请求顺序裁剪，拆分身份冲突会回滚事务。裁剪运算与身份生成位于 `store::annotations::clipping`，直接以值测试；SQL 和版本更新位于 `store::annotations`，不依赖查询管线。

创建遇到已有 UUID 时返回当前已有实体，不再次裁剪；更新按 UUID 设置最终值，最终值未变时保留时间戳；删除不存在的 UUID 也成功。删除标注不恢复此前被裁掉的部分。`active_override` 可以恢复被 idle 掩码覆盖的允许计时窗口；`excluded` 与 `no_foreground` 只参与覆盖检查和时间轴。`dismissed_away` 持久记录“忽略离开”，不改变时长，删除该标注即可撤销该忽略记录。

活跃时长 = 允许计时的窗口事实中非空闲部分 + 有效活动覆盖新增的部分 + 离线活动标注补入的部分。按时钟段内的有效时间并集计算，避免重复累计。停止片段在时钟段内按该段的单调映射计算时长，离线活动覆盖沿用该时长；睡眠恢复结算的可用状态可以延伸到所属时钟段的单调覆盖之外；这些范围保留停止原因集合，以 UTC 端点差计时，派生片段的时钟段 ID 为空。时钟段外的离线活动按 UTC 端点差计时。专注与分心占比以此活跃时长为分母。锁屏、睡眠、断开、暂停和核心离线显示停止原因集合；idle 保留为独立掩码。

`category_override` 只替换 Active 或 ActiveOverride 窗口片段的分类，并优先于 `active_override` 的可选分类；未指定分类的 `active_override` 保留规则分类。`offline_activity` 填入 Idle、Stopped、Gap、NoForeground 和没有采集数据的范围，保留 Active、ActiveOverride 与 Excluded 片段；未指定分类时为未分类。注解引用的分类已归档或不存在时，结果为未分类，并记录注解 UUID 和分类 ID 诊断。离线活动不附带应用或标题，不列入应用排行；切换次数按注解 UUID 将各离线活动视为一个应用身份。

管线输出“派生片段”：

```rust
struct DerivedSegment {
    start_utc_ms: i64,
    end_utc_ms: i64,
    clock_segment_id: Option<i64>,
    interval_id: Option<i64>,
    web_interval_id: Option<i64>,
    app_id: Option<i64>,
    window_instance_id: Option<i64>,
    title_id: Option<i64>,
    extraction: Option<ExtractionRef>,
    category_id: Option<i64>,
    category_source: CategorySource, // Rule(rule_id) | Annotation(annotation_id) | Unclassified
    activity: Activity,
    capture_level: Option<CaptureLevel>,
}
```

`Activity` 的取值为 `Active`、`Idle`、`ActiveOverride(annotation_id)`、`OfflineActivity(annotation_id)`、`NoForeground`、`Excluded`、`Stopped(reason)`、`Gap(reason)`。`Stopped(reason)` 中的 reason 是同时生效的停止原因集合。没有前台事实的片段，其 `capture_level` 为 None。

### 5.2 文档身份提取

规则按以下顺序尝试，第一个匹配的生效：

1. 提取规则集树解析后的生效规则，按规则格式文档 1.2 节的顺序（用户子规则集更深，因此同优先级时优先于内置）；内置规则集 `builtin.extraction` 首版覆盖 VS Code、JetBrains 系列、Office、浏览器 profile 后缀、Windows Terminal；
2. 通用回退：去掉"` - <应用显示名>`"后缀，剩下的部分作为文档名。

输出 `{document, project, profile, dirty, normalized_title, extractor_rule, status}`，其中 `extractor_rule` 为 `<规则集>/<规则>@<版本>`。`status` 取 `extracted`、`fallback`、`empty` 或 `unknown`。

文档身份为 `(app_id, document, project, profile)`。三个文本分量按 NFC 规范化并保留大小写；缺失（null）与空字符串不同。`document` 缺失、为空或 `status` 为 `empty` / `unknown` 时没有文档身份；隐私投影隐藏应用或标题时也不生成身份。`dirty`、`normalized_title`、`extractor_rule` 不参与身份。相同应用中同名但项目或 profile 不同的文档分开统计。

查询只向客户端提供不透明 `document_ref` 与允许显示的提取字段；引用绑定生成它的查询输入版本和覆盖日期范围，不能作为持久实体主键。文档排行、详情与时间轴共用此身份，引用格式和过期处理见 [04 第 6.2.1 节](04-ipc.md#621-m4-查询补充)。

标题格式处理只在已识别的格式里进行。比如 VS Code 的 `● ` 前缀，只在 VS Code 的规则中去掉，并记为 `dirty = true`。通用回退不删除数字和括号，以免把合法的文件名改掉。

### 5.3 分类

- 生效规则按规则格式文档 1.2 节排序（`priority` 大者优先；同优先级时所在规则集更深者优先；再按树的遍历顺序与文件顺序），第一条匹配的规则决定分类。
- 浏览器在前台且有网页区间时，规则可以同时看到 `web.*` 和 `title.*` 字段。
- 没有规则匹配就是"未分类"。

默认分类有 6 个：开发（`development`）、学习（`learning`）、沟通（`communication`）、娱乐（`entertainment`）、系统与工具（`system`）、未分类。括号内为 `builtin_key`，规则文本中写作 `builtin:<键>`；未分类不是分类行，而是“没有规则命中”。每个分类带一个倾向（专注、中性、分心）。默认值如下，用户新建分类的倾向默认为中性：

| 内置键 | 倾向 | 色板键 | 排序 |
|---|---|---|---|
| `development` | `focus` | `cat.8` | 0 |
| `learning` | `focus` | `cat.1` | 1 |
| `communication` | `neutral` | `cat.3` | 2 |
| `entertainment` | `distraction` | `cat.13` | 3 |
| `system` | `neutral` | `cat.10` | 4 |

`builtin.classification` 包含 52 条规则：开发 19 条、学习 10 条、沟通 10 条、娱乐 8 条、系统与工具 5 条。规则以可执行文件名精确匹配，覆盖编辑器、开发环境、终端、Office、阅读与学习工具、聊天、邮件、会议、游戏及启动器、播放器和系统工具；另以提取文档名精确匹配记事本中的 `Cargo.toml` 和 `pyproject.toml`，将这两类项目配置归为开发；其他记事本文档和浏览器按应用保持未分类。M4 分类导入接受 `app.identity_key`、`app.exe_name`、`app.display_name`、`window.class`、`title.raw`、`title.normalized`、`doc.document`、`doc.project`、`doc.profile`；窗口类名取自已存储窗口实例，提取先于分类。网页字段在 M5 接入；未知或尚未提供的字段返回 `E-RULE-FIELD`。

分类规则只读取采集级别允许保留的字段：`TitleHidden` 提供应用与已保留的窗口类名，标题及提取字段缺失；`Excluded` 不参与分类。普通条件对缺失字段不成立，取反条件的行为遵循规则格式第 2 节。导入时把内置键、分类名或 UUID 解析为分类 UUID。UUID 与 `builtin:` 使用独立标识目录，分类名称只在唯一匹配时可用；歧义名称返回 `E-RULE-REF`。创建和改名拒绝使用 UUID 格式或 `builtin:` 命名空间的名称；已有此类名称保留显示与改名入口，规则解析始终按稳定标识处理；查询中第一条匹配规则引用的分类已归档或不存在时，结果为未分类，并记录包含规则 ID 和分类 UUID 的诊断。

专注和分心占比以活跃时长为分母，分母为零时均为 0。最长连续专注段是同一时钟段内 UTC 首尾相接、倾向均为 `focus` 的活跃片段的单调时长之和；允许跨应用和跨专注分类，非活跃片段、其他倾向或时间间隙会中断连续段。没有时钟段的连续专注离线活动按 UTC 时长累计；进入或离开采集时钟段会中断连续段。

### 5.4 缓存

分类按前台事实计算一次，切片通过查询内的结果索引读取分类。单次查询内使用 `HashMap`，键包含实际读取的应用、窗口类名、标题与采集等级，规则哈希由持有缓存的规则快照固定；读取 `title.raw`、`title.normalized` 或 `doc.*` 时包含标题 ID，读取 `window.class` 时包含其值，纯应用规则可以跨标题复用结果。应用条件在每次查询内按应用元数据求值一次，按原顺序保留尚需读取标题或窗口字段的分类条件；其余结果可跨该应用的不同标题复用。缓存键中的标题和窗口类名仅由这些剩余条件决定。只为规则实际读取的字段构造规范化文本。应用元数据按批次收集尚未读取的应用 ID，在同一数据库快照内读取一次并通过共享引用供事实使用；分类元数据和规则标识也共享存储，切片与聚合克隆引用。查询结束即释放这些缓存和结果表。`TitleHidden` 的标题键为空，`Excluded` 不进入缓存。分类与日结果的跨查询 LRU 只在 `bench-query` 实测的日、周查询 P95 未达到 08 第 1.4 节目标时启用；M4 必须交付跨查询提取缓存，并测量完整管线；分类与日结果缓存继续按此条件启用（已有测量见[测量记录](../measurements.md)）。

| 层 | 键 | 值 | 失效 |
|---|---|---|---|
| 提取缓存 | `(app_id, title_id, capture_level, extractor_version, metadata_version)` | 提取结果 | 提取规则或元数据版本变化 |
| 分类匹配缓存 | `(classification_version, extractor_version, metadata_version, app_id, title_id, capture_level, window_class, domain_id, tab_title_id, stream_id)` | `(category_id, rule_id)` | 任一输入版本变化 |
| 日结果缓存 | `(逻辑日期, 时区, 日起点偏移, 查询语义版本, metadata_version, 各规则版本, 覆盖的 UTC 日期的 data_day_versions 摘要)` | 该日完整的派生片段和各维度完整聚合 | 任一依赖版本变化 |

分类匹配字段与缓存依赖：

| 字段 | 依赖 |
|---|---|
| `app.identity_key`、`app.exe_name` | `app_id` |
| `app.display_name` | `app_id`、`metadata_version` |
| `window.class` | 已存储的窗口类名值 |
| `title.raw` | `title_id` |
| `title.normalized`、`doc.*` | `app_id`、`title_id`、`extractor_version`、`metadata_version` |
| `web.domain` | `domain_id` |
| `web.tab_title` | `tab_title_id` |
| `browser.profile_label` | `stream_id`、`metadata_version` |
| 分类名称、颜色、倾向、层级 | `metadata_version`，影响展示与聚合 |

缺失字段用独立空值，与空字符串区分。分类条件的增删改由 `classification_version` 覆盖。

- 提取缓存使用内存 LRU，最多 4,096 条并按实际字节计费，达到查询内存预算时提前逐出；按实测启用的分类缓存也最多 4,096 条；日结果最多 31 天，总字节预算 8 MiB。单日超过 2 MiB 不缓存，直接分页计算。
- 日结果保存完整的分组，不截断成前 N 名。展示时再取前 N 名，否则跨日累加会出错。
- 今天的结果分两部分：已完成的部分可以缓存，进行中的区间每次请求时现算。
- 计算时读取一个一致快照。发布缓存前再检查一次输入版本，有变化就丢弃这次结果。
- 展示数据按需计算。托盘摘要只在打开菜单、刷新提示（60 秒一次或悬停）、policy_changed 或今天的日版本变化时计算今天的聚合；计算在 I/O runtime 发起，主线程展示最新缓存（07 第 1 节）。
- 内存缓存启用后仍未达性能目标时，持久日缓存 `derived_day_cache` 作为独立工作包评审，再决定是否交付。

### 5.5 正则安全

用户写的正则在保存时编译，并设置 `size_limit = 1 MiB`、`dfa_size_limit = 1 MiB`。输入文本上限为 4,096 个 UTF-16 单元，与采集、扩展和隐私求值一致（02 第 1.1 节）。规则预览查询每处理 5,000 个片段检查一次取消标志。

## 6. 保留与删除

| 数据 | 默认保留期 | 到期处理 |
|---|---|---|
| 前台区间、可用状态、采集缺口 | 永久 | — |
| 标题文本 | 永久，可设置（例如 90 天） | 区间保留，`title_id` 置空，`title_status = purged`；不再被引用的 `titles` 行删除 |
| 网页区间的标签标题 | 同标题 | `tab_title_id` 置空，`tab_title_status = purged`，再清理无引用标题 |
| 域名 | 永久 | — |
| 标注 | 永久 | 不受自动保留期影响 |
| `ingest_receipts`、`deletion_barriers` | 14 天；未完成清理任务的屏障继续保留 | 按接收或创建时间清理已到期且不再被任务使用的记录 |
| 迁移备份 | 最近 2 份 | 删除更旧的迁移备份；手动备份由用户管理 |

M4 交付隐私规则的历史字段降级；下列通用范围删除与保留期清理在 M6 交付。历史任务范围、回执和完成状态见 [06 第 4.2 节](06-privacy.md#42-历史隐私清理)。

删除操作：

- **按日期范围删除**：删除范围内的区间、网页区间、可用状态、采集缺口和标注；与范围相交的区间裁剪到范围外的部分。
- **按应用、域名或标题关键词删除**：删除匹配的区间，或只清除标题。
- **按隐私规则删除历史**：见 06。

删除或历史降级先在事务中写 `deletion_barriers`，`scope` 保存筛选条件与删除或字段降级动作，同时推进数据版本。随后每批处理 2,000 行；所有在途写入与迟到消息都先检查屏障及当前隐私策略。影响当前区间时，清除相关候选上下文、重建当前采集状态并刷新 `state` 与托盘。向受影响的扩展发送 `browser.purge {from_ms, to_ms}`，扩展清队列后回确认；未确认的 purge 请求保存在 settings，重连后补发（05 第 1.5 节）。

标题清理分别清空前台 `title_id` 和网页 `tab_title_id`，写对应状态，再删除两处均不再引用的 `titles`。同样清理无引用的 `domains` 与 `window_instances`，更新 `data_day_versions` 并失效对应日期缓存。

全局启用 `secure_delete = ON`；清理完成后结束相关读快照并执行 `wal_checkpoint(TRUNCATE)`，处理 WAL 残留。按隐私规则清理历史时，显示备份清单并让用户选择一并删除。验收分为 SQL 可见性检查与物理文件检查，后者包含主库、WAL 和用户选定删除的备份。

## 7. 版本升级与兼容

一次升级会同时变化多个版本，各自独立处理：

| 对象 | 版本来源 | 处理 |
|---|---|---|
| 数据库 schema | diesel 迁移 | 第 8.1–8.4 节 |
| 内置规则集（分类、提取、隐私） | 规则集版本 | 第 8.5 节 |
| 隐私解释 ID | `<规则集>/<规则>@<版本>` | 历史行保留当时的规则 ID 与标签快照，规则集升级后仍可解释 |
| JSON 配置列 | serde 默认值与忽略未知字段 | 第 8.6 节 |
| 核心、界面、host 协议 | `hello` 协商的协议版本范围（04） | `quartz.exe` 与 `quartz-host.exe` 随同一发行包升级；仍在运行的旧界面重连得到 `proto_mismatch`，提示重启 |
| 浏览器扩展 | `browser.hello` 的协议版本（05） | 扩展由商店单独更新，核心同时支持当前与上一个协议版本 |
| 内存缓存 | 键中的各类版本号 | 进程重启即清空，无需迁移 |

## 8. schema 迁移

### 8.1 迁移文件

- 迁移放在 `crates/quartz/migrations/<时间戳>_<名称>/up.sql`，用 `embed_migrations!` 编进 exe。不提供 `down.sql`：Quartz 不支持把数据库降级到旧 schema。
- 已发布的迁移不可修改。仓库里的 `migrations.lock` 记录每个已发布迁移的 SHA-256，CI 校验文件未变。diesel 自身不校验迁移内容，这一检查由我们补上。
- `store/schema.rs` 必须与迁移一致。CI 在空库上执行全部迁移，再运行 `diesel print-schema`，与提交的文件逐字节比较。CHECK 约束和部分索引不出现在 schema.rs 中，由迁移测试覆盖。本地执行 `deno task check-schema` 完成空库迁移与生成结果比较，需要 Deno 2.9.8 与 Diesel CLI 2.4.0（`sqlite-bundled`）；脚本使用仓库中的 `diesel.toml` 与 `crates/quartz/schema.patch`，在 Windows 和 Linux 上均可运行。

### 8.2 启动流程

核心在拿到数据目录锁之后、启动采集之前执行：

1. 先通过只读连接读取已应用的迁移版本，与 exe 内嵌的迁移比较；确认兼容后再打开写连接。
2. **数据库更新、程序更旧**：已应用的版本里有 exe 不认识的迁移。核心拒绝启动，不写入数据库，核心以 `newer_database` 错误码和可用备份路径启动独立 UI 错误窗口后退出，由 UI 本地化说明并列出恢复命令。
3. **没有待执行迁移**：正常启动。
4. **有待执行迁移**：先用 `VACUUM INTO` 写出 `backups\pre-migrate-<旧版本>-<时间>.db`，备份失败就不迁移。再按顺序执行迁移，托盘提示“正在升级数据库”。全部成功后清理更旧的迁移备份，只保留最近 2 份。
5. **迁移失败**：核心停止，不启动采集，数据库停在最后一个成功的迁移。核心以 `migration_failed` 错误码和备份路径启动独立 UI 错误窗口后退出；UI 给出备份路径和 `quartz restore-backup` 命令，用户可以用 `quartz restore-backup <文件>` 恢复后换回旧版本 exe，或等待修复版本继续迁移。

`quartz restore-backup <文件>` 先用英文列出将要恢复的备份、被替换的 `quartz.db` 路径以及备份之后的记录会丢失的提示；未提供 `--yes` 时返回非零退出码，不创建目录、不获取锁、不打开数据库。用户确认后执行 `quartz restore-backup <文件> --yes`；命令持有数据目录锁，检查备份并生成独立副本，同步到磁盘，对可读取的目标库完成 WAL checkpoint，并检查目标文件的 SQLite 排他锁；目标库返回类型化的 `SQLITE_CORRUPT` 或 `SQLITE_NOTADB` 时允许跳过 checkpoint，仍须通过排他锁检查。锁被其他连接占用时拒绝恢复。关闭目标连接后删除目标的 `-wal`、`-shm`、`-journal`，再原子替换主库；数据目录锁覆盖整个过程。替换前保留原主库，任何前置步骤失败都保留已同步的暂存副本。恢复直接替换当前数据库；需要保留当前记录时，用户应先自行备份。命令不读取标准输入。

### 8.3 编写约束

- 每个迁移默认在一个事务内执行。SQLite 的 `PRAGMA foreign_keys` 在事务内不能修改，所以需要重建表时：
  - 能在事务内完成的，用 `PRAGMA defer_foreign_keys = ON`，按“建新表 → 复制 → 删旧表 → 改名 → 重建索引”完成，最后执行 `PRAGMA foreign_key_check`，有违例就让迁移失败；
  - 确实需要关闭外键检查的，在迁移目录的 `metadata.toml` 中设 `run_in_transaction = false`，迁移脚本自己负责 `BEGIN`、`foreign_keys` 切换和检查。这类迁移在评审时单独标出。
- 迁移只改 schema 和数据，不依赖程序当前的规则或设置。
- 需要 Rust 逻辑的数据迁移（例如按新规则重算 `session_key`、把域名统一转成 punycode）实现 diesel 的 `Migration` trait，与内嵌的 SQL 迁移放进同一个自定义 `MigrationSource`，共用一个版本序列。
- 迁移耗时用 `gen-data` 生成的一年数据库测量。超过 10 秒的迁移要分批，并在托盘显示进度。

### 8.4 测试

- 每个发布版本都保存一份小型样本数据库到 `tests/fixtures/db/<版本>.db`，包含各类表与边界数据。测试把每份样本迁移到最新版本，检查数据断言。
- 迁移后的 schema（按名称排序的 `sqlite_schema`）必须与空库直接执行全部迁移得到的 schema 相同。
- 一年模拟数据迁移的耗时与峰值内存满足 8.3 的限制。
- 模拟“数据库更新、程序更旧”和迁移中途失败，确认核心不写入、不采集，备份可以恢复。

### 8.5 内置规则集升级

内置规则集（`builtin.classification`、`builtin.extraction`、`builtin.privacy`）以 `quartz-rules/1` 文本编译进 exe，带版本号。核心启动时先按 `categories.builtin_key` 插入缺少的五个内置分类，`name` 为 NULL，UUID 采用固定内置值；已有行的名称、颜色、倾向、排序、层级和归档状态全部保留。同一事务中只要插入了分类行，就把 `metadata_version` 加一，提交后发布元数据变化。随后安装内置规则集：

1. exe 中的版本高于数据库当前版本时，写入新版本并设为当前版本；旧版本保留。exe 中新出现的内置规则集直接创建。
2. 重新解析每类规则集树，检查用户子规则集的差异：
   - `override` 或 `disable` 的目标在新版本中不存在 → 该条差异标为“目标已删除”，暂不生效；
   - 被覆盖的内置规则内容变化 → 标为“上游已变更”，规则页提供三方对比（`parent_version` 指向的旧内置版本、新内置版本、用户覆盖），由用户选择保留或删除覆盖。
3. 不自动合并、不修改用户规则集。生效规则哈希变化后，缓存自然失效（第 2.7 节）。

### 8.6 JSON 配置列

`settings.value`、规则 `conditions`、`deletion_barriers.scope` 等 JSON 列用 serde 解析：新增字段带默认值，未知字段忽略，因此增加字段不需要迁移。字段改名、语义变化或删除时，写一个 Rust 数据迁移（8.3）改写已有 JSON，不在读取路径上兼容多种旧格式。

## 9. 导入导出

首版之后的独立工作包，这里只记录约束：

- 导出格式为 JSON Lines，包含格式版本、事实、规则、标注。CSV 只导出报表。
- 从 Taix 导入时，记录来源和原始主键，保证可以重复导入而不重复计算。Taix 只有小时汇总的数据不能还原切换顺序，在时间轴上显示为"导入汇总"。
- 旧数据使用本地时间时，导入前让用户确认历史时区；遇到夏令时有歧义的时间要提示，不能静默选择。
