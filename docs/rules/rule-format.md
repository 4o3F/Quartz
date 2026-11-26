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

- `key` 只能包含小写字母、数字、`.`、`-`、`_`。`builtin.` 前缀保留给 Quartz 内置规则集；用户规则集请用 `user.` 前缀。
- 规则的 `id` 在规则集内唯一，只能包含字母、数字、`-`、`_`。规则的全名是 `<规则集 key>/<规则 id>`，例如 `user.study-sites/bili-tutorial`。
- 规则集以 `key` 标识，规则的 `id` 只在该集内唯一。再次导入同一 key 生成新版本。wire 写入携带当前 `base_version`，创建不存在或已删除的 key 使用 0；保存版本为 u32，从 1 开始，删除后恢复沿用历史版本递增。
- 文件编码 UTF-8，注释用 `#`。未知的键会报错，不会被忽略。
- 内置规则集另有几个只供内置使用的键（`detector`、`languages`、`verified`、`label_key`），出现在用户规则集中会报 `E-RULE-KEY`。

### 1.1 规则集树

规则集通过 `parent` 组成树。子规则集只写相对父级的**差异**：

| 条目 | 作用 | 限制 |
|---|---|---|
| `[[rule]]` | 新增一条规则 | — |
| `[[override]]` | 用新内容替换某条祖先规则 | `target` 必须位于本规则集的祖先链上；内容是完整的新规则，`id` 沿用目标 |
| `[[disable]]` | 停用某条祖先规则 | `target` 必须位于祖先链上 |

同一规则集中，每个 target 最多出现一次，override 与 disable 合并检查；重复时在文本解析阶段返回 `E-RULE-CONFLICT`。

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
- 内置规则集不能直接编辑，只能通过子规则集修改。默认用户子集分别为 `user.local.classification`、`user.local.extraction`、`user.local.privacy`，以同类型 builtin 集为父；key 全局唯一。
- 同一 key 创建后 kind 与 parent 固定，删除后恢复也沿用原身份；换类型或父级时新建 key。编译时检查每条新增和覆盖规则的内容类型与 kind 一致。

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
| `classification` | `app.identity_key`、`app.exe_name`、`app.display_name`、`window.class`、`title.raw`、`title.normalized`、`doc.document`、`doc.project`、`doc.profile`；M5 加入 `web.*`、`browser.profile_label` | 使用当前查询输入，不能依赖分类本身 |
| `privacy` | `app.*`、`window.class`、`title.raw`、`web.domain`、`web.tab_title`、`browser.profile_label` | 隐私规则在入库前求值，只能用采集时就有的字段 |

M4 先提取再分类，窗口类名取自已保留的窗口实例；标题隐藏时 `title.raw`、`title.normalized` 与 `doc.*` 缺失。分类的 `web.*` 与 `browser.profile_label` 待 M5；尚未交付的字段在校验阶段返回 `E-RULE-FIELD`。

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
- `web.domain` 的 `equals` 和 `domain_suffix` 使用完整域名规范化：非过渡 UTS #46 转为小写 ASCII，Unicode 标签转为 Punycode，并去掉末尾的根点。`contains`、`starts_with`、`ends_with` 使用片段规范化：按域名分隔点逐段进行同样的 UTS #46 转换，保留首尾和连续分隔点，Unicode 分隔点统一为 `.`；比较对象为规范化后的 ASCII 域名。例如 `op = "starts_with", value = "docs."` 只匹配 `docs.` 开头，`ends_with` 的值 `.example.com` 只匹配带该后缀的子域名。片段中的非 ASCII 部分按标签编码后比较。规范化后为空的值（包括只含 UTS #46 忽略字符的片段）返回 `E-RULE-VALUE`。
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

分类引用也可使用分类 UUID，导入时将名称或内置键解析成 UUID 并保存。UUID 与 `builtin:` 引用从独立的标识目录解析；显示名称必须唯一，否则返回 `E-RULE-REF`，应改用 UUID 或内置键。创建或改名时，分类名称不能使用 UUID 格式或 `builtin:` 命名空间。已有数据库中的此类名称保留显示并允许改名，解析时始终按稳定标识处理；不存在的稳定标识返回引用错误。第一条匹配规则引用的分类在查询时已归档或被删除，结果为未分类，诊断保留规则 ID 和分类 UUID。

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
- 输出与文档身份见 [03 第 5.2 节](../design/03-data-model.md#52-文档身份提取)：身份为应用加 NFC 规范化的 document/project/profile，保留大小写和缺失/空串区别；dirty、规范化标题与提取规则来源不参与身份。document 为空或未知时不生成身份，客户端使用版本绑定的不透明引用。

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

## 4. 校验、预览与调试

### 4.1 命令

| 命令 | 作用 | 需要核心运行 |
|---|---|---|
| `quartz rules check <文件>...` | 校验一个或多个规则文件，包括它们与已安装规则集组成的树 | 否（树检查使用本地已安装规则集的只读副本） |

界面中有相同功能：通过 `rules.preview` 在 1–7 个逻辑日内预览草稿或导入文本的影响，以及规则集树视图。

导出通过 `rules_text.export` 协议操作取得；`rules_text.validate` 对指定 key 与基础版本作上下文校验。

规范导出使用 LF 换行，末尾保留一个换行，先写 `format` 和 `[set]`，随后按保存的跨节顺序写 `[[rule]]`、`[[override]]`、`[[disable]]`。表内键按字典序排列，条件使用内联表并保留条件数组顺序；字符串使用双引号及 TOML 转义。默认 priority、match、case_sensitive、negate 写出，隐私 preset 展开为 app/window_title/web，分类名称和内置键均写为保存的 UUID。可选空值省略；内置条目保留 detector、verified、label_key 和非空的 languages；languages 为空时省略，与未提供该字段等价。用户条目不增加这些字段。相同保存内容重复导出得到相同字节，导出后解析再导出也得到相同字节。内置 key 的只读验证权限与诊断返回方式见 [04 第 6.3.3 节](../design/04-ipc.md#633-文本)。

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
| `E-RULE-PARENT` | 父规则集 key 语法无效、父集不存在或已删除，或父链形成环 |
| `E-RULE-TARGET` | `override`/`disable` 的目标不存在，或不在祖先链上 |
| `E-RULE-CONFLICT` | 同一规则集重复修改一个 target，或与另一个非祖先规则集修改了同一条规则 |
| `E-RULE-FIELD` | 未知字段，或该规则类型不允许使用此字段 |
| `E-RULE-OP` | 未知操作符，或操作符不适用于该字段 |
| `E-RULE-VALUE` | 值为空、超长、时长或时间格式错误、枚举值无效 |
| `E-RULE-REGEX` | 正则无法编译、超出大小限制或使用了不支持的语法 |
| `E-RULE-REF` | 引用的分类、链或国策不存在，或分类显示名称有歧义 |
| `E-RULE-DUP` | 规则集内 `id` 重复 |

界面保存隐私规则前通过 `rules.preview` 显示预览并要求确认；核心不强制回执。

### 4.3 内置规则更新

内置规则集随 Quartz 升级。你的子规则集不受影响；如果 `override` 或 `disable` 指向的内置规则在新版本中不存在，会在规则集树中标出：

- **目标已删除**：`override` 或 `disable` 指向的内置规则在新版本中不存在。这条差异暂时不生效，请删除它或改为 `[[rule]]`。

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

**需求**：“内置规则把 Windows Terminal 算成开发，我想算系统与工具；另外不要把微信算沟通。”

```toml
format = "quartz-rules/1"

[set]
key = "user.my-tweaks"
kind = "classification"
name = "我的调整"
parent = "builtin.classification"

[[override]]
target = "builtin.classification/windows-terminal"
label = "Windows Terminal 算系统与工具"
category = "builtin:system"
priority = 50
match = "all"
when = [{ field = "app.identity_key", op = "starts_with", value = "aumid:Microsoft.WindowsTerminal_" }]

[[disable]]
target = "builtin.classification/wechat"
reason = "微信不计入沟通"
```

## 6. 版本

- 本文对应 `format = "quartz-rules/1"`。规则格式只增加可选内容时不改版本；字段改名、语义变化或删除时升级为 `/2`，Quartz 继续接受旧版本并在导入时转换。
- 字段表、操作符表和错误码表由 Quartz 源码中的类型生成或校验，与程序保持一致。
- 内置规则集的 `key` 与规则 `id`（例如 `builtin.classification/windows-terminal`）是公开接口，只在大版本中改名。
