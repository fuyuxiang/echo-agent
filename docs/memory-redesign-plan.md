# Echo-Agent 记忆系统重构方案

## 一、北极星(核心哲学)

> **可靠地记住关于你的事,优先于聪明地检索任何事。**
>
> echo-agent 是单人微信私人助理。它存在的意义是越来越懂你这个人。
> 因此「关于你的事」必须可靠常驻、永不丢失;其余一切(工作笔记、
> 语义检索、遗忘/巩固等机制)都是辅助,服从于这一点。

一切取舍由此推导:漏记用户的生日,比检索不够智能严重一百倍。

## 二、架构骨架:以「驻留性」为主轴

记忆按「是否常驻上下文」分两区,而非按内容类型平铺。

### Core 区(常驻 / 不衰减 / 有界 / 需策展)
- 关系内核。会话开头**无条件全量注入** system prompt,不进检索池、不赌相关性。
- 内部按主题细分(仅为可读):
  - **关于你**:身份、偏好、家人、长期目标(= MemoryType.USER)
  - **关于你的世界**:你认定为长期的项目/环境事实(= MemoryType.ENVIRONMENT 中被标记 durable 的)
- **遗忘曲线对 Core 区完全失效**。身份事实没有保质期。
- 有硬字符预算;溢出是「该策展整理」的信号,不是悄悄丢弃。

### Recall 区(按需检索)
- 其余一切。内部按类型分:
  - **情景记忆(带时间戳事件)**:遗忘曲线**只活在这里**,合理衰减。
  - **环境/语义事实(可能过时)**:温和衰减。
- 巩固/晋升通道:新信息先进 Recall(情景)→ 后台审查决定哪些够格「晋升」到 Core。
  这就是当前坏掉的 reviewer(海马体→皮层固化的对应物)。

### 自我认知 = 配置,不是记忆
- Agent 能力/限制(能否生图、有哪些工具)运行时从工具注册表派生,不再写入可变记忆。
- 消除 MEMORY.md 里跨版本自相矛盾的「self / skills / capabilities」段落。

## 三、根因诊断(已用远程数据 data-server 验证)

你「昨天说生日、今天问不出」不是单一 bug,是多层叠加。事实证据:

1. **字段名拼写 bug(头号根因,确定)**
   - `inference_stage.py:68` 读 `config.memory.nudge_interval`,但 schema 字段叫
     `memory_nudge_interval`(schema.py:369)。`hasattr` 恒为 False →
     `_memory_nudge_interval = 0` → **后台记忆审查被永久禁用**。
   - 验证:`echo_agent.db` 的 memories 表中 **USER 类型 0 条**,只有 1 条 environment。
     reviewer 从未成功保存过任何用户事实。

2. **`total_tool_calls > 0` 死结**
   - `response_stage.py:85`:`should_review_memory and total_tool_calls > 0`。
     纯聊天(无工具调用)永远不触发审查 —— 个人闲聊正是这种。

3. **阈值偏高**:`consolidation_threshold=50`、`memory_nudge_interval=15`,
   短对话攒不到。

4. **MEMORY.md 被测试噪音污染**:Recent Interaction Notes 全是
   「rm -rf 拒绝 25 次」「math 42 回答 24 次」等 eval 流量,淹没真实事实。

5. **自我认知混进可变记忆**:MEMORY.md 有 self/skills/capabilities 段落,
   且出现跨版本自相矛盾(「sunset.png NEVER generated, previous memory was FALSE」)。

6. **模型 confabulation**:你的出生信息白纸黑字在微信 session 第 216 条
   (343 条消息、status=active、24h 内未过期、`get_history(500)` 会注入),
   模型却回答「我是被动式、记不住」—— 它既没读眼前历史,也谎称自身机制。

7. **注入位已存在但未接线**:`build_system_prompt` 已预留 `user_profile` /
   `env_context` 参数(context.py:201-202),调用方从未传入。
   **Core 常驻区的注入插槽是现成的,只是没接通。**

结论:架构骨架其实齐备(USER/ENV/长期三层、snapshot、scope、巩固后台、
注入插槽),是 bug 把管线掐断 + 缺少统领哲学导致记的全是噪音。

## 四、实施步骤

按「先让它工作 → 再让它正确分层 → 最后清理历史」三阶段。每阶段可独立验证、独立回滚。

### 阶段 0:修 bug,让现有管线复活(低风险,纯逻辑)
- **0.1** 修字段名:`inference_stage.py:68` 改读 `config.memory.memory_nudge_interval`。
- **0.2** 拆 `total_tool_calls > 0` 死结(`response_stage.py:85`):
  纯聊天也应触发记忆审查。改为「有实质对话轮次」即可触发,不依赖工具调用。
- **0.3** 阈值下调(`config/schema.py`):`memory_nudge_interval` 15→10、
  `consolidation_threshold` 50→20(适配个人助理对话量)。
- **验证**:本地构造一段纯聊天(含「我生日是X」),跑 pipeline,断言
  memories 表新增 USER 条目。补单测。

### 阶段 1:Core 常驻区接线(中风险,改注入逻辑)
- **1.1** 接通注入插槽:`context_stage.build()` 把 USER 记忆(+durable ENV)
  作为 `user_profile`/`env_context` 传给 `build_system_prompt`,**全量常驻**,
  不再只走 retrieval 检索池。
- **1.2** 遗忘曲线对 Core 区失效:`forgetting`/检索打分对 USER 类型(及 durable 标记)
  跳过衰减与 archival/forget 阈值(retrieval.py / tiers.py / forgetting.py)。
- **1.3** Core 区字符预算 + 溢出策展信号(有界预算 +「溢出=该整理」)。
- **1.4** 作用域:USER 记忆 `source_session` 绑定微信 chat_id(已是默认 session 行为),
  单用户=永远记得,多用户=不串台。确认 `scope_policy=session` 下行为正确。
- **验证**:跨 session 读取断言;Core 区注入内容快照测试。

### 阶段 2:自我认知配置化 + 注入安全(中风险)
- **2.1** Agent 能力/限制改为运行时从工具注册表派生,注入 system prompt;
  从巩固/审查的可写范围中排除 self/skills/capabilities 主题。
- **2.2** 审查写入前做威胁扫描:
  拦 prompt injection / 外泄模式,因记忆进 system prompt。
- **2.3** 修正模型自我认知:在记忆指引/identity 中明确「你能跨会话记住用户的事,
  回答关于过往的问题应先翻 history/检索,不要声称记不住」。

### 阶段 3:隔离测试噪音 + 清理历史(含破坏性操作,单独确认)
- **3.1** eval / 测试通道的流量不进自动巩固(按 channel 前缀 `eval:` 跳过)。
- **3.2** 【破坏性·需单独确认】清理远程服务器 MEMORY.md 中的测试噪音段落。
  在服务器上操作真实记忆文件,先备份再清理,逐项确认。

## 五、不做什么(明确排除)
- 不引入 dreaming/REM/向量知识库/wiki —— 对单人助理是过度设计。
- 不采用物理双文件 —— echo-agent 已有 USER/ENV 类型,逻辑分层即可。
- 不重写存储结构(sqlite schema 不动),只改注入、触发、衰减适用范围。

## 六、部署说明
- 代码改在本地仓库,本地测试通过后再部署到远程服务器(123.56.188.16)。
- 阶段 0-2 是代码改动,部署后生效。阶段 3.2 直接动服务器数据,破坏性,最后做。
- 每阶段独立成可回滚的提交。
