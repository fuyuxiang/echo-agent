# 记忆管理使用指南

Echo Agent 的记忆系统采用分层架构，模拟人类记忆的工作方式——从短期工作记忆到长期归档存储，每一层都有不同的容量、持久性和检索特性。

## 架构概览

```
┌─────────────────────────────────────────────────────────┐
│                   Memory System                         │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  ┌───────────────┐    ┌───────────────────────────┐    │
│  │Working Memory │───▶│    Eligibility Check       │    │
│  │ (max 20条)    │    │    (eligibility.py)        │    │
│  └───────────────┘    └───────────┬───────────────┘    │
│                                   │                     │
│                                   ▼                     │
│  ┌───────────────┐    ┌───────────────────────────┐    │
│  │Episodic Memory│◀───│    Review & Quality        │    │
│  │ (时序索引)     │    │    (reviewer.py)           │    │
│  └───────┬───────┘    └───────────────────────────┘    │
│          │                                              │
│          ▼                                              │
│  ┌───────────────┐    ┌───────────────────────────┐    │
│  │Semantic Memory│◀──▶│  Contradiction Detection   │    │
│  │ (向量索引)     │    │  (contradiction.py)        │    │
│  └───────┬───────┘    └───────────────────────────┘    │
│          │                                              │
│          ▼                                              │
│  ┌───────────────┐    ┌───────────────────────────┐    │
│  │Archival Memory│◀───│    Consolidation           │    │
│  │ (长期存储)     │    │    (consolidator.py)       │    │
│  └───────────────┘    └───────────────────────────┘    │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │           Retrieval Layer                        │   │
│  │   Vector │ BM25 │ Hybrid    (retrieval.py)      │   │
│  └─────────────────────────────────────────────────┘   │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │    Local Embeddings & Reranking                  │   │
│  │    (local_embed.py / local_rerank.py)            │   │
│  └─────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
```

## 四层记忆详解

### 第一层：Working Memory（工作记忆）

工作记忆是进程内缓冲区，保存当前对话的即时上下文。类似于人的"短期记忆"，容量有限且不持久化。

| 属性 | 值 |
|------|------|
| 最大条目数 | 20 |
| 持久化 | 否 |
| 上下文渲染 | Markdown，最多 2000 字符 |
| 作用域 | 当前对话 |

**示例：**

```python
# 工作记忆自动收集当前对话的关键信息
working_memory.add(MemoryEntry(
    key="user_preference_lang",
    content="用户偏好中文交流",
    tier=MemoryTier.WORKING,
    type=MemoryType.USER
))

# 渲染上下文供模型使用
context = working_memory.get_context(max_chars=2000)
```

!!! note "工作记忆不跨对话"
    工作记忆在对话结束后即丢失。如需保留重要信息，系统会通过 Eligibility Check 决定是否提升到更高层级。

### 第二层：Episodic Memory（情景记忆）

情景记忆存储对话片段（episodes），按时间索引。它记录的是"发生了什么"——带有时间戳的交互记录。

| 属性 | 值 |
|------|------|
| 索引方式 | 时序索引 |
| 持久化 | 是 |
| 内容类型 | 对话片段、事件记录 |
| 衰减 | 受时间衰减影响 |

**示例：**

```python
# 情景记忆记录用户交互历史
episodic_entry = MemoryEntry(
    key="episode_2024_0315_debug_session",
    content="用户请求调试数据库连接问题，最终发现是连接池耗尽",
    tier=MemoryTier.EPISODIC,
    type=MemoryType.USER,
    provenance=Provenance(source="conversation", timestamp="2024-03-15T10:30:00Z")
)
```

### 第三层：Semantic Memory（语义记忆）

语义记忆存储结构化知识，通过向量索引实现语义检索。它记录的是"知道什么"——事实、偏好、规则。

| 属性 | 值 |
|------|------|
| 索引方式 | 向量索引（local_embed.py） |
| 持久化 | 是 |
| 内容类型 | 结构化知识、用户偏好、环境信息 |
| 检索 | 支持语义相似度搜索 |

**示例：**

```python
# 语义记忆存储结构化知识
semantic_entry = MemoryEntry(
    key="user_tech_stack",
    content="用户主要使用 Python + FastAPI 开发后端，前端使用 Vue 3",
    tier=MemoryTier.SEMANTIC,
    type=MemoryType.USER
)
```

### 第四层：Archival Memory（归档记忆）

归档记忆是长期存储层，保存经过整合和验证的重要信息。类似于人的"长期记忆"。

| 属性 | 值 |
|------|------|
| 存储方式 | 持久化长期存储 |
| 来源 | 从上层整合而来 |
| 内容类型 | 经验证的核心知识 |
| 衰减 | 极低 |

**示例：**

```python
# 归档记忆保存经过整合的核心知识
archival_entry = MemoryEntry(
    key="project_architecture_v2",
    content="项目采用微服务架构，共 5 个核心服务，通过 gRPC 通信",
    tier=MemoryTier.ARCHIVAL,
    type=MemoryType.ENVIRONMENT
)
```

## 记忆生命周期

记忆从创建到最终归档（或遗忘）经历完整的生命周期：

```
创建 ──▶ 资格检查 ──▶ 质量审核 ──▶ 存储 ──▶ 整合 ──▶ 归档
  │                                    │         │
  │                                    ▼         ▼
  │                                  衰减 ──▶ 遗忘
  │
  └──▶ 不合格 ──▶ 丢弃
```

### 创建（Creation）

记忆可以通过两种方式创建：

1. **隐式创建**：系统从对话中自动提取值得记住的信息
2. **显式创建**：通过 `memory` 工具手动存储

### 资格检查（Eligibility）

`eligibility.py` 决定哪些信息值得被记住：

- 用户明确的偏好声明
- 重复出现的模式
- 环境配置信息
- 项目关键决策

### 质量审核（Review）

`reviewer.py` 对通过资格检查的记忆进行质量控制：

- 内容是否清晰、无歧义
- 是否与已有记忆重复
- 是否包含足够上下文

### 整合（Consolidation）

`consolidator.py` 定期合并和摘要相关记忆：

```python
# 整合将多条相关记忆合并为一条精炼记忆
# 例如：多次提到"用户喜欢简洁代码风格"会被合并为一条确定性更高的记忆
```

整合在记忆条目数达到 `memory.consolidationThreshold`（默认 20）时触发；`memory.sleepConsolidation` 默认开启，会在空闲期额外执行一轮整合。

### 衰减与遗忘（Decay & Forgetting）

`forgetting.py` 实现基于时间的记忆衰减：

- 长时间未被访问的记忆会逐渐降低优先级
- 衰减到阈值以下的记忆会被标记为"遗忘"
- 遗忘不等于删除——归档层可能仍保留

衰减按 `memory.importanceDecayDays`（默认 30 天）的周期折减重要性分数。分数低于 `memory.archivalThreshold`（默认 0.05）的记忆移入归档层，低于 `memory.forgetThreshold`（默认 0.01）的记忆被遗忘。

## 检索模式

`retrieval.py` 提供三种检索模式，适用于不同场景：

### Vector（向量检索）

基于语义相似度的检索，使用 `local_embed.py` 生成嵌入向量。

```python
# 语义搜索——理解意图而非匹配关键词
results = memory.retrieve(
    query="用户的编程语言偏好",
    mode="vector"
)
```

**适用场景：** 模糊查询、概念相关的信息检索、跨语言匹配。

### BM25（关键词检索）

基于词频-逆文档频率的传统检索算法。

```python
# 精确关键词匹配
results = memory.retrieve(
    query="FastAPI database connection",
    mode="bm25"
)
```

**适用场景：** 精确术语搜索、代码片段匹配、专有名词查找。

### Hybrid（混合检索）

结合 Vector 和 BM25 的优势，通过 `local_rerank.py` 进行重排序。

```python
# 混合模式——兼顾语义理解和精确匹配
results = memory.retrieve(
    query="数据库连接池配置",
    mode="hybrid"
)
```

**适用场景：** 大多数通用查询场景（推荐默认使用）。

!!! tip "检索模式选择"
    日常使用建议选择 Hybrid 模式。只有在明确需要纯语义匹配或纯关键词匹配时，才切换到 Vector 或 BM25。

## 预取机制

`prefetch.py` 实现主动记忆加载：

- 在对话开始时，根据上下文预测可能需要的记忆
- 提前加载到工作记忆中，减少检索延迟
- 基于用户历史行为模式进行预测

## `memory` 工具使用

通过 `memory` 工具可以显式操作记忆系统：

### 存储记忆

```
memory store --key "project_db" --content "项目使用 PostgreSQL 15" --type USER
```

### 检索记忆

```
memory search --query "数据库配置" --mode hybrid --limit 5
```

### 删除记忆

```
memory delete --key "outdated_info"
```

### 查看记忆状态

```
memory status
```

!!! warning "记忆作用域"
    记忆系统按用户隔离（`memory_scope` 由 `ToolExecutionContext` 提供）。不同用户之间的记忆互不可见。

## 溯源追踪（Provenance Tracking）

每条 `MemoryEntry` 都包含溯源信息，记录记忆的来源和变更历史：

```python
class MemoryEntry:
    key: str           # 唯一标识
    content: str       # 记忆内容
    tier: MemoryTier   # 所在层级
    type: MemoryType   # USER 或 ENVIRONMENT
    provenance: Provenance  # 溯源信息
```

溯源信息包括：

- **来源（source）**：记忆从何而来（对话、工具调用、整合）
- **时间戳（timestamp）**：创建和最后修改时间
- **变更链（chain）**：整合、更新的历史记录

这使得系统可以：
- 追溯任何记忆的原始来源
- 在矛盾检测时判断哪条记忆更可信
- 审计记忆的完整生命周期

## 矛盾检测与解决

`contradiction.py` 负责发现并处理冲突记忆：

### 检测机制

当新记忆写入时，系统自动检查是否与已有记忆矛盾：

```python
# 系统发现矛盾
# 已有记忆："项目使用 MySQL 数据库"
# 新记忆："项目已迁移到 PostgreSQL"
# → 触发矛盾检测
```

### 解决策略

1. **时间优先**：更新的记忆优先级更高
2. **溯源可信度**：用户显式声明 > 系统推断
3. **确认机制**：无法自动解决时，标记待确认

!!! note "矛盾不会静默丢失"
    当检测到矛盾时，旧记忆不会被直接删除，而是标记为"已被取代"，保留完整的变更历史。

## 配置选项

### 工作记忆配置

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `max_entries` | 20 | 工作记忆最大条目数 |
| `max_context_chars` | 2000 | `get_context()` 渲染的最大字符数 |

### 检索配置

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `memory.retrievalOnMiss` | `degrade` | 检索缓存未命中时的行为：`degrade` 走有界同步检索、超时回退关键词检索；`sync` 始终完整同步检索 |
| `memory.retrievalMissTimeoutSeconds` | `0.8` | 上述有界检索的时间预算（秒），`0` 表示完全跳过 |
| `memory.rerankEnabled` | `true` | 是否对融合后的 top-K 做 cross-encoder 精排 |
| `memory.rerankTopK` | `10` | 参与精排的候选数量 |
| `memory.rerankMinScore` | `0.0` | 精排相关性下限，`0` 表示只重排不剔除 |

### 衰减配置

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `memory.importanceDecayDays` | `30.0` | 重要性衰减周期（天） |
| `memory.archivalThreshold` | `0.05` | 低于此分数的记忆进入归档层 |
| `memory.forgetThreshold` | `0.01` | 低于此分数的记忆被遗忘 |

### 整合配置

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `memory.consolidationThreshold` | `20` | 触发记忆整合的条目数阈值 |
| `memory.sleepConsolidation` | `true` | 是否启用空闲期整合 |
| `memory.contradictionDetection` | `true` | 是否启用矛盾检测 |

## 记忆类型

系统支持两种记忆类型：

- **USER**：与用户相关的记忆（偏好、习惯、历史交互）
- **ENVIRONMENT**：与环境相关的记忆（项目配置、技术栈、系统信息）

## 最佳实践

### 1. 让系统自动工作

大多数情况下，记忆系统会自动处理信息的存储和检索。只在需要确保某些关键信息被记住时才使用显式 `memory` 工具。

### 2. 使用有意义的 key

```python
# 好的 key
"user_preferred_language"
"project_deploy_target"

# 不好的 key
"info1"
"temp"
```

### 3. 保持内容简洁明确

每条记忆应该是自包含的、可以独立理解的一段信息。避免过长或含糊的内容。

### 4. 利用 type 区分作用域

- 用户个人偏好 → `USER` 类型
- 项目/环境信息 → `ENVIRONMENT` 类型

### 5. 信任矛盾检测

当系统提示记忆矛盾时，及时确认哪条是正确的。不要忽略矛盾提示。

### 6. 定期观察记忆状态

使用 `memory status` 了解当前记忆的分布和健康度，确保重要信息没有因衰减而丢失。

!!! tip "关于预取"
    预取机制会根据对话上下文自动加载相关记忆。如果发现某些记忆总是在需要时缺失，可以考虑将其提升到语义记忆层以提高检索命中率。

