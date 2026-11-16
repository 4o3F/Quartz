# Quartz 规则格式（quartz-rules/1）

本文是 Quartz 规则文本的完整规范。它有两个读者：

- **用户**：了解规则能表达什么、怎样组织。
- **AI 助手**：用户可以把本文整份交给任意 AI，再描述需求，让 AI 写出规则文件；然后把结果粘贴到 Quartz 的“导入规则”，或用 `quartz rules check` 检查。

Quartz 本身不包含任何 AI 功能，也不会把数据发给任何服务。发给 AI 的内容完全由用户决定。

> **给 AI 的要求**
>
> 1. 每个规则集输出一个 `toml` 代码块，内容是完整的规则文件，第一行是 `format = "quartz-rules/1"`，然后是 `[set]`。需要多个规则集时输出多个代码块，并说明它们的父子关系。
> 2. 只使用本文列出的规则类型、字段、操作符和参数。不要发明新字段，也不要输出代码、脚本或正则以外的表达式。
> 3. 每条规则都写 `label`，用一句话说明它的意图。
> 4. 用户想修改内置或已有规则时，不要复制整套规则，而是新建一个以它为 `parent` 的规则集，用 `[[override]]` 或 `[[disable]]` 只写改动。
> 5. 不确定用户想要多宽的范围时，选择**更窄**的写法，并在代码块之后用一两句话说明还可以放宽到什么程度。
> 6. 写判例（`precedent`）时尤其要窄：判例一旦保存，在这条链的整个生命周期内永久有效，不能撤销。

## 1. 规则集

规则以**规则集**为单位组织。一个文件就是一个规则集，一个规则集只包含一种规则。

```toml
format = "quartz-rules/1"

[set]
key = "user.study-sites"                 # 必填，全局唯一
kind = "classification"                  # 必填，见下表
name = "学习网站"                          # 必填，显示名称
parent = "builtin.classification"        # 可选，父规则集的 key
description = "把教程、课程类网站归为学习"     # 可选

[[rule]]                                 # 新增规则
id = "bili-tutorial"
# ...

[[override]]                             # 替换祖先规则集中的一条规则
target = "builtin.classification/video-sites"
# ...新的完整规则内容

[[disable]]                              # 停用祖先规则集中的一条规则
target = "builtin.classification/bilibili-entertainment"
reason = "B 站改由本规则集细分"
```

| `kind` | 规则类型 | 章节 |
|---|---|---|
| `classification` | 分类规则 | 3.1 |
| `extraction` | 文档提取规则 | 3.2 |
| `privacy` | 隐私规则 | 3.3 |
| `precedent` | 判例（M7 起） | 3.4 |
| `policy_check` | 国策自动检查（M7 起） | 3.5 |

- `key` 只能包含小写字母、数字、`.`、`-`、`_`。`builtin.` 前缀保留给 Quartz 内置规则集；用户规则集请用 `user.` 前缀。
- 规则的 `id` 在规则集内唯一，只能包含字母、数字、`-`、`_`。规则的全名是 `<规则集 key>/<规则 id>`，例如 `user.study-sites/bili-tutorial`。
- 再次导入同一 `key` 的文件，生成该规则集的新版本；旧版本保留，可以对比和回退。
- 文件编码 UTF-8，注释用 `#`。未知的键会报错，不会被忽略。
- 内置规则集另有几个只供内置使用的键（`detector`、`languages`、`verified`、`label_key`），出现在用户规则集中会报 `E-RULE-KEY`。

### 1.1 规则集树

规则集通过 `parent` 组成树。子规则集只写相对父级的**差异**：

| 条目 | 作用 | 限制 |
|---|---|---|
| `[[rule]]` | 新增一条规则 | — |
| `[[override]]` | 用新内容替换某条祖先规则 | `target` 必须位于本规则集的祖先链上；内容是完整的新规则，`id` 沿用目标 |
| `[[disable]]` | 停用某条祖先规则 | `target` 必须位于祖先链上 |

示例：内置分类规则集 `builtin.classification` 是根；用户新建 `user.my-tweaks`，以它为父，只停用一条、覆盖一条、新增两条。内置规则升级时，用户的差异原样保留。

```text
builtin.classification          （内置，随 Quartz 更新）
├── user.my-tweaks              （停用 1、覆盖 1、新增 2）
│   └── user.exam-month         （考试月临时调整，可整体停用）
└── user.study-sites            （新增 5）
```

规则：

- 父子规则集的 `kind` 必须相同。不允许成环。
- 停用一个规则集时，它的整个子树一起停用，因为子规则集的差异依赖父级。
- 两个互不为祖先的规则集，不能 `override` 或 `disable` 同一条规则。需要叠加修改时，让其中一个成为另一个的子规则集。
- `precedent` 规则集只能使用 `[[rule]]`，不能覆盖或停用，因为判例永久有效。
- 内置规则集不能直接编辑，只能通过子规则集修改。

### 1.2 生效规则的计算

Quartz 按以下步骤得到某一类规则最终生效的列表：

1. 取全部启用的规则集，按树的前序遍历排列：父级在子级之前，同级按用户设定的顺序。
2. 依次应用每个规则集的差异：`rule` 加入，`override` 替换目标，`disable` 移除目标。
3. 每条生效规则记住它的来源：定义它的规则集、版本，以及覆盖过它的规则集。

规则之间的先后（分类和提取规则取第一条命中的规则）：

1. `priority` 大者优先；
2. 同优先级时，所在规则集在树中**更深**者优先（更具体的覆盖更通用的）；
3. 仍相同时，按第 1 步的遍历顺序，再按规则在文件中的顺序。

隐私规则不分先后：所有命中的规则逐项取最严格的结果（3.3）。

## 2. 条件

除提取规则外，每条规则都用 `match` 和 `when` 描述“什么时候命中”。

```toml
match = "all"                      # all：全部条件成立；any：任一条件成立。默认 all
when = [
  { field = "app.exe_name", op = "equals", value = "code.exe" },
  { field = "title.raw", op = "contains", value = "quartz", case_sensitive = false },
  { field = "web.domain", op = "domain_suffix", value = "bilibili.com", negate = true },
]
```

| 键 | 必填 | 说明 |
|---|---|---|
| `field` | 是 | 字段名，见 2.1 |
| `op` | 是 | 操作符，见 2.2 |
| `value` | 是 | 字符串。最长 4,096 个 UTF-16 单元 |
| `case_sensitive` | 否 | 默认 `false` |
| `negate` | 否 | 默认 `false`。为 `true` 时取反：字段存在且不满足条件，或字段不存在，都算成立 |

`when` 至少一个条件，最多 32 个。不支持嵌套分组；需要“A 且（B 或 C）”时，拆成两条规则，或用 `regex` 表达 B 或 C。

### 2.1 字段

| 字段 | 含义 | 示例值 |
|---|---|---|
| `app.exe_name` | 可执行文件名，小写 | `code.exe`、`msedge.exe` |
| `app.identity_key` | 应用的唯一标识 | `exe:c:\program files\microsoft vs code\code.exe`、`aumid:Microsoft.WindowsTerminal_8wekyb3d8bbwe!App` |
| `app.display_name` | 应用显示名，随 Windows 语言可能不同，尽量不用 | `Visual Studio Code` |
| `window.class` | 窗口类名 | `Chrome_WidgetWin_1` |
| `title.raw` | 窗口标题原文 | `rule-format.md - Quartz - Visual Studio Code` |
| `title.normalized` | 提取规则规范化后的标题（去掉应用名后缀、未保存标记等） | `rule-format.md - Quartz` |
| `doc.document` | 提取出的文档名 | `rule-format.md` |
| `doc.project` | 提取出的项目名 | `Quartz` |
| `doc.profile` | 提取出的配置文件或浏览器 profile 后缀 | `工作` |
| `web.domain` | 当前网页域名，小写，不含端口 | `github.com` |
| `web.tab_title` | 当前网页标签标题 | `Pull requests · 4o3F/Quartz` |
| `browser.profile_label` | 用户为浏览器 profile 设置的名称 | `个人` |
| `category` | 片段所属分类的名称，内置分类也可写 `builtin:<键>` | `开发`、`builtin:entertainment` |
| `category.tendency` | 分类倾向 | `focus`、`neutral`、`distraction` |
| `activity` | 活动状态 | `active`、`idle`、`locked`、`suspended`、`no_foreground` |

不是每种规则都能使用全部字段：

| 规则类型 | 可用字段 | 原因 |
|---|---|---|
| `classification` | `app.*`、`window.class`、`title.*`、`doc.*`、`web.*`、`browser.profile_label` | 分类不能依赖分类本身 |
| `privacy` | `app.*`、`window.class`、`title.raw`、`web.domain`、`web.tab_title`、`browser.profile_label` | 隐私规则在入库前求值，只能用采集时就有的字段 |
| `precedent`、`policy_check` | 全部字段 | 基于已入库并已分类的片段 |

`web.*` 只在浏览器扩展已连接且关联到当前窗口时存在。字段不存在时，普通条件不成立，`negate = true` 的条件成立。

### 2.2 操作符

| 操作符 | 含义 |
|---|---|
| `equals` | 完全相等 |
| `contains` | 包含子串 |
| `starts_with` | 以此开头 |
| `ends_with` | 以此结尾 |
| `regex` | 正则匹配（Rust `regex` 语法：不支持前后断言和反向引用）。默认部分匹配，需要整串匹配时写 `^...$` |
| `domain_suffix` | 域名等于该值，或是它的子域名。`bilibili.com` 匹配 `bilibili.com` 与 `www.bilibili.com`，不匹配 `notbilibili.com`。只能用于 `web.domain` |

比较规则：

- `case_sensitive = false`（默认）时，全角与半角字母数字、大小写、兼容字符都视为相同，例如 `ＶＳ` 与 `vs` 相同。
- `regex` 不受 `case_sensitive` 影响，忽略大小写请在正则开头写 `(?i)`。
- 域名可以写中文等 Unicode 形式，Quartz 自动转换。
- `category`、`category.tendency`、`activity` 只能用 `equals`。

## 3. 规则类型

以下示例都只列出 `[[rule]]` 的内容；完整文件还需要 `format` 与 `[set]`。`[[override]]` 的内容与 `[[rule]]` 相同，只是多一个 `target`、没有 `id`。

### 3.1 分类规则 `classification`

决定一段活动属于哪个分类，取第一条命中的规则（顺序见 1.2）。

```toml
[[rule]]
id = "dev-vscode"
label = "VS Code 算开发"
category = "开发"                  # 分类名称，或 builtin:<键>；分类必须已存在
priority = 50                      # 0–1000，默认 50
match = "all"
when = [{ field = "app.exe_name", op = "equals", value = "code.exe" }]
```

内置分类的键：`builtin:development`（开发）、`builtin:learning`（学习）、`builtin:communication`（沟通）、`builtin:entertainment`（娱乐）、`builtin:system`（系统与工具）。未分类不需要规则。

### 3.2 文档提取规则 `extraction`

从窗口标题中提取文档、项目等信息，供分类规则和统计使用。

```toml
[[rule]]
id = "vscode-title"
label = "VS Code 标题：● 文件 - 项目 - Visual Studio Code"
app = [{ field = "app.exe_name", op = "equals", value = "code.exe" }]
pattern = '^(?P<dirty>● )?(?P<document>.+?) - (?P<project>.+?) - Visual Studio Code$'
priority = 50
```

- `app`：选择应用的条件，格式同 `when`，只能使用 `app.*` 字段。
- `pattern`：作用于 `title.raw` 的正则，用命名捕获组输出结果。可用的组名：`document`、`project`、`profile`、`dirty`（捕获到任意内容即视为有未保存修改）。
- 第一条应用条件和正则都匹配成功的规则生效（顺序见 1.2）；都不匹配时使用通用回退。

### 3.3 隐私规则 `privacy`

决定命中的窗口哪些信息不入库。效果不可逆：被丢弃的信息不会因为之后放宽规则而恢复。

```toml
[[rule]]
id = "bank"
label = "网银只记域名"
match = "all"
when = [{ field = "web.domain", op = "domain_suffix", value = "bank.example" }]
preset = "title_hidden"            # 与下面三项二选一
# app = "keep"                     # keep | drop
# window_title = "drop"            # keep | drop
# web = "domain_only"              # full | domain_only | none
```

| `preset` | 效果 |
|---|---|
| `title_hidden` | 记录应用，不记录窗口标题；网页信息按其他规则 |
| `private_window` | 记录应用，不记录标题和任何网页信息 |
| `excluded` | 完全排除：只记录“已排除”的时间段 |

多条隐私规则同时命中时，每一项取最严格的结果。停用内置隐私规则（例如无痕窗口识别）时，导入界面会单独警告。

### 3.4 判例 `precedent`（M7 起）

CTDP 专注任务中“判允许”的行为。命中判例的活动在这条链今后的所有任务中都不再被视为疑似违规。**判例保存后不能修改或删除。**

每条链有自己的判例规则集，`[set]` 中多写一个 `chain`：

```toml
format = "quartz-rules/1"

[set]
key = "user.precedents-main"
kind = "precedent"
name = "主链判例"
chain = "主链"                     # 链的名称

[[rule]]
id = "wechat-reply"
label = "回复微信消息，每次不超过 3 分钟"
match = "all"
when = [{ field = "app.exe_name", op = "equals", value = "weixin.exe" }]
max_continuous = "3m"              # 可选：连续超过这个时长仍算疑似违规
max_total_per_session = "10m"      # 可选：一次任务内累计超过这个时长仍算疑似违规
```

- 时长写法：`90s`、`3m`、`1h30m`。
- 判例规则集只能追加 `[[rule]]`。再次导入时，已有规则必须原样保留，否则报 `E-RULE-IMMUTABLE`。
- 写判例时，从最窄的范围开始：具体的文档或标题 → 具体的应用或域名 → 整个分类。只有用户明确说“这一类都可以”时才用 `category`。
- 离开电脑这类情况用 `activity`，例如 `{ field = "activity", op = "equals", value = "idle" }` 加 `max_continuous = "5m"` 表示“短暂离开 5 分钟以内允许”。

### 3.5 国策自动检查 `policy_check`（M7 起）

为 RSIP 国策提供基于电脑使用记录的自动检查。只能检查这台电脑上可观测的行为；手机、家务等国策请在 Quartz 中设为手动结算。

```toml
[[rule]]
id = "curfew"
label = "电子宵禁：23:30 后不用娱乐应用"
policy = "电子宵禁"                # 国策名称
kind = "window_max"
window = "23:30-07:00"             # 本地时间，可以跨午夜
max = "0m"
match = "any"
when = [{ field = "category", op = "equals", value = "builtin:entertainment" }]
```

| `kind` | 参数 | 满足条件 |
|---|---|---|
| `window_max` | `window`、`max`、`when` | 时间窗内命中条件的活跃时长不超过 `max` |
| `after_first_active` | `within`、`when` | 当天第一次活跃之后的 `within` 时长内，没有命中条件的活动 |
| `first_active_before` | `time` | 当天第一次活跃早于 `time` |
| `last_active_before` | `time` | 当天最后一次活跃早于 `time`（按逻辑日计算，可跨午夜，如 `00:30`） |

“当天”按 Quartz 设置的逻辑日起点计算。

## 4. 校验、预览与调试

### 4.1 命令

| 命令 | 作用 | 需要核心运行 |
|---|---|---|
| `quartz rules check <文件>...` | 校验一个或多个规则文件，包括它们与已安装规则集组成的树 | 否（树检查使用本地已安装规则集的只读副本） |
| `quartz rules preview <文件> --days 7` | 每条规则在过去几天会命中的片段数、时长和样例 | 是 |
| `quartz rules diff <文件> --days 7` | 与同一 `key` 当前版本对比：规则级差异，以及过去几天哪些片段的结果会改变 | 是 |
| `quartz rules explain --at "2026-11-16 14:05"` | 解释某一时刻的片段：每个规则集、每条规则、每个条件是否成立，最终采用了哪条规则，哪些规则被遮蔽 | 是 |
| `quartz rules tree [--kind classification]` | 显示已安装的规则集树、版本与启用状态 | 是 |

界面中有相同功能：导入时的校验、预览与 diff，片段详情中的“为什么是这个结果”，以及规则集树视图。

### 4.2 错误

错误格式固定，可以原样贴回给 AI 修改：

```text
error[E-RULE-FIELD]: user.study-sites: rule[0].when[1].field: 未知字段 "app.name"
  可用字段：app.exe_name, app.identity_key, app.display_name, ...
error[E-RULE-TARGET]: user.my-tweaks: override[0].target: "builtin.classification/video" 不在祖先规则集中
```

| 错误码 | 含义 |
|---|---|
| `E-RULE-FORMAT` | 缺少或不支持的 `format` 版本 |
| `E-RULE-KEY` | 未知的键或缺少必填键 |
| `E-RULE-SET` | `[set]` 无效：`key` 格式错误、使用了 `builtin.` 前缀、`kind` 未知、与父级 `kind` 不同 |
| `E-RULE-PARENT` | 父规则集不存在，或形成环 |
| `E-RULE-TARGET` | `override`/`disable` 的目标不存在，或不在祖先链上 |
| `E-RULE-CONFLICT` | 与另一个非祖先规则集覆盖或停用了同一条规则 |
| `E-RULE-IMMUTABLE` | 修改或删除了判例，或在判例规则集中使用 `override`/`disable` |
| `E-RULE-FIELD` | 未知字段，或该规则类型不允许使用此字段 |
| `E-RULE-OP` | 未知操作符，或操作符不适用于该字段 |
| `E-RULE-VALUE` | 值为空、超长、时长或时间格式错误、枚举值无效 |
| `E-RULE-REGEX` | 正则无法编译、超出大小限制或使用了不支持的语法 |
| `E-RULE-REF` | 引用的分类、链或国策不存在 |
| `E-RULE-DUP` | 规则集内 `id` 重复 |

判例和隐私规则必须在界面中看过预览才能保存。

### 4.3 内置规则更新

内置规则集随 Quartz 升级。你的子规则集不受影响，但可能出现两种情况，导入或升级后会在规则集树中标出：

- **目标已删除**：`override` 或 `disable` 指向的内置规则在新版本中不存在。这条差异暂时不生效，请删除它或改为 `[[rule]]`。
- **上游已变更**：被覆盖的内置规则在新版本中改了内容。Quartz 显示三方对比（你覆盖时的内置版本、新的内置版本、你的覆盖），由你决定保留覆盖还是删除它。

## 5. 示例

把下面的需求交给 AI 时，期望得到类似的结果。

**需求**：“在 B 站看教程算学习，但首页和推荐不算。”

```toml
format = "quartz-rules/1"

[set]
key = "user.study-sites"
kind = "classification"
name = "学习网站"
parent = "builtin.classification"

[[rule]]
id = "bili-tutorial"
label = "B 站视频页且标题含教程关键词算学习"
category = "builtin:learning"
priority = 60
match = "all"
when = [
  { field = "web.domain", op = "domain_suffix", value = "bilibili.com" },
  { field = "web.tab_title", op = "regex", value = "(?i)(教程|课程|讲解|tutorial|lecture)" },
]
```

**需求**：“内置规则把 Windows Terminal 算成系统工具，我想算开发；另外不要把微信算沟通。”

```toml
format = "quartz-rules/1"

[set]
key = "user.my-tweaks"
kind = "classification"
name = "我的调整"
parent = "builtin.classification"

[[override]]
target = "builtin.classification/windows-terminal"
label = "Windows Terminal 算开发"
category = "builtin:development"
priority = 50
match = "all"
when = [{ field = "app.identity_key", op = "starts_with", value = "aumid:Microsoft.WindowsTerminal_" }]

[[disable]]
target = "builtin.classification/wechat"
reason = "微信不计入沟通"
```

**需求**：“专注时查文档可以，比如 docs.rs 和 MDN。”

```toml
format = "quartz-rules/1"

[set]
key = "user.precedents-main"
kind = "precedent"
name = "主链判例"
chain = "主链"

[[rule]]
id = "docs-sites"
label = "查阅 docs.rs 与 MDN 文档"
match = "any"
when = [
  { field = "web.domain", op = "domain_suffix", value = "docs.rs" },
  { field = "web.domain", op = "domain_suffix", value = "developer.mozilla.org" },
]
```

**反例**：把上一条写成 `{ field = "app.exe_name", op = "equals", value = "msedge.exe" }`。这会永久放行整个浏览器，范围远超“查文档”。

## 6. 版本

- 本文对应 `format = "quartz-rules/1"`。规则格式只增加可选内容时不改版本；字段改名、语义变化或删除时升级为 `/2`，Quartz 继续接受旧版本并在导入时转换。
- 字段表、操作符表和错误码表由 Quartz 源码中的类型生成或校验，与程序保持一致。
- 内置规则集的 `key` 与规则 `id`（例如 `builtin.classification/windows-terminal`）是公开接口，只在大版本中改名；`quartz rules tree` 与 `quartz rules explain` 可以查到它们。
