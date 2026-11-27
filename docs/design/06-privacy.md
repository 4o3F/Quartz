# 06 隐私策略

Quartz 按用户决策 4 记录窗口标题。无痕页面、密码管理器条目、私信和病历都可能出现在标题里。浏览器扩展的 `incognito: "not_allowed"` 只约束扩展；Windows 层的标题由核心采集层保护。

每次观测在写入数据库、推送界面、显示托盘前先经过隐私求值。识别器读取统一观测，输出字段动作；浏览器无痕是其中一种识别器。

## 1. 基本原则

1. 入库前生效。原始标题只在主线程快照、传输队列与采集线程的有界内存中停留，求值后只输出允许字段。
2. 多个命中按字段取最严格动作。应用被排除时，其他内容全部丢弃。
3. 降级记录带修订号的规则 ID 与标签快照，供历史解释。
4. 已丢弃内容不随规则放宽恢复。
5. 排除仍保留时间边界，在时间轴显示“已排除”，不计活跃时长。
6. 按应用、窗口类名、标题模式识别的规则使用数据表；需要新系统信号时再增加识别器。

## 2. 字段动作与采集级别

| 字段 | 动作 | 合并方式 |
|---|---|---|
| `app` | `keep` / `drop` | drop 优先；drop 强制窗口标题 drop、web none |
| `window_title` | `keep` / `drop` | drop 优先 |
| `web` | `full` / `domain_only` / `none` | full < domain_only < none，取最严格 |

预设组合：

| 预设 | app | window_title | web | 显示 |
|---|---|---|---|---|
| `full` | keep | keep | full | 正常 |
| `title_hidden` | keep | drop | 保持已有动作，默认 full | 应用名 + 隐私标签 |
| `private_window` | keep | drop | none | 应用名 + 无痕或隐私标签 |
| `excluded` | drop | drop | none | 已排除 |

`intervals.capture_level` 是前台字段摘要：app drop 为 `excluded`；否则窗口标题 drop 为 `title_hidden`；其余为 `full`。`private_window` 的前台摘要也是 `title_hidden`。网页使用 `web_intervals.web_level`：full 保留域名与标签标题，domain_only 只保留域名，none 不写网页行。

网页规则命中 `web = domain_only` 时，同时丢弃关联范围的 Windows 标题，避免同一页面标题仍保存在前台事实中。全局隐私模式使用 `private_window` 组合；“不记录任何标题”使用 window_title drop、web domain_only。用户暂停是可用状态，按 02 第 3 节停止采集前台。

## 3. 识别器框架

### 3.1 观测与判决

所有来源先转换成统一观测。以下是领域结构示意：

```rust
struct Observation<'a> {
    source: ObservationSource,      // Foreground | Browser { stream_id }
    app: Option<&'a AppIdentity>,
    window: Option<WindowFacts<'a>>, // class、原始标题快照、display_affinity
    web: Option<WebFacts<'a>>,      // domain、tab_title
    browser_profile_label: Option<&'a str>,
}

struct Verdict {
    app: FieldAction,              // Keep | Drop
    window_title: FieldAction,     // Keep | Drop
    web: WebCaptureLevel,          // Full | DomainOnly | None
    label: Option<Label>,          // Key("privacy.label.private_window") | Text(用户输入)
    rule_id: Option<String>,       // builtin.privacy/browser-private-edge@3 | user.bank/bank@2 | builtin.capture_excluded@1
}

trait Detector {
    fn id(&self) -> &'static str;
    fn revision(&self) -> u32;
    fn evaluate(&self, obs: &Observation<'_>) -> Option<Verdict>;
}
```

观测不暴露 HWND 等平台类型。识别器只读观测，不做 I/O；显示亲和性等系统信号由平台层提前取得，仅在有识别器需要时查询。

动作按字段合并。解释来源先选造成该行最严格降级的规则；同级时用户规则优先于内置规则，同来源按 `sort_order` 小者优先。前台与网页行各自保存 `privacy_rule_id` 和标签：内置规则存标签键 `privacy_label_key`，显示时按当前语言翻译；用户规则存 `privacy_label` 文本快照（10 第 3 节）。对应行完整保留时，这些列均为 NULL。

### 3.2 内置识别器

| ID | 信号 | 预设 | 说明 |
|---|---|---|---|
| `builtin.browser_private` | 浏览器已接入扩展时以扩展关联为准（第 3.4 节）；未接入时用浏览器 exe + 类名 + 标题模式 | private_window，标签“无痕窗口” | M1 已验证 Edge 157、Firefox 160 可由标题识别，Chrome 157 不能（09 H5） |
| `builtin.credential_ui` | consent.exe、CredentialUIBroker.exe、LogonUI.exe 与凭据窗口 | title_hidden | 应用与窗口模式使用规则数据 |
| `builtin.password_manager` | KeePass、KeePassXC、1Password、Bitwarden 等 exe 名 | title_hidden | 名单使用规则数据 |
| `builtin.capture_excluded` | `WDA_EXCLUDEFROMCAPTURE` 或 `WDA_MONITOR` | title_hidden | 查询失败记 unknown，由其他规则继续求值 |
| `rules` | `kind = privacy` 的规则集树（含 `builtin.privacy` 与用户规则集） | 按规则字段动作 | 见第 3.3、4 节 |
| `session.privacy_mode` | 限时隐私模式 | private_window | 状态与截止 UTC 存 `timed_policies`，到期触发 policy_changed |

`GetWindowDisplayAffinity` 支持跨进程查询；窗口条件、DWM 状态会影响成功率。M1 验证常见应用窗口的查询覆盖、错误码与成本（[Microsoft 文档](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowdisplayaffinity)）。

### 3.3 规则数据表

按应用、类名、标题模式识别的内置规则，以 `quartz-rules/1` 规则集 `builtin.privacy` 编译进 exe，带版本号（规则格式见 [规则格式文档](../rules/rule-format.md)，升级见 03 第 8.5 节）。内置规则集比用户规则多三个只允许内置使用的键：`detector`（所属识别器）、`languages`（模式适用的应用界面语言）、`verified`（是否有样本验证）。

```toml
format = "quartz-rules/1"

[set]
key = "builtin.privacy"
kind = "privacy"
name = "内置隐私规则"

[[rule]]
id = "browser-private-edge"
detector = "builtin.browser_private"
label_key = "privacy.label.private_window"
match = "all"
when = [
  { field = "app.exe_name", op = "equals", value = "msedge.exe" },
  { field = "window.class", op = "equals", value = "Chrome_WidgetWin_1", case_sensitive = true },
  { field = "title.raw", op = "regex", value = 'InPrivate' },
]
languages = ["*"]               # 品牌名，各语言相同；随语言变化的模式按语言分别列出（10 第 4.2 节）
preset = "private_window"
verified = false

[[rule]]
id = "password-manager-keepassxc"
detector = "builtin.password_manager"
label_key = "privacy.label.password_manager"
match = "all"
when = [{ field = "app.exe_name", op = "equals", value = "keepassxc.exe" }]
preset = "title_hidden"
verified = true
```

`verified = false` 的条目仍生效，设置页显示未验证。所有语言变体同时生效，不按 Quartz 界面语言筛选。每个已验证条目都要有样本。`label_key` 只供内置规则使用，用户规则用 `label` 文本。

用户可以用子规则集停用或覆盖内置隐私规则（规则格式文档 1.1 节），导入时单独警告；`builtin.capture_excluded` 这类基于系统信号的识别器是代码，不在规则集中，只能在设置中开关。

### 3.4 浏览器窗口以扩展为准

浏览器窗口是否无痕以扩展为准，标题模式只在该浏览器未接入扩展时兜底。扩展 manifest 设 `incognito: "not_allowed"`（05 第 1.1 节），无痕窗口没有扩展输入，因此“扩展能确认的才是普通窗口”。

- 浏览器有已连接的扩展 stream 时：该浏览器的前台窗口在与扩展唯一关联（05 第 3.3 节）之前一律 `title_hidden`，不按标题模式判断是否无痕；取得唯一关联后以 `policy_changed` 切段，按字段动作恢复允许保留的标题与网页字段；关联失效时同样切段隐藏。不做等待缓冲。
- 浏览器没有已连接的扩展 stream 时（未安装、未连接、断线或 M5 之前）：使用 `builtin.browser_private` 的标题模式。设置页对该浏览器显示“未接入扩展，无痕识别依赖标题”；标题模式无法识别的浏览器（M1 实测 Chrome 157）在接入扩展前无痕标题会被记录，这是已知缺口。

该规则只收紧隐私：扩展报告与标题模式任一判定为无痕即按无痕处理。M5 测量 linked 覆盖率；覆盖不足时普通窗口标题被隐藏的比例同步记录。

## 4. 用户规则

用户隐私规则是 `kind = privacy` 的规则集（03 第 2.6 节），可以独立成树，也可以作为 `builtin.privacy` 的子规则集。规则写法见规则格式文档 3.3 节；字段只能使用下表中采集时即可得到的字段。

| 可用字段 | 求值时机 |
|---|---|
| `app.identity_key`、`app.exe_name`、`app.display_name` | 身份解析后同步可得 |
| `window.class`、`title.raw` | Windows 观测快照同步可得 |
| `web.domain`、`web.tab_title`、`browser.profile_label` | 关联扩展上下文后，走第 5 节延迟机制 |

保存与预览均拒绝 `doc.*` 和 `title.normalized`。操作符沿用 03 第 2.6 节。标题与正则输入上限统一为 4,096 个 UTF-16 单元，隐私求值使用截断后文本；超限状态见 02 第 1.1 节。

常见用法：

- `app.exe_name equals "wechat.exe"` → excluded。
- `app.exe_name equals "outlook.exe"` → title_hidden。
- `title.raw contains "病历"` → title_hidden。
- `web.domain domain_suffix "bank.example"` → window_title drop、web domain_only。

规则编辑器分别显示条件命中时长与有效贡献；隐私影响统计现存事实中将进一步隐藏的切片、字段和时长，包含 idle 等非活跃事实，仅使用仍保留的字段（04 第 6.3 节）。规则集每次修改生成新版本，形成 `user.bank/bank@2` 这样的解释 ID。

### 4.1 规则变化的作用范围

规则修改立即产生 `policy_changed`：原子关闭当前区间（如有），重新求值，停止原因集合为空时才开新区间，刷新 state 与托盘。“立即”的含义由规则注册表定义（01 第 6.2 节）：新规则快照替换后，采集线程在下一个事件或收到 `RulesChanged` 通知时发现 epoch 变化，用新快照产生边界；同一区间始终只按一个快照求值。隐私模式启停、空闲例外与保持计时到期也使用同一边界；限时状态存 `timed_policies`，重启后按截止 UTC 恢复。

保存只改变后续采集策略；历史隐私清理留待以后。

### 4.2 历史隐私清理

历史隐私清理（按隐私规则对已有事实执行进一步字段降级，含删除屏障、物理清理与备份联动）留待以后，届时再设计协议、任务状态机与交互。通用按日期范围删除见 [03 第 6 节](03-data-model.md#6-保留与删除) 与 [04 第 6.3 节](04-ipc.md#63-规则标注与设置)。

## 5. 执行点与异步合流

采集线程在身份解析后求值同步字段。依赖 `web.domain`、`web.tab_title` 或 `browser.profile_label` 的规则只能在关联到扩展上下文后求值：浏览器有已连接 stream 时，关联前的窗口标题按第 3.4 节隐藏，关联后把前台标题、标签标题与域名一起求值，命中标题隐藏时两种标题都丢弃，网页 domain_only 同样丢弃窗口标题；扩展未连接且存在任何依赖网页字段的隐藏规则时，直接丢弃该浏览器窗口标题。不做等待缓冲，被丢弃的标题不会在之后恢复（05 第 3.4 节）。

| 出口 | 约束 |
|---|---|
| 数据库 | 仅写降级后的字段 |
| IPC state、托盘 | 仅接收降级后的状态 |
| 日志 | 只接收降级后的事件，任何级别均不写标题、域名、URL；excluded 只记区间 ID、规则 ID、错误码 |
| 提取与分类缓存 | 只接收已允许入库的字段；缺失字段用空值参与键，不保留原文 |
| 浏览器扩展 | 不持久保存任何浏览内容（`storage.local` 只存 `profile_instance_id`）；不缓冲、不重放 |
| 崩溃转储 | 不生成完整内存转储（panic abort，不注册 MiniDumpWriteDump） |

网页字段规则隐藏窗口标题时，标签标题也要丢弃，保留域名至多为 domain_only。仅由同步 Windows 规则产生的 title_hidden 按第 2 节保持已有网页动作；private_window 与 excluded 均禁止关联。求交条件见 05 第 3.2 节。

## 6. 其他隐私措施

- 数据只在本机，核心不联网；M6 通过依赖审计确认没有网络请求代码。
- 标题保留期与历史清理使用 03 第 6 节的主库、WAL、备份规则。
- 设置中的“不记录任何标题”同时丢弃窗口标题与网页标签标题。
- 数据库依靠用户文件权限保护。数据库加密列为以后选项。

## 7. 测试

- 样本库覆盖 Edge、Chrome、Firefox 的普通与无痕窗口、不同语言和版本；M1 验证用户实际使用的浏览器。
- 扩展未连接、关联前与关联失效：数据库、日志、IPC 与托盘均不出现被丢弃标题。
- 字段动作组合：title_hidden 保持允许的网页信息，private_window 无网页行，domain_only 无两种标题，excluded 无应用身份日志。
- 策略更新与限时状态到期切段；重启恢复截止时间，当前 state 与托盘立即变化。
- SQL 可见性：前台 title_id、网页 tab_title_id 均已清除，无引用字典已删；历史标签在规则删除后仍可显示。
- 物理文件检查：执行 secure_delete 清理与 wal_checkpoint(TRUNCATE) 后检查主库、WAL、已选择删除的备份；记录仍保留的手动备份范围。
- 扩展断线、重连与核心重启后，数据库中不出现关联前或被规则丢弃的标题与域名。

## 8. 以后

`builtin.browser_incognito_flag` 留待允许扩展进入无痕模式的独立工作包；届时再定义输入、权限与验收。
