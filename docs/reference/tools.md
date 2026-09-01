# 内置工具参考

Echo Agent 内置 41 个工具。本页的工具名、参数、能力标签均取自代码中的注册信息：工具名是各工具类的 `name` 属性（定义在 `echo_agent/agent/tools/` 下），能力标签取自 `echo_agent/security/capabilities.py` 的 `TOOL_CAPABILITIES`，档位归属取自 `echo_agent/security/tool_policy.py`。

!!! note "工具名不等于模块名"
    工具名与实现它的模块文件名往往不同，调用时必须使用工具名。例如 `shell.py` 注册的工具是 `exec`，`code_exec.py` 注册的是 `execute_code`，`tts.py` 注册的是 `text_to_speech`，`web.py` 同时注册 `web_fetch` 与 `web_search`。

## 两个 profile 的区别

配置中有两个名为 `profile` 的字段，作用不同，不可混用：

| 配置项 | 取值 | 默认值 | 作用 |
|--------|------|--------|------|
| `tools.profile` | `minimal` / `messaging` / `coding` / `full` | `full` | 决定哪些工具暴露给模型 |
| `security.profile` | `personal_cli` / `daemon` / `public_gateway` | `personal_cli` | 决定运行形态的整体安全基线 |

`tools.profile` 的四档是工具白名单，逐档累加；`full` 的白名单是 `*`，即放通全部工具。详见[安全档位矩阵](security-profile-matrix.md)。

## 按档位可用的工具

下表列出每个工具在哪些 `tools.profile` 档位下可用。`full` 档放通全部工具，故不单列。

| 工具 | 实现模块 | minimal | messaging | coding | 能力标签 |
|------|----------|:-------:|:---------:|:------:|----------|
| `agents_list` | 无实现（见下） | ✅ | ✅ | ✅ | `agent.read` |
| `agents_route` | 无实现（见下） | ✅ | ✅ | ✅ | `agent.dispatch` |
| `artifact_create` | `artifact.py` | ✅ | ✅ | ✅ | `artifact.write` |
| `artifact_append` | `artifact.py` | ✅ | ✅ | ✅ | `artifact.write` |
| `artifact_validate` | `artifact.py` | ✅ | ✅ | ✅ | `artifact.read` |
| `artifact_finalize` | `artifact.py` | ✅ | ✅ | ✅ | `artifact.write` |
| `artifact_deliver` | `artifact.py` | ✅ | ✅ | ✅ | `artifact.read` `message.send` |
| `clarify` | `clarify.py` | ✅ | ✅ | ✅ | `message.ask` |
| `knowledge_search` | `knowledge.py` | ✅ | ✅ | ✅ | `knowledge.read` |
| `list_dir` | `filesystem.py` | ✅ | ✅ | ✅ | `fs.read` |
| `message` | `message.py` | ✅ | ✅ | ✅ | `message.send` |
| `notify` | `notify.py` | ✅ | ✅ | ✅ | `message.send` |
| `read_file` | `filesystem.py` | ✅ | ✅ | ✅ | `fs.read` |
| `read_spill` | `read_spill.py` | ✅ | ✅ | ✅ | `fs.read` |
| `search_files` | `search.py` | ✅ | ✅ | ✅ | `fs.read` |
| `session_search` | `session_search.py` | ✅ | ✅ | ✅ | `session.read` |
| `skill_view` | `skills.py` | ✅ | ✅ | ✅ | `skill.read` |
| `skills_list` | `skills.py` | ✅ | ✅ | ✅ | `skill.read` |
| `todo` | `todo.py` | ✅ | ✅ | ✅ | `task.write` |
| `image_generate` | `image_gen_fal.py` | ❌ | ✅ | ✅ | `media.generate` `network.outbound` |
| `memory` | `memory.py` | ❌ | ✅ | ✅ | `memory.read` `memory.write` |
| `text_to_speech` | `tts.py` | ❌ | ✅ | ✅ | `media.generate` `network.outbound` |
| `vision_analyze` | `vision.py` | ❌ | ✅ | ✅ | `media.read` |
| `edit_file` | `filesystem.py` | ❌ | ❌ | ✅ | `fs.read` `fs.write` |
| `knowledge_index` | `knowledge.py` | ❌ | ❌ | ✅ | `knowledge.write` `fs.read` |
| `patch` | `patch.py` | ❌ | ❌ | ✅ | `fs.read` `fs.write` |
| `task` | `task.py` | ❌ | ❌ | ✅ | `task.write` |
| `workflow` | `workflow.py` | ❌ | ❌ | ✅ | `workflow.write` |
| `write_file` | `filesystem.py` | ❌ | ❌ | ✅ | `fs.write` |
| `browser` | `browser.py` | ❌ | ❌ | ❌ | 未分类 |
| `cronjob` | `cronjob.py` | ❌ | ❌ | ❌ | `scheduler.write` |
| `delegate_task` | `delegate.py` | ❌ | ❌ | ❌ | 未分类 |
| `exec` | `shell.py` | ❌ | ❌ | ❌ | `process.exec` |
| `execute_code` | `code_exec.py` | ❌ | ❌ | ❌ | `code.exec` `process.exec` |
| `process` | `process.py` | ❌ | ❌ | ❌ | `process.exec` `process.manage` |
| `read_document` | `document.py` | ❌ | ❌ | ❌ | 未分类 |
| `send_file` | `send_file.py` | ❌ | ❌ | ❌ | `fs.read` `message.send` |
| `skill_install` | `skill_install.py` | ❌ | ❌ | ❌ | `skill.install` `network.outbound` `fs.write` |
| `skill_manage` | `skills.py` | ❌ | ❌ | ❌ | `skill.write` `fs.write` |
| `skill_run` | `skill_run.py` | ❌ | ❌ | ❌ | 未分类 |
| `spawn_task` | `delegate.py` | ❌ | ❌ | ❌ | 未分类 |
| `web_fetch` | `web.py` | ❌ | ❌ | ❌ | `network.outbound` |
| `web_search` | `web.py` | ❌ | ❌ | ❌ | `network.outbound` |

标记为「未分类」的工具在 `TOOL_CAPABILITIES` 中没有条目，`tool_capabilities()` 对它们返回空集合，因此基于能力的拦截规则不会命中它们；这类工具只受工具名维度的策略约束。

`agents_list` 与 `agents_route` 只出现在策略表（`capabilities.py`、`tool_policy.py`、`risk_classifier.py`）中，`echo_agent/agent/tools/` 下没有对应实现，因此当前不可调用。它们是为多 Agent 协作预留的名字，本页不为其提供参数说明。除这两项外，下文为全部 41 个已实现的工具逐一列出参数。

## 高风险工具

`HIGH_RISK_TOOLS` 是独立于档位的高风险集合，共 6 个：`cronjob`、`exec`、`execute_code`、`process`、`skill_install`、`skill_manage`。

两个运行形态在此基础上追加限制：

- **`public_gateway`** — 在高风险 6 个之外，另行拒绝 `edit_file`、`knowledge_index`、`patch`、`workflow`、`write_file`，共 11 个；同时按能力拒绝 `code.exec`、`fs.write`、`process.exec`、`process.manage`、`scheduler.write`、`skill.install`、`skill.write`、`workflow.write`。
- **`daemon`** — 默认拒绝 `exec`、`execute_code`、`process`、`skill_install`；同时按能力拒绝 `code.exec`、`process.exec`、`process.manage`、`skill.install`。

`artifact_*` 使用独立的会话产物命名空间，不接受任意文件路径，也不具备 `fs.write` 或 `process.exec` 能力，因此在 `public_gateway` 下仍默认可用。公网文档任务应使用该链路，不应放开通用 `write_file` 或 `exec`。

## 工具详细列表

参数列中标记 `*` 的为必填。类型与默认值取自各工具的 `parameters` JSON Schema。

### 文件与检索

#### read_file

读取文件内容。

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|:----:|--------|------|
| `path` | string | ✅ | — | 文件路径 |
| `offset` | integer | | — | 起始行 |
| `limit` | integer | | — | 读取行数 |

#### write_file

写入文件，文件不存在时创建。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `path` | string | ✅ | 文件路径 |
| `content` | string | ✅ | 写入内容 |

#### edit_file

替换文件中的字符串。

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|:----:|--------|------|
| `path` | string | ✅ | — | 文件路径 |
| `old_string` | string | ✅ | — | 被替换的原字符串 |
| `new_string` | string | ✅ | — | 替换后的字符串 |
| `replace_all` | boolean | | `false` | 是否替换全部匹配 |

#### patch

以统一 diff 格式或搜索替换块的形式应用补丁。

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|:----:|--------|------|
| `file_path` | string | ✅ | — | 目标文件路径 |
| `patch` | string | ✅ | — | 补丁内容 |
| `fuzzy_threshold` | number | | `0.6` | 模糊匹配阈值 |

#### list_dir

列出指定路径下的文件与子目录。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `path` | string | ✅ | 目录路径 |

#### search_files

按正则搜索文件内容，或按 glob 查找文件。

| 参数 | 类型 | 必填 | 默认值 | 取值 | 说明 |
|------|------|:----:|--------|------|------|
| `pattern` | string | ✅ | — | — | 搜索模式 |
| `mode` | string | | — | `content` \| `glob` | 按内容搜索或按文件名匹配 |
| `path` | string | | — | — | 搜索范围 |
| `max_results` | integer | | `50` | — | 最大结果数 |

#### read_document

读取文档文件的文本内容，支持 pdf、docx、xlsx、pptx、txt、csv、md。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `path` | string | ✅ | 文档路径 |
| `max_chars` | integer | | 最大读取字符数 |
| `unit` | integer \| string | | 读取单位 |

#### read_spill

读取被溢写到磁盘的工具输出产物，路径取自工具结果中的提示。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `path` | string | ✅ | 溢写产物路径 |
| `offset` | integer | | 起始位置 |
| `limit` | integer | | 读取长度 |
| `pattern` | string | | 过滤模式 |

### 用户产物

长报告使用固定流程：`artifact_create` → 多次 `artifact_append` → `artifact_validate` → `artifact_finalize` → `artifact_deliver`。产物按会话隔离，模型只接触不透明的 `artifact_id`，不会获得服务器文件路径。

#### artifact_create

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `filename` | string | ✅ | 文件名；扩展名由 `artifacts.allowed_extensions` 限制 |
| `title` | string | | 可选标题 |

#### artifact_append

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `artifact_id` | string | ✅ | `artifact_create` 返回的 ID |
| `sequence` | integer | ✅ | 从 0 开始的连续分块编号 |
| `content` | string | ✅ | UTF-8 文本分块 |
| `expected_bytes` | integer | | 乐观并发检查的预期字节偏移 |

每个 assistant 回合只调用一次 `artifact_append`，收到结果后再生成下一块，避免多个工具参数合并后再次撞上单次输出上限。相同 `sequence` 和相同内容的重试是幂等的；不同内容或乱序写入会被拒绝。

#### artifact_validate

只需要 `artifact_id`。验证器不依赖 shell，可计算字符、中文字、英文词、行、段落和标题，并检查 Markdown、JSON、CSV 格式。

#### artifact_finalize

只需要 `artifact_id`。校验通过后原子定稿，之后禁止追加。

#### artifact_deliver

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `artifact_id` | string | ✅ | 已定稿产物 ID |
| `caption` | string | | 简短说明 |
| `fallback_to_text` | boolean | | 通道不支持附件时是否改用编号文本分段，默认 true |

工具只能投递到当前会话。支持附件的通道上传文件；不支持附件的通道在配置上限内自动发送编号文本分段，并如实返回交付模式。

### 知识与记忆

#### knowledge_search

检索本地知识库，返回带引用的片段。

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|:----:|--------|------|
| `query` | string | ✅ | — | 查询文本 |
| `max_results` | integer | | `5` | 返回结果数 |

#### knowledge_index

查看或重建本地知识索引。

| 参数 | 类型 | 必填 | 取值 | 说明 |
|------|------|:----:|------|------|
| `action` | string | ✅ | `status` \| `rebuild` | 查看状态或重建索引 |

#### memory

管理跨会话的持久记忆。

| 参数 | 类型 | 必填 | 取值 | 说明 |
|------|------|:----:|------|------|
| `action` | string | ✅ | `add` \| `replace` \| `remove` \| `search` \| `list` \| `list_contradictions` \| `resolve_contradiction` | 操作类型 |
| `content` | string | | — | 记忆内容 |
| `key` | string | | — | 记忆键 |
| `old_text` | string | | — | 被替换的原文本 |
| `query` | string | | — | 检索关键词 |
| `tags` | string | | — | 标签 |
| `importance` | number | | — | 重要度 |
| `pinned` | boolean | | — | 是否置顶 |
| `source` | string | | `user_stated` \| `model_inferred` | 记忆来源 |
| `target` | string | | `user` \| `environment` | 记忆归属 |
| `contradiction_id` | string | | — | 矛盾记录 ID |
| `winner_id` | string | | — | 解决矛盾时保留的一方 |

#### session_search

按关键词或正则检索跨会话的历史消息。

| 参数 | 类型 | 必填 | 默认值 | 取值 | 说明 |
|------|------|:----:|--------|------|------|
| `query` | string | ✅ | — | — | 查询文本 |
| `max_results` | integer | | `20` | — | 最大结果数 |
| `role_filter` | string | | — | `user` \| `assistant` \| `all` | 按角色过滤 |
| `session_key` | string | | — | — | 限定会话 |

### 消息与媒体

#### message

向指定通道与会话发送消息。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `channel` | string | ✅ | 目标通道注册名 |
| `chat_id` | string | ✅ | 目标会话 ID |
| `text` | string | ✅ | 消息正文 |

#### notify

向指定通道或当前会话发送通知消息。与 `message` 的区别是通道与会话均可省略，省略时投递到当前会话。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `message` | string | ✅ | 通知正文 |
| `channel` | string | | 目标通道 |
| `chat_id` | string | | 目标会话 ID |

#### send_file

向指定通道与会话发送本地文件或图片。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `channel` | string | ✅ | 目标通道注册名 |
| `chat_id` | string | ✅ | 目标会话 ID |
| `file_path` | string | ✅ | 本地文件路径 |
| `caption` | string | | 附带说明文字 |
| `as_image` | boolean | | 是否以图片形式发送 |

调用前应确认目标通道的 `supports_files` 为真，否则文件无法送达。目前只有 `weixin` 恒定支持，`qqbot` 取决于 `media_enabled` 配置。详见[消息通道](../integrations/channels/index.md)。

#### clarify

向用户提问以澄清需求，可附带选项。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `question` | string | ✅ | 需要澄清的问题 |
| `options` | array | | 供用户选择的选项列表 |

#### image_generate

根据文本提示生成图像。

| 参数 | 类型 | 必填 | 取值 | 说明 |
|------|------|:----:|------|------|
| `prompt` | string | ✅ | — | 图像描述 |
| `aspect_ratio` | string | | `landscape` \| `square` \| `portrait` | 画面比例 |

`image_gen.py` 与 `image_gen_fal.py` 注册的工具名相同，实际启用哪一个取决于配置。

#### text_to_speech

将文本转为语音音频。默认使用 edge-tts，配置后可改用 OpenAI TTS。

| 参数 | 类型 | 必填 | 取值 | 说明 |
|------|------|:----:|------|------|
| `text` | string | ✅ | — | 待合成文本 |
| `backend` | string | | `edge` \| `openai` | 合成后端 |
| `voice` | string | | — | 音色 |
| `output_path` | string | | — | 输出文件路径 |
| `deliver` | boolean | | — | 是否直接投递 |
| `deliver_channel` | string | | — | 投递目标通道 |
| `deliver_chat_id` | string | | — | 投递目标会话 |
| `caption` | string | | — | 附带说明文字 |

#### vision_analyze

调用具备视觉能力的模型分析图像，输入可为本地路径或 URL。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `image` | string | ✅ | 图片路径或 URL |
| `prompt` | string | ✅ | 针对图片的问题 |
| `model` | string | | 指定模型 |

### 网络访问

#### web_search

检索互联网信息。

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|:----:|--------|------|
| `query` | string | ✅ | — | 查询文本 |
| `max_results` | integer | | `5` | 最大结果数 |

#### web_fetch

抓取指定 URL 的内容。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `url` | string | ✅ | 目标 URL |
| `max_chars` | integer | | 最大读取字符数 |

#### browser

驱动真实浏览器完成多步网页交互。会话通过 `session_id` 关联，元素通过快照中的 `ref` 定位。

| 参数 | 类型 | 必填 | 取值 | 说明 |
|------|------|:----:|------|------|
| `action` | string | ✅ | `open` `navigate` `snapshot` `click` `type` `press` `scroll` `back` `forward` `reload` `hover` `select` `upload` `wait` `evaluate` `console` `screenshot` `get_images` `close` | 操作类型 |
| `session_id` | string | | — | 浏览器会话 ID |
| `url` | string | | — | 目标 URL |
| `ref` | string | | — | 快照中的元素引用 |
| `text` | string | | — | 输入文本 |
| `key` | string | | — | 按键名 |
| `press_enter` | boolean | | — | 输入后是否回车 |
| `direction` | string | | `up` \| `down` \| `left` \| `right` \| `top` \| `bottom` | 滚动方向 |
| `amount` | integer | | — | 滚动距离 |
| `values` | array | | — | 下拉选择的值 |
| `paths` | array | | — | 上传的文件路径 |
| `expression` | string | | — | 待求值的表达式 |
| `state` | string | | `load` \| `domcontentloaded` \| `networkidle` | 等待的页面状态 |
| `full_page` | boolean | | — | 截图是否包含整页 |
| `timeout_sec` | integer | | — | 超时（秒） |

出站请求统一经 `echo_agent/security/net_guard.py` 的 SSRF 策略校验，无法解析的主机会被拒绝。

### 执行与进程

以下工具均属 `HIGH_RISK_TOOLS`。

#### exec

在工作区内执行 shell 命令。

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|:----:|--------|------|
| `command` | string | ✅ | — | 待执行命令 |
| `cwd` | string | | — | 工作目录 |
| `timeout` | integer | | `30` | 超时（秒） |

#### execute_code

在沙箱子进程中执行代码片段。

| 参数 | 类型 | 必填 | 默认值 | 取值 | 说明 |
|------|------|:----:|--------|------|------|
| `code` | string | ✅ | — | — | 代码内容 |
| `language` | string | ✅ | — | `python` \| `javascript` \| `bash` | 语言 |
| `timeout` | integer | | `30` | — | 超时（秒） |

#### process

管理后台进程。

| 参数 | 类型 | 必填 | 默认值 | 取值 | 说明 |
|------|------|:----:|--------|------|------|
| `action` | string | ✅ | — | `start` \| `list` \| `poll` \| `stop` | 操作类型 |
| `command` | string | | — | — | 启动命令 |
| `process_id` | string | | — | — | 进程 ID |
| `timeout` | integer | | `300` | — | 超时（秒） |

### 任务与编排

#### todo

管理待办清单，用于多步工作的规划。

| 参数 | 类型 | 必填 | 取值 | 说明 |
|------|------|:----:|------|------|
| `action` | string | ✅ | `create` \| `update` \| `list` \| `complete` \| `delete` | 操作类型 |
| `title` | string | | — | 标题 |
| `items` | array | | — | 条目列表 |
| `task_id` | string | | — | 条目 ID |
| `status` | string | | `pending` \| `in_progress` \| `done` | 状态 |
| `notes` | string | | — | 备注 |

#### task

带完整生命周期跟踪的任务管理。

| 参数 | 类型 | 必填 | 取值 | 说明 |
|------|------|:----:|------|------|
| `action` | string | ✅ | `create` \| `list` \| `get` \| `start` \| `complete` \| `fail` \| `cancel` \| `retry` \| `update` | 操作类型 |
| `title` | string | | — | 标题 |
| `description` | string | | — | 描述 |
| `task_id` | string | | — | 任务 ID |
| `priority` | integer | | — | 优先级 |
| `status_filter` | string | | — | 列表过滤条件 |
| `result` | string | | — | 完成结果 |
| `error` | string | | — | 失败原因 |
| `workflow_id` | string | | — | 关联的工作流 ID |

#### workflow

按 DAG 依赖编排多步工作流。该引擎只负责编排，不执行业务逻辑。

| 参数 | 类型 | 必填 | 取值 | 说明 |
|------|------|:----:|------|------|
| `action` | string | ✅ | `create` \| `start` \| `status` \| `advance` \| `pause` \| `resume` \| `cancel` \| `list` | 操作类型 |
| `name` | string | | — | 工作流名称 |
| `description` | string | | — | 描述 |
| `steps` | array | | — | 步骤定义 |
| `workflow_id` | string | | — | 工作流 ID |
| `status_filter` | string | | — | 列表过滤条件 |

#### cronjob

管理定时任务。属 `HIGH_RISK_TOOLS`。

| 参数 | 类型 | 必填 | 取值 | 说明 |
|------|------|:----:|------|------|
| `action` | string | ✅ | `create` \| `list` \| `delete` \| `trigger` \| `authorize` \| `revoke` | 操作类型 |
| `name` | string | | — | 任务名称 |
| `schedule` | string | | — | cron 表达式 |
| `command` | string | | — | 执行内容 |
| `job_id` | string | | — | 任务 ID（`delete`/`trigger`/`authorize`/`revoke` 必填） |
| `target_channel` | string | | — | 结果投递通道 |
| `target_chat_id` | string | | — | 结果投递会话 |

`authorize` 用于给**已存在**的任务补授权：任务内容被修改后授权会自动失效（见
[定时任务](../guides/scheduled-jobs.md)），此时需要重新授权。由于 `cronjob` 属
`HIGH_RISK_TOOLS`，`authorize` 一定会触发人工确认提示，提示中会列出该任务的指令、
频率与投递目标，确认后才签发授权。`revoke` 撤销授权，不需要额外确认。

#### delegate_task

将子任务委派给 worker agent，用于并行或隔离执行。

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|:----:|--------|------|
| `goal` | string | | — | 总体目标 |
| `tasks` | array | | — | 子任务列表 |
| `tools` | array | | — | 允许 worker 使用的工具 |
| `worker_profile` | string | | — | worker 的档位 |
| `max_iterations` | integer | | `12` | 最大迭代轮数 |

#### spawn_task

派生一个可使用工具的后台 worker，异步执行。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `task` | string | ✅ | 任务描述 |
| `context` | string | | 附加上下文 |

### 技能管理

#### skills_list

列出全部可用技能的精简元数据。该工具无参数。

#### skill_view

查看技能的完整内容（`SKILL.md`）或其中某个支持文件。省略 `file_path` 时返回 `SKILL.md`。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `name` | string | ✅ | 技能名 |
| `file_path` | string | | 技能内的文件路径 |

#### skill_run

以 Agent 自身的 Python 解释器运行技能脚本，工作目录锁定为该技能目录。

| 参数 | 类型 | 必填 | 说明 |
|------|------|:----:|------|
| `name` | string | ✅ | 技能名 |
| `script` | string | ✅ | 脚本路径 |
| `args` | array | | 命令行参数 |
| `timeout` | integer | | 超时（秒） |

#### skill_manage

创建、编辑、打补丁或删除技能。属 `HIGH_RISK_TOOLS`。

| 参数 | 类型 | 必填 | 取值 | 说明 |
|------|------|:----:|------|------|
| `action` | string | ✅ | `create` \| `edit` \| `patch` \| `delete` \| `write_file` \| `remove_file` | 操作类型 |
| `name` | string | ✅ | — | 技能名 |
| `category` | string | | — | 分类 |
| `content` | string | | — | 内容 |
| `file_path` | string | | — | 技能内的文件路径 |
| `old_text` | string | | — | 被替换的原文本 |
| `new_text` | string | | — | 替换后的文本 |

#### skill_install

从外部来源安装技能到本地技能库。属 `HIGH_RISK_TOOLS`。

| 参数 | 类型 | 必填 | 取值 | 说明 |
|------|------|:----:|------|------|
| `source` | string | ✅ | `git` \| `local` \| `url` | 来源类型 |
| `location` | string | ✅ | — | 来源地址 |
| `name` | string | | — | 安装后的技能名 |
| `subdirectory` | string | | — | 源中的子目录 |
| `run_install` | boolean | | — | 是否执行安装脚本 |

## 相关页面

- [安全档位矩阵](security-profile-matrix.md) — 档位与运行形态的完整对照
- [配置参考](configuration.md) — 由 schema 自动生成的逐项配置说明
- [消息通道](../integrations/channels/index.md) — 通道能力与投递限制
