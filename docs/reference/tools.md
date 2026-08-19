# 内置工具参考

Echo Agent 提供 30 个内置工具，按风险等级分为四个类别。

## 风险等级概览

| 等级 | 类别 | 说明 | 默认审批模式 |
|------|------|------|-------------|
| 1 | MINIMAL_TOOLS | 只读操作，无副作用 | `auto` |
| 2 | MESSAGING_TOOLS | 消息发送与媒体操作 | `auto` |
| 3 | CODING_TOOLS | 文件写入与代码修改 | `ask` |
| 4 | HIGH_RISK_TOOLS | 系统执行与安装 | `ask` |

---

## 工具详细列表

### MINIMAL_TOOLS（只读）

#### browser

浏览网页并提取内容。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `url` | string | 是 | 目标 URL |
| `selector` | string | 否 | CSS 选择器（过滤内容） |
| `format` | string | 否 | 输出格式：text / markdown / html |
| `timeout` | int | 否 | 超时（秒），默认 30 |

```json
{
  "tool": "browser",
  "params": {
    "url": "https://example.com",
    "format": "markdown"
  }
}
```

#### clarify

向用户请求澄清信息。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `question` | string | 是 | 需要澄清的问题 |
| `options` | list | 否 | 提供选项列表 |

#### knowledge

查询知识库。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `query` | string | 是 | 查询文本 |
| `top_k` | int | 否 | 返回结果数，默认 5 |
| `filter` | object | 否 | 元数据过滤条件 |

#### memory

读取/写入长期记忆。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `action` | string | 是 | read / write / search / delete |
| `key` | string | 否 | 记忆键名（read/write/delete 时） |
| `value` | string | 否 | 记忆内容（write 时） |
| `query` | string | 否 | 搜索查询（search 时） |

#### read_spill

读取溢出存储中的大文件分片。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `spill_id` | string | 是 | 溢出文件 ID |
| `offset` | int | 否 | 读取偏移 |
| `limit` | int | 否 | 读取长度 |

#### search

搜索引擎查询。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `query` | string | 是 | 搜索关键词 |
| `engine` | string | 否 | 搜索引擎：google / bing / duckduckgo |
| `count` | int | 否 | 返回结果数，默认 5 |

#### session_search

搜索历史会话记录。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `query` | string | 是 | 搜索内容 |
| `session_id` | string | 否 | 限定会话 ID |
| `time_range` | object | 否 | 时间范围过滤 |

#### skills

列出可用技能。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `category` | string | 否 | 按分类过滤 |
| `query` | string | 否 | 按名称/描述搜索 |

#### todo

查看和管理待办事项。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `action` | string | 是 | list / add / complete / remove |
| `text` | string | 否 | 待办内容（add 时） |
| `id` | string | 否 | 待办 ID（complete/remove 时） |

#### vision

分析图片内容。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `image` | string | 是 | 图片路径或 URL |
| `prompt` | string | 否 | 分析提示 |

#### web

发送 HTTP 请求。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `url` | string | 是 | 请求 URL |
| `method` | string | 否 | HTTP 方法，默认 GET |
| `headers` | object | 否 | 请求头 |
| `body` | string | 否 | 请求体 |

---

### MESSAGING_TOOLS（消息与媒体）

#### message

通过通道发送消息。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `channel` | string | 是 | 目标通道标识 |
| `content` | string | 是 | 消息内容 |
| `reply_to` | string | 否 | 回复的消息 ID |
| `format` | string | 否 | 格式：text / markdown |

#### notify

发送通知（支持多通道）。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `title` | string | 是 | 通知标题 |
| `body` | string | 是 | 通知内容 |
| `level` | string | 否 | 级别：info / warning / error |
| `targets` | list | 否 | 通知目标列表 |

#### send_file

通过通道发送文件。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `channel` | string | 是 | 目标通道 |
| `path` | string | 是 | 本地文件路径 |
| `caption` | string | 否 | 文件说明 |

#### image_gen

生成图片（默认后端）。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `prompt` | string | 是 | 图片描述提示词 |
| `size` | string | 否 | 尺寸：1024x1024 / 1792x1024 / 1024x1792 |
| `style` | string | 否 | 风格：natural / vivid |

#### image_gen_fal

使用 Fal.ai 后端生成图片。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `prompt` | string | 是 | 图片描述 |
| `model` | string | 否 | Fal 模型标识 |
| `size` | string | 否 | 输出尺寸 |

#### tts

文本转语音。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `text` | string | 是 | 要转换的文本 |
| `voice` | string | 否 | 语音模型 |
| `output` | string | 否 | 输出文件路径 |

---

### CODING_TOOLS（代码与文件写入）

#### code_exec

在沙箱中执行代码片段。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `code` | string | 是 | 代码内容 |
| `language` | string | 否 | 语言：python / javascript / bash |
| `timeout` | int | 否 | 超时（秒），默认 30 |

#### document

创建或编辑文档。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `action` | string | 是 | create / edit / append |
| `path` | string | 是 | 文件路径 |
| `content` | string | 是 | 内容 |

#### filesystem

文件系统操作。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `action` | string | 是 | read / write / list / mkdir / delete / move / copy |
| `path` | string | 是 | 目标路径 |
| `content` | string | 否 | 文件内容（write 时） |
| `dest` | string | 否 | 目标路径（move/copy 时） |

#### patch

对文件应用差异补丁。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `path` | string | 是 | 目标文件路径 |
| `diff` | string | 是 | unified diff 格式的补丁内容 |

#### delegate

将子任务委派给其他 Agent。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `task` | string | 是 | 任务描述 |
| `agent_type` | string | 否 | Agent 类型 |
| `context` | object | 否 | 传递的上下文 |

#### task

管理任务分解与追踪。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `action` | string | 是 | create / update / complete / list |
| `title` | string | 否 | 任务标题 |
| `status` | string | 否 | 状态更新 |

#### workflow

定义和执行多步骤工作流。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `action` | string | 是 | define / run / status / cancel |
| `name` | string | 否 | 工作流名称 |
| `steps` | list | 否 | 步骤定义（define 时） |

#### skill_run

执行已安装的技能。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `skill_id` | string | 是 | 技能标识 |
| `input` | object | 否 | 技能输入参数 |

---

### HIGH_RISK_TOOLS（高风险）

!!! danger "高风险工具"
    以下工具可执行系统命令、修改进程或安装外部代码。仅在 `tools.profile: full` 或明确启用时可用。

#### shell

执行 Shell 命令。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `command` | string | 是 | 要执行的命令 |
| `cwd` | string | 否 | 工作目录 |
| `timeout` | int | 否 | 超时（秒） |
| `env` | object | 否 | 附加环境变量 |

#### process

管理系统进程。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `action` | string | 是 | list / start / stop / signal |
| `pid` | int | 否 | 进程 ID（stop/signal 时） |
| `command` | string | 否 | 启动命令（start 时） |
| `signal` | string | 否 | 信号名称（signal 时） |

#### cronjob

创建/管理定时任务。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `action` | string | 是 | create / delete / enable / disable / list |
| `schedule` | string | 否 | cron 表达式（create 时） |
| `command` | string | 否 | 执行内容（create 时） |
| `id` | string | 否 | 任务 ID（其他操作时） |

#### skill_install

安装外部技能包。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `source` | string | 是 | 技能来源（URL/路径/registry 名称） |
| `verify` | bool | 否 | 是否验证签名，默认 true |
| `sandbox` | bool | 否 | 是否沙箱安装，默认 true |

---

## 审批模式

每个工具的审批行为可通过配置覆盖：

| 模式 | 行为 |
|------|------|
| `auto` | 自动执行，不需用户确认 |
| `ask` | 执行前请求用户审批 |
| `deny` | 禁止使用该工具 |

配置示例：

```yaml
tools:
  profile: coding
  overrides:
    shell:
      approval_mode: ask      # Shell 命令需要审批
    filesystem:
      approval_mode: auto     # 文件操作自动执行
    process:
      approval_mode: deny     # 禁用进程管理
```

---

## 工具与 Profile 对应关系

| 工具 | minimal | messaging | coding | full |
|------|---------|-----------|--------|------|
| browser | ✓ | ✓ | ✓ | ✓ |
| clarify | ✓ | ✓ | ✓ | ✓ |
| knowledge | ✓ | ✓ | ✓ | ✓ |
| memory | ✓ | ✓ | ✓ | ✓ |
| read_spill | ✓ | ✓ | ✓ | ✓ |
| search | ✓ | ✓ | ✓ | ✓ |
| session_search | ✓ | ✓ | ✓ | ✓ |
| skills | ✓ | ✓ | ✓ | ✓ |
| todo | ✓ | ✓ | ✓ | ✓ |
| vision | ✓ | ✓ | ✓ | ✓ |
| web | ✓ | ✓ | ✓ | ✓ |
| message | — | ✓ | ✓ | ✓ |
| notify | — | ✓ | ✓ | ✓ |
| send_file | — | ✓ | ✓ | ✓ |
| image_gen | — | ✓ | ✓ | ✓ |
| image_gen_fal | — | ✓ | ✓ | ✓ |
| tts | — | ✓ | ✓ | ✓ |
| code_exec | — | — | ✓ | ✓ |
| document | — | — | ✓ | ✓ |
| filesystem | — | — | ✓ | ✓ |
| patch | — | — | ✓ | ✓ |
| delegate | — | — | ✓ | ✓ |
| task | — | — | ✓ | ✓ |
| workflow | — | — | ✓ | ✓ |
| skill_run | — | — | ✓ | ✓ |
| shell | — | — | — | ✓ |
| process | — | — | — | ✓ |
| cronjob | — | — | — | ✓ |
| skill_install | — | — | — | ✓ |

!!! question "需维护者确认"
    `delegate`、`task`、`workflow` 的风险分类是否正确？它们涉及创建子 Agent，是否应归为 HIGH_RISK_TOOLS？
