-- Schema v1: docs/design/03-data-model.md sections 2 and 3.

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
  name             TEXT NOT NULL,
  enabled          INTEGER NOT NULL DEFAULT 1, -- 停用时整个子树不生效
  sibling_order    INTEGER NOT NULL,       -- 同一父级下的顺序
  current_version  INTEGER NOT NULL,
  chain_id         INTEGER,                -- kind = precedent 时所属的链（12，M7）
  created_utc_ms   INTEGER NOT NULL
);

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

CREATE TABLE data_day_versions (
  source           INTEGER NOT NULL,       -- 1 intervals | 2 availability | 3 web | 4 annotations | 5 gaps
  utc_date         INTEGER NOT NULL,    -- 自 1970-01-01 起的天数
  version          INTEGER NOT NULL,
  PRIMARY KEY (source, utc_date)
) WITHOUT ROWID;

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

CREATE TABLE enum_labels (
  enum_name        TEXT NOT NULL,          -- 如 'intervals.end_reason'；共用的枚举用类型名，如 'title_status'
  code             INTEGER NOT NULL,
  label            TEXT NOT NULL,          -- 与列注释、Rust 枚举变体名一致
  deprecated       INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (enum_name, code)
) WITHOUT ROWID;

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

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('capture_runs.end_reason', 1, 'shutdown'),
  ('capture_runs.end_reason', 2, 'user_exit'),
  ('capture_runs.end_reason', 3, 'crash_recovered'),
  ('capture_runs.end_reason', 4, 'update');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('clock_segments.end_reason', 1, 'time_change'),
  ('clock_segments.end_reason', 2, 'resume'),
  ('clock_segments.end_reason', 3, 'drift'),
  ('clock_segments.end_reason', 4, 'run_end'),
  ('clock_segments.end_reason', 5, 'crash_recovered');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('applications.kind', 1, 'exe'),
  ('applications.kind', 2, 'aumid'),
  ('applications.kind', 3, 'unresolved');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('intervals.kind', 1, 'window'),
  ('intervals.kind', 2, 'no_foreground');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('intervals.no_foreground_reason', 1, 'empty'),
  ('intervals.no_foreground_reason', 2, 'unknown_desktop');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('title_status', 1, 'ok'),
  ('title_status', 2, 'empty'),
  ('title_status', 3, 'unreadable'),
  ('title_status', 4, 'truncated'),
  ('title_status', 5, 'redacted'),
  ('title_status', 6, 'purged');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('intervals.capture_level', 1, 'full'),
  ('intervals.capture_level', 2, 'title_hidden'),
  ('intervals.capture_level', 3, 'excluded');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('intervals.end_reason', 1, 'switch'),
  ('intervals.end_reason', 2, 'title'),
  ('intervals.end_reason', 3, 'no_foreground'),
  ('intervals.end_reason', 4, 'locked'),
  ('intervals.end_reason', 5, 'suspended'),
  ('intervals.end_reason', 6, 'paused'),
  ('intervals.end_reason', 7, 'disconnected'),
  ('intervals.end_reason', 8, 'shutdown'),
  ('intervals.end_reason', 9, 'crash_recovered'),
  ('intervals.end_reason', 10, 'clock_jump'),
  ('intervals.end_reason', 11, 'midnight_split'),
  ('intervals.end_reason', 12, 'policy_changed'),
  ('intervals.end_reason', 13, 'capture_gap');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('availability_intervals.reason', 1, 'idle'),
  ('availability_intervals.reason', 2, 'locked'),
  ('availability_intervals.reason', 3, 'disconnected'),
  ('availability_intervals.reason', 4, 'suspended'),
  ('availability_intervals.reason', 5, 'paused');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('availability_intervals.end_reason', 1, 'resumed'),
  ('availability_intervals.end_reason', 2, 'shutdown'),
  ('availability_intervals.end_reason', 3, 'crash_recovered'),
  ('availability_intervals.end_reason', 4, 'clock_jump'),
  ('availability_intervals.end_reason', 5, 'midnight_split'),
  ('availability_intervals.end_reason', 6, 'policy_changed');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('availability_intervals.exception', 1, 'keep_alive'),
  ('availability_intervals.exception', 2, 'app_exempt'),
  ('availability_intervals.exception', 3, 'audible');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('web_intervals.end_reason', 1, 'browser_state'),
  ('web_intervals.end_reason', 2, 'foreground_end'),
  ('web_intervals.end_reason', 3, 'disconnected'),
  ('web_intervals.end_reason', 4, 'gap'),
  ('web_intervals.end_reason', 5, 'policy_changed'),
  ('web_intervals.end_reason', 6, 'crash_recovered'),
  ('web_intervals.end_reason', 7, 'shutdown');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('web_intervals.link_status', 1, 'linked'),
  ('web_intervals.link_status', 2, 'ambiguous');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('web_intervals.web_level', 1, 'full'),
  ('web_intervals.web_level', 2, 'domain_only');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('ingest_receipts.outcome', 1, 'stored'),
  ('ingest_receipts.outcome', 2, 'discarded_no_overlap'),
  ('ingest_receipts.outcome', 3, 'discarded_privacy'),
  ('ingest_receipts.outcome', 4, 'discarded_gap'),
  ('ingest_receipts.outcome', 5, 'discarded_barrier');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('categories.tendency', 1, 'focus'),
  ('categories.tendency', 2, 'neutral'),
  ('categories.tendency', 3, 'distraction');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('rule_sets.kind', 1, 'classification'),
  ('rule_sets.kind', 2, 'extraction'),
  ('rule_sets.kind', 3, 'privacy'),
  ('rule_sets.kind', 4, 'precedent'),
  ('rule_sets.kind', 5, 'policy_check');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('rule_sets.source', 1, 'builtin'),
  ('rule_sets.source', 2, 'user');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('annotations.kind', 1, 'category_override'),
  ('annotations.kind', 2, 'offline_activity'),
  ('annotations.kind', 3, 'active_override'),
  ('annotations.kind', 4, 'dismissed_away'),
  ('annotations.kind', 5, 'note');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('data_day_versions.source', 1, 'intervals'),
  ('data_day_versions.source', 2, 'availability'),
  ('data_day_versions.source', 3, 'web'),
  ('data_day_versions.source', 4, 'annotations'),
  ('data_day_versions.source', 5, 'gaps');

INSERT INTO enum_labels (enum_name, code, label) VALUES
  ('capture_gaps.reason', 1, 'event_overload'),
  ('capture_gaps.reason', 2, 'write_queue_overload'),
  ('capture_gaps.reason', 3, 'database_error');

CREATE VIEW intervals_v AS
SELECT t.*,
       e_kind.label AS kind_label,
       e_no_foreground_reason.label AS no_foreground_reason_label,
       e_title_status.label AS title_status_label,
       e_capture_level.label AS capture_level_label,
       strftime('%Y-%m-%d %H:%M:%f', t.start_utc_ms / 1000.0, 'unixepoch', 'localtime') AS start_local,
       strftime('%Y-%m-%d %H:%M:%f', t.end_utc_ms / 1000.0, 'unixepoch', 'localtime') AS end_local,
       strftime('%Y-%m-%d %H:%M:%f', t.confirmed_utc_ms / 1000.0, 'unixepoch', 'localtime') AS confirmed_local,
       e_end_reason.label AS end_reason_label,
       a.display_name AS application_name,
       title.text AS title
FROM intervals t
LEFT JOIN enum_labels e_kind ON e_kind.enum_name = 'intervals.kind' AND e_kind.code = t.kind
LEFT JOIN enum_labels e_no_foreground_reason ON e_no_foreground_reason.enum_name = 'intervals.no_foreground_reason' AND e_no_foreground_reason.code = t.no_foreground_reason
LEFT JOIN enum_labels e_title_status ON e_title_status.enum_name = 'title_status' AND e_title_status.code = t.title_status
LEFT JOIN enum_labels e_capture_level ON e_capture_level.enum_name = 'intervals.capture_level' AND e_capture_level.code = t.capture_level
LEFT JOIN enum_labels e_end_reason ON e_end_reason.enum_name = 'intervals.end_reason' AND e_end_reason.code = t.end_reason
LEFT JOIN applications a ON a.id = t.app_id
LEFT JOIN titles title ON title.id = t.title_id;

CREATE VIEW availability_intervals_v AS
SELECT t.*,
       e_reason.label AS reason_label,
       strftime('%Y-%m-%d %H:%M:%f', t.start_utc_ms / 1000.0, 'unixepoch', 'localtime') AS start_local,
       strftime('%Y-%m-%d %H:%M:%f', t.end_utc_ms / 1000.0, 'unixepoch', 'localtime') AS end_local,
       strftime('%Y-%m-%d %H:%M:%f', t.confirmed_utc_ms / 1000.0, 'unixepoch', 'localtime') AS confirmed_local,
       e_end_reason.label AS end_reason_label,
       strftime('%Y-%m-%d %H:%M:%f', t.last_input_utc_ms / 1000.0, 'unixepoch', 'localtime') AS last_input_local,
       strftime('%Y-%m-%d %H:%M:%f', t.detected_utc_ms / 1000.0, 'unixepoch', 'localtime') AS detected_local,
       e_exception.label AS exception_label
FROM availability_intervals t
LEFT JOIN enum_labels e_reason ON e_reason.enum_name = 'availability_intervals.reason' AND e_reason.code = t.reason
LEFT JOIN enum_labels e_end_reason ON e_end_reason.enum_name = 'availability_intervals.end_reason' AND e_end_reason.code = t.end_reason
LEFT JOIN enum_labels e_exception ON e_exception.enum_name = 'availability_intervals.exception' AND e_exception.code = t.exception;

CREATE VIEW web_intervals_v AS
SELECT t.*,
       strftime('%Y-%m-%d %H:%M:%f', t.start_utc_ms / 1000.0, 'unixepoch', 'localtime') AS start_local,
       strftime('%Y-%m-%d %H:%M:%f', t.end_utc_ms / 1000.0, 'unixepoch', 'localtime') AS end_local,
       strftime('%Y-%m-%d %H:%M:%f', t.confirmed_utc_ms / 1000.0, 'unixepoch', 'localtime') AS confirmed_local,
       e_end_reason.label AS end_reason_label,
       e_tab_title_status.label AS tab_title_status_label,
       e_link_status.label AS link_status_label,
       e_web_level.label AS web_level_label,
       a.display_name AS application_name,
       title.text AS tab_title,
       d.host AS domain
FROM web_intervals t
LEFT JOIN enum_labels e_end_reason ON e_end_reason.enum_name = 'web_intervals.end_reason' AND e_end_reason.code = t.end_reason
LEFT JOIN enum_labels e_tab_title_status ON e_tab_title_status.enum_name = 'title_status' AND e_tab_title_status.code = t.tab_title_status
LEFT JOIN enum_labels e_link_status ON e_link_status.enum_name = 'web_intervals.link_status' AND e_link_status.code = t.link_status
LEFT JOIN enum_labels e_web_level ON e_web_level.enum_name = 'web_intervals.web_level' AND e_web_level.code = t.web_level
LEFT JOIN intervals i ON i.id = t.interval_id
LEFT JOIN applications a ON a.id = i.app_id
LEFT JOIN titles title ON title.id = t.tab_title_id
LEFT JOIN domains d ON d.id = t.domain_id;

CREATE VIEW capture_gaps_v AS
SELECT t.*,
       strftime('%Y-%m-%d %H:%M:%f', t.start_utc_ms / 1000.0, 'unixepoch', 'localtime') AS start_local,
       strftime('%Y-%m-%d %H:%M:%f', t.end_utc_ms / 1000.0, 'unixepoch', 'localtime') AS end_local,
       e_reason.label AS reason_label
FROM capture_gaps t
LEFT JOIN enum_labels e_reason ON e_reason.enum_name = 'capture_gaps.reason' AND e_reason.code = t.reason;

CREATE VIEW annotations_v AS
SELECT t.*,
       e_kind.label AS kind_label,
       strftime('%Y-%m-%d %H:%M:%f', t.start_utc_ms / 1000.0, 'unixepoch', 'localtime') AS start_local,
       strftime('%Y-%m-%d %H:%M:%f', t.end_utc_ms / 1000.0, 'unixepoch', 'localtime') AS end_local,
       strftime('%Y-%m-%d %H:%M:%f', t.created_utc_ms / 1000.0, 'unixepoch', 'localtime') AS created_local,
       strftime('%Y-%m-%d %H:%M:%f', t.updated_utc_ms / 1000.0, 'unixepoch', 'localtime') AS updated_local
FROM annotations t
LEFT JOIN enum_labels e_kind ON e_kind.enum_name = 'annotations.kind' AND e_kind.code = t.kind;
