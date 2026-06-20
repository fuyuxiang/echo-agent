# 配置可读性基础设施(子项目 A)实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 echo-agent 配置体系建立"如实呈现 + 防止再退化"的基础设施——字段有效性元数据、双形式中英文档、`config dump/explain/validate` 命令、守护测试、死字段处置 backlog。

**Architecture:** 在 `schema.py` 每个字段上用 Pydantic `Field(json_schema_extra=...)` 附结构化元数据,作为唯一真相源。所有产物(文档、命令、守护测试、backlog)都从这份元数据派生。全程不修改任何运行时行为。

**Tech Stack:** Python 3.11+、Pydantic v2、PyYAML、argparse、pytest、ruff;复用现有 `echo_agent/cli/i18n`(`t()`/`set_locale()`)与 `echo_agent/cli/colors`。

## Global Constraints

- 语言:所有面向用户的文案、文档、说明用简体中文;代码注释/标识符用英文(项目惯例)。
- 提交信息禁止任何 Claude/Anthropic 署名或 emoji 标记,只描述改动本身。
- **本子项目全程不改任何运行时行为**:不改字段类型/默认值、不删字段、不接线任何死字段。
- 元数据承载方式固定为 `Field(json_schema_extra={...})`,不引入新依赖。
- 字段名风格:YAML 产物用 camelCase(与 `default.yaml`、向导写出的配置一致);Markdown 同时标注 camelCase 与 snake_case。
- 破坏性操作前确认(本计划无破坏性操作)。
- 每个任务结束 `ruff check .` 与相关 `pytest` 必须通过。

## 元数据契约(所有任务共用)

每个字段的 `json_schema_extra` 必须含 `status`,取值 `"effective"` 或 `"dead"`:
- `status="effective"`:必须另有 `ref`(读取点 `"相对路径:行号"`)、`desc_zh`、`desc_en`(各一行面向用户的说明)。
- `status="dead"`:必须另有 `reason`(为何不生效,一行)、`disposition`(`"fix"` | `"remove"` | `"keep"`)。

## 文件结构

- 新建 `echo_agent/config/metadata.py` — 字段元数据的遍历与提取工具(纯函数)。
- 修改 `echo_agent/config/schema.py` — 给全部字段补 `json_schema_extra` 元数据(不改类型/默认值)。
- 新建 `echo_agent/config/docgen.py` — 从元数据渲染 YAML 模板与 Markdown 参考(纯函数,不碰磁盘)。
- 新建 `echo_agent/cli/config_cmd.py` — `config dump/explain/validate` 实现 + gen-docs 开发命令。
- 修改 `echo_agent/__main__.py` — 注册 `config` 子命令并分发。
- 新建 `docs/config-reference.yaml` / `.en.yaml` / `config-reference.md` / `.en.md` — 生成产物(提交进仓)。
- 新建 `docs/config-dead-fields-backlog.md` — 死字段处置 backlog(生成产物)。
- 新建测试:`tests/test_config_metadata.py`、`tests/test_config_metadata_guard.py`、`tests/test_config_docgen.py`、`tests/test_config_cmd.py`、`tests/test_config_backlog.py`、`tests/test_docs_consistency.py`。

---

### Task 1: 元数据提取工具(metadata.py)

地基。提供"遍历 `Config` 全部字段(含嵌套子模型)并取出每个字段的元数据"的纯函数,后续所有任务都依赖它。本任务**不**给 schema 补元数据(那是 Task 2),只先把读取能力做出来并用 schema 现有(尚无元数据)字段验证遍历逻辑。

**Files:**
- Create: `echo_agent/config/metadata.py`
- Test: `tests/test_config_metadata.py`

**Interfaces:**
- Consumes: `echo_agent.config.schema.Config`(Pydantic v2 模型)。
- Produces:
  - `FieldInfo` dataclass:`path: str`(点路径,camelCase,如 `"memory.archivalThreshold"`)、`snake_path: str`(如 `"memory.archival_threshold"`)、`type_str: str`、`default: object`、`choices: list[str] | None`(Literal 取值,否则 None)、`extra: dict`(该字段的 `json_schema_extra`,无则 `{}`)。
  - `iter_fields(model: type[BaseModel] = Config, prefix: str = "", snake_prefix: str = "") -> Iterator[FieldInfo]`:深度优先遍历;遇到嵌套 `_Base` 子模型则递归下钻,叶子字段产出 `FieldInfo`。**容器型字段(`list[_Base]` / `dict[str, _Base]`,如 `providers`、`routes`、`mcp_servers`、`worker_profiles`、`platforms`)既产出容器字段本身作为叶子,也下钻其元素子模型**:元素字段路径加容器标记后缀——list 用 `[]`(如 `models.providers[].apiKey`),dict 用 `{}`(如 `tools.mcpServers{}.command`)。非 `_Base` 元素的容器(如 `list[str]`)仍按普通叶子处理。

  > 修订说明(2026-06-20):此处由最初的"容器按叶子、不下钻"改为"下钻容器子模型"。原因:`providers`/`routes`/`mcp_servers`/`worker_profiles`/`platforms` 内部含大量用户需配置的有效字段(README 头号卖点)与 8 个死字段(含安全 bug `gateway.platforms.enabled`),不下钻则文档与 backlog 残缺。

- [ ] **Step 1: 写失败测试**

```python
# tests/test_config_metadata.py
"""Tests for config field metadata traversal."""
from __future__ import annotations

from echo_agent.config.metadata import FieldInfo, iter_fields
from echo_agent.config.schema import Config


def test_iter_fields_returns_leaf_fields():
    fields = list(iter_fields(Config))
    paths = {f.path for f in fields}
    # 顶层标量
    assert "workspace" in paths
    # 嵌套叶子(camelCase 点路径)
    assert "memory.archivalThreshold" in paths
    assert "compression.triggerRatio" in paths
    # 不应把中间子模型本身当叶子产出
    assert "memory" not in paths
    assert "compression" not in paths


def test_field_info_has_snake_path_and_type():
    fields = {f.path: f for f in iter_fields(Config)}
    info = fields["memory.archivalThreshold"]
    assert info.snake_path == "memory.archival_threshold"
    assert "float" in info.type_str.lower()


def test_literal_choices_extracted():
    fields = {f.path: f for f in iter_fields(Config)}
    # SecurityConfig.profile 是 Literal["personal_cli","daemon","public_gateway"]
    info = fields["security.profile"]
    assert info.choices == ["personal_cli", "daemon", "public_gateway"]


def test_container_field_descends_into_submodel():
    paths = {f.path for f in iter_fields(Config)}
    # tools.mcpServers 是 dict[str, MCPServerConfig]:容器本身产出
    assert "tools.mcpServers" in paths
    # 且下钻其元素子模型,dict 用 {} 标记
    assert "tools.mcpServers{}.command" in paths
    # list[_Base] 用 [] 标记
    assert "models.providers[].apiKey" in paths
    assert "models.routes[].model" in paths
    assert "multiAgent.workerProfiles[].instructions" in paths
    assert "gateway.platforms{}.rateLimitRpm" in paths


def test_extra_defaults_to_empty_dict():
    fields = {f.path: f for f in iter_fields(Config)}
    # 尚未补元数据时 extra 为空 dict,不是 None
    assert fields["workspace"].extra == {}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `pytest tests/test_config_metadata.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'echo_agent.config.metadata'`

- [ ] **Step 3: 实现 metadata.py**

```python
# echo_agent/config/metadata.py
"""Traversal utilities over the Config schema and its field metadata.

Pure functions: read the Pydantic model definition, yield per-field info.
No side effects, no disk access.
"""
from __future__ import annotations

import typing
from collections.abc import Iterator
from dataclasses import dataclass, field
from typing import Any, Literal, get_args, get_origin

from pydantic import BaseModel
from pydantic.alias_generators import to_camel

from echo_agent.config.schema import Config, _Base


@dataclass
class FieldInfo:
    path: str
    snake_path: str
    type_str: str
    default: Any
    choices: list[str] | None
    extra: dict[str, Any] = field(default_factory=dict)


def _is_base_submodel(annotation: Any) -> type[BaseModel] | None:
    """Return the model class if annotation is a _Base subclass, else None."""
    if isinstance(annotation, type) and issubclass(annotation, _Base):
        return annotation
    return None


def _literal_choices(annotation: Any) -> list[str] | None:
    if get_origin(annotation) is Literal:
        return [str(a) for a in get_args(annotation)]
    return None


def _type_str(annotation: Any) -> str:
    name = getattr(annotation, "__name__", None)
    if name:
        return name
    return str(annotation).replace("typing.", "")


def iter_fields(
    model: type[BaseModel] = Config,
    prefix: str = "",
    snake_prefix: str = "",
) -> Iterator[FieldInfo]:
    for name, fld in model.model_fields.items():
        camel = to_camel(name)
        path = f"{prefix}.{camel}" if prefix else camel
        snake_path = f"{snake_prefix}.{name}" if snake_prefix else name
        annotation = fld.annotation
        submodel = _is_base_submodel(annotation)
        if submodel is not None:
            yield from iter_fields(submodel, path, snake_path)
            continue
        extra = fld.json_schema_extra if isinstance(fld.json_schema_extra, dict) else {}
        yield FieldInfo(
            path=path,
            snake_path=snake_path,
            type_str=_type_str(annotation),
            default=fld.get_default(call_default_factory=True),
            choices=_literal_choices(annotation),
            extra=dict(extra),
        )
```

- [ ] **Step 4: 运行测试确认通过**

Run: `pytest tests/test_config_metadata.py -v`
Expected: PASS(5 passed)。若 `test_container_field_is_leaf_not_descended` 失败,确认 `dict[str, MCPServerConfig]` 的 `get_origin` 不是 `_Base` 子类(它是 `dict`,不会进入 `_is_base_submodel` 分支,符合预期)。

- [ ] **Step 5: ruff 与提交**

```bash
ruff check echo_agent/config/metadata.py tests/test_config_metadata.py
git add echo_agent/config/metadata.py tests/test_config_metadata.py
git commit -m "新增配置字段元数据遍历工具 iter_fields/FieldInfo"
```

---

### Task 2: 给 schema 全部字段补元数据

把元数据契约落到 `schema.py` 每个字段上。这是体量最大的任务,但机械、低风险:**只加 `json_schema_extra`,绝不改 `default=` 的值、不改类型、不删字段。** 字段本身已有 `Field(default_factory=...)` 的,改成 `Field(default_factory=..., json_schema_extra={...})`;裸默认值的(如 `enabled: bool = True`)改成 `enabled: bool = Field(default=True, json_schema_extra={...})`。

**Files:**
- Modify: `echo_agent/config/schema.py`(全部字段)
- Test: 复用 `tests/test_config_metadata.py`(本任务末尾加抽样断言)

**Interfaces:**
- Consumes: `iter_fields`(Task 1)。
- Produces: schema 全部字段带合契约的 `json_schema_extra`。Task 3/4/5 依赖它。

**死字段权威清单(status="dead",共 35 项)。** 下表是本子项目唯一的死字段判定源,逐字段照填 `reason` 与 `disposition`。未在此表中的字段一律 `status="effective"`。

| 字段路径(snake) | disposition | reason(照填) |
|---|---|---|
| `channels.wecom.encoding_aes_key` | fix | 企业微信加密回调密钥未接线,wecom.py 仅用明文 token 做 SHA1,从不 AES 解密 |
| `gateway.platforms.{}.enabled` | fix | server.py:90 平台循环只读 rate_limit_rpm,enabled 从不检查,禁用平台无效 |
| `gateway.platforms.{}.home_channel` | remove | 全仓无读取点 |
| `gateway.platforms.{}.home_chat_id` | remove | 全仓无读取点 |
| `gateway.platforms.{}.reply_mode` | remove | 全仓无读取点 |
| `gateway.max_agent_cache_size` | remove | 仅 schema 定义,无读取点 |
| `gateway.enable_progressive_edit` | fix | ProgressiveEditor 在 server.py:85 无条件实例化,此开关从不被读;真正开关是 emit_progress_events |
| `multi_agent.worker_profiles.{}.provider` | remove | executor 始终用注入的 provider,profile.provider 从不被读 |
| `evaluation.enabled` | fix | 无读取点,eval 子命令被调用时无条件运行 |
| `evaluation.parallel_cases` | fix | 并发度取自 CLI --parallel(默认 3),从不读 config |
| `models.cost_limit_daily_usd` | remove | 无读取点,成本限制由 cost.daily_budget_usd 实现,此字段为误导孤儿 |
| `models.routes.{}.context_window` | remove | 仅透传进 RouteDecision,无消费方;真实窗口来自 session.context_window_tokens |
| `models.providers.{}.max_retries` | fix | 重试硬编码在 LLMProvider._RETRY_DELAYS,该配置无效果 |
| `tools.exec.timeout_seconds` | fix | shell/process 工具用类级默认值,该字段无效;仅 code_exec.timeout_seconds 生效 |
| `tools.mcp_servers.{}.transport` | fix | _create_transport 按 url/command 隐式选择,显式 transport 被忽略 |
| `observability.trace_enabled` | fix | 仅向导提示用,运行时 TraceLogger 在 loop.py 无条件构造,开关不生效 |
| `observability.show_tool_calls` | remove | 无运行时读取点 |
| `observability.show_route_decisions` | remove | 无读取点,路由决策在 inference_stage 无条件记录 |
| `scheduler.dead_task_timeout_seconds` | remove | 仅 schema 定义,无消费方 |
| `storage.backend` | fix | app.py:71 永远构造 SQLiteBackend,filesystem 后端不存在,此开关无效 |
| `storage.workspace_dir` | remove | 无读取点,agent 直接用 self.workspace/"data" |
| `skills.auto_load` | remove | 仅 schema 定义,无消费方 |
| `skills.platform_disabled` | remove | 仅 schema 定义,无消费方 |
| `planning.max_branches` | fix | 未传入 planner 构造(loop.py 只接 default_strategy/max_tree_depth/reflection_enabled) |
| `a2a.capabilities` | fix | AgentCard 构造时未用,改用 a2a/models.py 默认值 |
| `agent.reasoning_effort` | fix | 仅 schema 定义,从未接线到 provider 的 ChatRequest.reasoning_effort |
| `session.archive_after_hours` | fix | 构造 SessionManager 时未传,代码用默认 168 |
| `memory.hybrid_retrieval` | fix | HybridRetriever 在 loop.py:414 无条件构造,开关不控制任何分支 |
| `memory.adaptive_forgetting` | fix | 遗忘曲线在 store.py:172 无条件创建,开关不生效 |
| `memory.archival_threshold` | fix | store.py:174 硬编码 0.05,配置值未传入 ForgettingCurve |
| `memory.forget_threshold` | fix | store.py:175 硬编码 0.01,配置值未传入 |
| `memory.max_episodes` | remove | 全仓无引用,episode 无数量上限控制 |
| `memory.embedding_batch_size` | remove | 全仓无引用 |
| `memory.consolidation_idle_seconds` | remove | 全仓无引用 |
| `knowledge.require_citations` | fix | 引用始终生成(index.py 无条件输出 citation),开关不生效 |

> 注:`{}` 表示该字段位于 `dict`/`list` 容器的元素子模型上(如 `GatewayPlatformConfig`、`WorkerProfileConfig`、`ModelRouteConfig`、`ProviderConfig`、`MCPServerConfig`)。即使这些容器在运行时可能为空,其元素子模型的字段仍要标注——文档与守护测试遍历的是类型定义,不是运行时实例。

**effective 字段的 `ref` 来源:** 见设计文档与项目记忆 `config-dead-fields-audit.md` 中各 agent 给出的 `file:line`。`ref` 路径相对于 `echo_agent/`(如 `"agent/loop.py:104"`)。

**effective 字段的 desc:** 一行中文(`desc_zh`)+ 一行英文(`desc_en`),面向最终用户描述"这项控制什么"。

- [ ] **Step 1: 写守护风格的抽样失败测试**

在 `tests/test_config_metadata.py` 末尾追加:

```python
def test_dead_fields_are_marked():
    fields = {f.snake_path: f for f in iter_fields(Config)}
    # 抽查几个已知死字段
    assert fields["storage.backend"].extra.get("status") == "dead"
    assert fields["agent.reasoning_effort"].extra.get("status") == "dead"
    assert fields["memory.archival_threshold"].extra.get("disposition") == "fix"
    assert fields["models.cost_limit_daily_usd"].extra.get("disposition") == "remove"


def test_effective_fields_have_desc():
    fields = {f.snake_path: f for f in iter_fields(Config)}
    info = fields["compression.trigger_ratio"]
    assert info.extra.get("status") == "effective"
    assert info.extra.get("desc_zh")
    assert info.extra.get("desc_en")
    assert info.extra.get("ref")
```

- [ ] **Step 2: 运行确认失败**

Run: `pytest tests/test_config_metadata.py::test_dead_fields_are_marked tests/test_config_metadata.py::test_effective_fields_have_desc -v`
Expected: FAIL(字段 extra 为空,断言不通过)

- [ ] **Step 3: 给 schema 全部字段补元数据**

逐字段加 `json_schema_extra`。dead 字段照上表填;其余填 effective。示例(只展示形态,实现时覆盖全部字段):

```python
# effective 示例(原:enabled: bool = True)
enabled: bool = Field(
    default=True,
    json_schema_extra={
        "status": "effective", "ref": "agent/loop.py:184",
        "desc_zh": "是否启用认知记忆", "desc_en": "Enable cognitive memory",
    },
)

# effective + default_factory 示例(原:safe_bins: list[str] = Field(default_factory=lambda: [...]))
safe_bins: list[str] = Field(
    default_factory=lambda: ["awk", "cat", "date", "echo", "find", "grep",
        "head", "ls", "pwd", "rg", "sed", "sort", "tail", "tr", "uniq", "wc"],
    json_schema_extra={
        "status": "effective", "ref": "security/guards.py:265",
        "desc_zh": "allowlist 模式下免审批直接放行的安全命令",
        "desc_en": "Commands allowed without approval under allowlist mode",
    },
)

# dead 示例(原:backend: Literal["sqlite","filesystem"] = "sqlite")
backend: Literal["sqlite", "filesystem"] = Field(
    default="sqlite",
    json_schema_extra={
        "status": "dead", "disposition": "fix",
        "reason": "app.py:71 永远构造 SQLiteBackend,filesystem 后端不存在,此开关无效",
    },
)
```

注意:Pydantic v2 中 `Field(default=X)` 与裸 `= X` 等价,改写不影响默认值与校验。`default_factory` 保持原样,只追加 `json_schema_extra`。

- [ ] **Step 4: 运行全部元数据测试确认通过**

Run: `pytest tests/test_config_metadata.py -v`
Expected: PASS(7 passed)

- [ ] **Step 5: 跑全量测试确认未破坏运行时**

Run: `pytest -q`
Expected: 与基线一致(本任务不改行为,既有测试应全绿)

- [ ] **Step 6: ruff 与提交**

```bash
ruff check echo_agent/config/schema.py tests/test_config_metadata.py
git add echo_agent/config/schema.py tests/test_config_metadata.py
git commit -m "为配置 schema 全部字段补有效性元数据(effective/dead)"
```

---

### Task 3: 守护测试(防再退化)

锁住元数据契约:任何字段漏标 `status`、或元数据不齐、或 `ref` 路径不存在,CI 红灯。放在 docgen/命令之前,先把"地基不会塌"焊死。

**Files:**
- Create: `tests/test_config_metadata_guard.py`

**Interfaces:**
- Consumes: `iter_fields`(Task 1)、已补元数据的 schema(Task 2)。
- Produces: 无导出;纯断言。

- [ ] **Step 1: 写守护测试**

```python
# tests/test_config_metadata_guard.py
"""Guard tests: every Config field must declare valid metadata.

This is the anti-regression mechanism. A new field with no status, or an
effective field whose ref points at a non-existent file, fails CI.
"""
from __future__ import annotations

from pathlib import Path

from echo_agent.config.metadata import iter_fields
from echo_agent.config.schema import Config

_ECHO_ROOT = Path(__file__).resolve().parent.parent / "echo_agent"


def test_every_field_declares_status():
    missing = [f.path for f in iter_fields(Config)
               if f.extra.get("status") not in ("effective", "dead")]
    assert not missing, f"字段缺少 status 元数据: {missing}"


def test_effective_fields_have_desc_and_ref():
    bad = []
    for f in iter_fields(Config):
        if f.extra.get("status") != "effective":
            continue
        if not (f.extra.get("desc_zh") and f.extra.get("desc_en") and f.extra.get("ref")):
            bad.append(f.path)
    assert not bad, f"effective 字段缺少 desc_zh/desc_en/ref: {bad}"


def test_dead_fields_have_reason_and_disposition():
    bad = []
    for f in iter_fields(Config):
        if f.extra.get("status") != "dead":
            continue
        if not f.extra.get("reason") or f.extra.get("disposition") not in ("fix", "remove", "keep"):
            bad.append(f.path)
    assert not bad, f"dead 字段缺少 reason 或合法 disposition: {bad}"


def test_effective_ref_files_exist():
    bad = []
    for f in iter_fields(Config):
        if f.extra.get("status") != "effective":
            continue
        ref = f.extra.get("ref", "")
        rel_path = ref.split(":")[0]
        if not (_ECHO_ROOT / rel_path).exists():
            bad.append((f.path, ref))
    assert not bad, f"effective 字段 ref 指向不存在的文件: {bad}"
```

- [ ] **Step 2: 运行确认通过**

Run: `pytest tests/test_config_metadata_guard.py -v`
Expected: PASS(4 passed)。若 `test_every_field_declares_status` 报出字段列表,回 Task 2 补漏;若 `test_effective_ref_files_exist` 报错,修正 schema 里写错的 ref 路径。

- [ ] **Step 3: ruff 与提交**

```bash
ruff check tests/test_config_metadata_guard.py
git add tests/test_config_metadata_guard.py
git commit -m "新增配置元数据守护测试,强制每个字段声明有效性"
```

---

### Task 4: 文档生成器(docgen.py)

纯函数:把字段元数据渲染成注释版 YAML 与 Markdown 表。**只渲染 effective 字段。** 不碰磁盘(返回字符串),便于测试。

**Files:**
- Create: `echo_agent/config/docgen.py`
- Test: `tests/test_config_docgen.py`

**Interfaces:**
- Consumes: `iter_fields`、`FieldInfo`(Task 1);已补元数据的 schema(Task 2)。
- Produces:
  - `render_yaml(lang: str = "zh") -> str`:注释版 YAML 文本。每个 effective 叶子字段上方一行 `# <desc> (默认: <default>[, 可选: a|b])`,字段名 camelCase,值为默认值,按组缩进。
  - `render_markdown(lang: str = "zh") -> str`:按顶层组分章(`## <group>`),每章一张表,列:字段(camelCase) / snake / 类型 / 默认值 / 可选值 / 说明。
  - 两者都跳过 `status != "effective"` 的字段。

- [ ] **Step 1: 写失败测试**

```python
# tests/test_config_docgen.py
"""Tests for config reference doc generation."""
from __future__ import annotations

from echo_agent.config.docgen import render_markdown, render_yaml


def test_yaml_includes_effective_excludes_dead():
    out = render_yaml("zh")
    # effective 字段出现(camelCase)
    assert "triggerRatio" in out
    # dead 字段不出现
    assert "archivalThreshold" not in out
    assert "reasoningEffort" not in out


def test_yaml_has_comment_with_default():
    out = render_yaml("zh")
    # 注释行包含中文说明与默认值
    assert "# " in out
    assert "0.7" in out  # compression.triggerRatio 默认值


def test_markdown_has_group_headers_and_choices():
    out = render_markdown("zh")
    assert "## security" in out or "## Security" in out
    # security.profile 是 effective Literal,应列出可选值
    assert "personal_cli" in out
    # dead 字段不出现
    assert "showToolCalls" not in out


def test_lang_switch_changes_desc():
    zh = render_yaml("zh")
    en = render_yaml("en")
    # 同一字段在两种语言下注释不同(desc_zh vs desc_en)
    assert zh != en
```

- [ ] **Step 2: 运行确认失败**

Run: `pytest tests/test_config_docgen.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'echo_agent.config.docgen'`

- [ ] **Step 3: 实现 docgen.py**

```python
# echo_agent/config/docgen.py
"""Render the config reference (annotated YAML + Markdown) from field metadata.

Pure functions: build strings from iter_fields(); no disk access. Only
fields with status == "effective" are rendered — dead fields never reach
user-facing docs.
"""
from __future__ import annotations

from echo_agent.config.metadata import FieldInfo, iter_fields
from echo_agent.config.schema import Config


def _desc(info: FieldInfo, lang: str) -> str:
    key = "desc_zh" if lang == "zh" else "desc_en"
    return str(info.extra.get(key) or info.extra.get("desc_en") or "")


def _fmt_default(default: object) -> str:
    if default == "" :
        return '""'
    if isinstance(default, (list, dict)) and not default:
        return "[]" if isinstance(default, list) else "{}"
    return str(default)


def _effective_fields() -> list[FieldInfo]:
    return [f for f in iter_fields(Config) if f.extra.get("status") == "effective"]


def render_yaml(lang: str = "zh") -> str:
    lines: list[str] = []
    if lang == "zh":
        lines.append("# Echo Agent 配置参考(自动生成,请勿手改)")
    else:
        lines.append("# Echo Agent configuration reference (auto-generated, do not edit)")
    last_top = None
    for info in _effective_fields():
        parts = info.path.split(".")
        top = parts[0]
        if top != last_top:
            lines.append("")
            lines.append(f"# ── {top} ──")
            last_top = top
        indent = "  " * (len(parts) - 1)
        desc = _desc(info, lang)
        suffix = f" (默认: {_fmt_default(info.default)})" if lang == "zh" else f" (default: {_fmt_default(info.default)})"
        if info.choices:
            choice_str = "|".join(info.choices)
            suffix += f" [{choice_str}]"
        lines.append(f"{indent}# {desc}{suffix}")
        lines.append(f"{indent}{parts[-1]}: {_fmt_default(info.default)}")
    return "\n".join(lines) + "\n"


def render_markdown(lang: str = "zh") -> str:
    header_field = "字段" if lang == "zh" else "Field"
    header = f"# Echo Agent {'配置参考' if lang == 'zh' else 'Configuration Reference'}\n"
    by_group: dict[str, list[FieldInfo]] = {}
    for info in _effective_fields():
        top = info.path.split(".")[0]
        by_group.setdefault(top, []).append(info)
    blocks: list[str] = [header]
    for group, infos in by_group.items():
        blocks.append(f"## {group}\n")
        blocks.append(f"| {header_field} | snake | type | default | choices | {'说明' if lang=='zh' else 'description'} |")
        blocks.append("|---|---|---|---|---|---|")
        for info in infos:
            choices = "/".join(info.choices) if info.choices else "—"
            blocks.append(
                f"| `{info.path}` | `{info.snake_path}` | {info.type_str} | "
                f"`{_fmt_default(info.default)}` | {choices} | {_desc(info, lang)} |"
            )
        blocks.append("")
    return "\n".join(blocks) + "\n"
```

- [ ] **Step 4: 运行确认通过**

Run: `pytest tests/test_config_docgen.py -v`
Expected: PASS(4 passed)

- [ ] **Step 5: ruff 与提交**

```bash
ruff check echo_agent/config/docgen.py tests/test_config_docgen.py
git add echo_agent/config/docgen.py tests/test_config_docgen.py
git commit -m "新增配置文档生成器 docgen(注释 YAML + Markdown,仅有效字段)"
```

---

### Task 5: config 命令模块(dump/explain/validate/gen-docs)

实现面向用户的运行时命令与开发期 gen-docs。本任务交付一个可被 `__main__.py` 调用的入口函数;CLI 注册在 Task 6。

**Files:**
- Create: `echo_agent/cli/config_cmd.py`
- Test: `tests/test_config_cmd.py`

**Interfaces:**
- Consumes: `load_config`、`resolve_config_file`、`ConfigError`(`echo_agent.config.loader`);`iter_fields`、`FieldInfo`(Task 1);`render_yaml`、`render_markdown`(Task 4);`Config`(schema)。
- Produces:
  - `run_config_command(action: str, key: str = "", *, fmt: str = "yaml", show_source: bool = False, config_path=None, workspace=None) -> int`:返回进程退出码(validate 失败返回非 0,其余返回 0)。
  - `SECRET_HINTS: tuple[str, ...] = ("key", "secret", "token", "password")`(脱敏判定,与向导一致)。
  - `redact(data: dict) -> dict`:递归把键名命中 `SECRET_HINTS` 的值替换为 `"****"`(非空值才替换,空值保留)。
  - `known_paths() -> set[str]`:schema 全部字段的 camelCase 与 snake 点路径集合(供 validate 检测未知字段)。

- [ ] **Step 1: 写失败测试**

```python
# tests/test_config_cmd.py
"""Tests for the `config` CLI command (dump/explain/validate)."""
from __future__ import annotations

import pytest

from echo_agent.cli.config_cmd import known_paths, redact, run_config_command


def test_redact_masks_secret_keys():
    data = {"models": {"providers": [{"name": "openai", "apiKey": "sk-secret"}]},
            "channels": {"telegram": {"token": "abc"}}}
    out = redact(data)
    assert out["models"]["providers"][0]["apiKey"] == "****"
    assert out["channels"]["telegram"]["token"] == "****"
    # 非敏感字段保留
    assert out["models"]["providers"][0]["name"] == "openai"


def test_redact_keeps_empty_values():
    out = redact({"channels": {"telegram": {"token": ""}}})
    assert out["channels"]["telegram"]["token"] == ""


def test_dump_prints_yaml(capsys):
    rc = run_config_command("dump")
    assert rc == 0
    out = capsys.readouterr().out
    assert "workspace" in out


def test_explain_effective_field(capsys):
    rc = run_config_command("explain", "compression.triggerRatio")
    assert rc == 0
    out = capsys.readouterr().out
    assert "0.7" in out  # 默认值
    assert "effective" in out.lower() or "生效" in out


def test_explain_dead_field_warns(capsys):
    rc = run_config_command("explain", "storage.backend")
    out = capsys.readouterr().out
    # 死字段必须明确提示未生效
    assert "未生效" in out or "not in effect" in out.lower() or "dead" in out.lower()


def test_explain_unknown_key(capsys):
    rc = run_config_command("explain", "memory.nonsenseField")
    assert rc != 0
    out = capsys.readouterr().out
    assert "未知" in out or "unknown" in out.lower()


def test_known_paths_contains_both_styles():
    paths = known_paths()
    assert "memory.archivalThreshold" in paths
    assert "memory.archival_threshold" in paths


def test_validate_clean_config(tmp_path, capsys):
    cfg = tmp_path / "echo-agent.yaml"
    cfg.write_text("memory:\n  enabled: true\n", encoding="utf-8")
    rc = run_config_command("validate", config_path=str(cfg))
    assert rc == 0


def test_validate_reports_unknown_field(tmp_path, capsys):
    cfg = tmp_path / "echo-agent.yaml"
    cfg.write_text("memory:\n  enabledd: true\n", encoding="utf-8")
    rc = run_config_command("validate", config_path=str(cfg))
    out = capsys.readouterr().out
    assert rc != 0
    assert "enabledd" in out


def test_validate_warns_dead_field(tmp_path, capsys):
    cfg = tmp_path / "echo-agent.yaml"
    cfg.write_text("storage:\n  backend: filesystem\n", encoding="utf-8")
    rc = run_config_command("validate", config_path=str(cfg))
    out = capsys.readouterr().out
    # 用户显式设了死字段 → 警告不生效(但配置本身合法,rc 可为 0)
    assert "backend" in out and ("未生效" in out or "not in effect" in out.lower())
```

- [ ] **Step 2: 运行确认失败**

Run: `pytest tests/test_config_cmd.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'echo_agent.cli.config_cmd'`

- [ ] **Step 3: 实现 config_cmd.py**

```python
# echo_agent/cli/config_cmd.py
"""The `config` command: dump / explain / validate / gen-docs.

dump/explain/validate are user-facing runtime commands. gen-docs is a
developer command that writes the reference docs (used by CI consistency
checks). None of these mutate runtime behaviour.
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import yaml

from echo_agent.config.docgen import render_markdown, render_yaml
from echo_agent.config.loader import ConfigError, load_config, resolve_config_file
from echo_agent.config.metadata import FieldInfo, iter_fields
from echo_agent.config.schema import Config

SECRET_HINTS: tuple[str, ...] = ("key", "secret", "token", "password")


def _is_secret(key: str) -> bool:
    low = key.lower()
    return any(h in low for h in SECRET_HINTS)


def redact(data: Any) -> Any:
    if isinstance(data, dict):
        out: dict[str, Any] = {}
        for k, v in data.items():
            if _is_secret(k) and isinstance(v, str) and v:
                out[k] = "****"
            else:
                out[k] = redact(v)
        return out
    if isinstance(data, list):
        return [redact(x) for x in data]
    return data


def known_paths() -> set[str]:
    paths: set[str] = set()
    for f in iter_fields(Config):
        paths.add(f.path)
        paths.add(f.snake_path)
    return paths


def _field_by_key(key: str) -> FieldInfo | None:
    for f in iter_fields(Config):
        if key in (f.path, f.snake_path):
            return f
    return None


def _dump(fmt: str, show_source: bool, config_path, workspace) -> int:
    config_file = resolve_config_file(config_path=config_path, search_dir=workspace)
    config = load_config(config_path=config_file)
    data = redact(config.model_dump(by_alias=True))
    if fmt == "json":
        print(json.dumps(data, indent=2, ensure_ascii=False, default=str))
    else:
        print(yaml.dump(data, default_flow_style=False, allow_unicode=True, sort_keys=False))
    return 0


def _explain(key: str, config_path, workspace) -> int:
    info = _field_by_key(key)
    if info is None:
        print(f"未知配置项 / unknown key: {key}")
        return 1
    config_file = resolve_config_file(config_path=config_path, search_dir=workspace)
    config = load_config(config_path=config_file)
    current = config
    for part in info.snake_path.split("."):
        current = getattr(current, part, None)
        if current is None:
            break
    status = info.extra.get("status")
    print(f"配置项 / key:   {info.path}  ({info.snake_path})")
    print(f"类型 / type:    {info.type_str}")
    print(f"默认值 / def:   {info.default!r}")
    print(f"当前值 / now:   {current!r}")
    if info.choices:
        print(f"可选 / choices: {'|'.join(info.choices)}")
    print(f"说明 / desc:    {info.extra.get('desc_zh', '')}")
    print(f"                {info.extra.get('desc_en', '')}")
    if status == "dead":
        print(f"⚠ 此项当前未生效 / not in effect: {info.extra.get('reason', '')}")
    return 0


def _validate(config_path, workspace) -> int:
    config_file = resolve_config_file(config_path=config_path, search_dir=workspace)
    raw: dict[str, Any] = {}
    if config_file and Path(config_file).exists():
        with open(config_file, encoding="utf-8") as fh:
            raw = yaml.safe_load(fh) or {}
    # 1) schema 校验
    try:
        load_config(config_path=config_file)
    except ConfigError as e:
        print(f"配置非法 / invalid:\n{e}")
        return 1
    rc = 0
    known = known_paths()
    dead_paths = {f.snake_path: f for f in iter_fields(Config)
                  if f.extra.get("status") == "dead"}
    dead_camel = {f.path: f for f in iter_fields(Config)
                  if f.extra.get("status") == "dead"}

    # 2) 未知字段检测 + 3) 死字段提示(扁平化用户键)
    def walk(node: Any, prefix: str) -> None:
        nonlocal rc
        if not isinstance(node, dict):
            return
        for k, v in node.items():
            path = f"{prefix}.{k}" if prefix else k
            if isinstance(v, dict):
                # 仅当 path 是已知中间组才继续下钻;否则报未知
                if any(p == path or p.startswith(path + ".") for p in known):
                    walk(v, path)
                elif path not in known:
                    print(f"未知配置项 / unknown: {path}")
                    rc = 1
                continue
            if path not in known:
                print(f"未知配置项 / unknown: {path}")
                rc = 1
            elif path in dead_paths or path in dead_camel:
                info = dead_paths.get(path) or dead_camel[path]
                print(f"⚠ {path} 已设置但未生效 / set but not in effect: {info.extra.get('reason','')}")

    walk(raw, "")
    if rc == 0:
        print("配置有效 / configuration valid")
    return rc


def gen_docs(out_dir: str = "docs") -> None:
    """Developer command: write the four reference files + backlog (Task 7)."""
    base = Path(out_dir)
    base.mkdir(parents=True, exist_ok=True)
    (base / "config-reference.yaml").write_text(render_yaml("zh"), encoding="utf-8")
    (base / "config-reference.en.yaml").write_text(render_yaml("en"), encoding="utf-8")
    (base / "config-reference.md").write_text(render_markdown("zh"), encoding="utf-8")
    (base / "config-reference.en.md").write_text(render_markdown("en"), encoding="utf-8")


def run_config_command(
    action: str,
    key: str = "",
    *,
    fmt: str = "yaml",
    show_source: bool = False,
    config_path=None,
    workspace=None,
) -> int:
    if action == "dump":
        return _dump(fmt, show_source, config_path, workspace)
    if action == "explain":
        if not key:
            print("用法 / usage: config explain <key>")
            return 1
        return _explain(key, config_path, workspace)
    if action == "validate":
        return _validate(config_path, workspace)
    if action == "gen-docs":
        gen_docs()
        print("已生成配置参考文档 / reference docs generated")
        return 0
    print(f"未知子命令 / unknown action: {action}")
    return 1
```

> 说明:`--source` 标注来源层在本计划中保留接口位(`show_source` 参数已传入),实现可在 `_dump` 内对比 `_load_yaml_file`/`_env_overrides` 各层后标注。若实现者时间有限,先交付不带来源标注的 dump(参数透传保留),来源标注作为 `disposition: keep` 的增强项——但**不得**删除 `show_source` 形参,Task 6 会传它。

- [ ] **Step 4: 运行确认通过**

Run: `pytest tests/test_config_cmd.py -v`
Expected: PASS(10 passed)。`test_validate_warns_dead_field` 依赖 `storage.backend` 在 Task 2 已标 dead。

- [ ] **Step 5: ruff 与提交**

```bash
ruff check echo_agent/cli/config_cmd.py tests/test_config_cmd.py
git add echo_agent/cli/config_cmd.py tests/test_config_cmd.py
git commit -m "新增 config 命令:dump/explain/validate/gen-docs"
```

---

### Task 6: 在 __main__.py 注册 config 子命令

把 `config` 命令接进 CLI 分发,遵循现有 argparse 模式。

**Files:**
- Modify: `echo_agent/__main__.py`(`_build_parser` 加 parser,`_dispatch` 加分支)
- Test: `tests/test_config_cmd.py`(追加 CLI 解析测试)

**Interfaces:**
- Consumes: `run_config_command`(Task 5)。
- Produces: `echo-agent config <action> [key]` 可用。

- [ ] **Step 1: 追加 CLI 解析测试**

在 `tests/test_config_cmd.py` 末尾追加:

```python
def test_parser_accepts_config_subcommand():
    from echo_agent.__main__ import _build_parser
    parser = _build_parser()
    args = parser.parse_args(["config", "explain", "memory.enabled"])
    assert args.command == "config"
    assert args.action == "explain"
    assert args.key == "memory.enabled"


def test_parser_config_dump_format():
    from echo_agent.__main__ import _build_parser
    parser = _build_parser()
    args = parser.parse_args(["config", "dump", "--format", "json"])
    assert args.action == "dump"
    assert args.format == "json"
```

- [ ] **Step 2: 运行确认失败**

Run: `pytest tests/test_config_cmd.py::test_parser_accepts_config_subcommand -v`
Expected: FAIL — `config` 子命令未注册(argparse 报错或 args 无 action)

- [ ] **Step 3: 在 _build_parser 加 parser**

在 `evo_parser` 块之后、`# top-level flags` 之前插入:

```python
    # config
    config_parser = subparsers.add_parser("config", help="Inspect and validate configuration")
    config_parser.add_argument(
        "action",
        choices=["dump", "explain", "validate", "gen-docs"],
        help="Config action",
    )
    config_parser.add_argument("key", nargs="?", default="", help="Dotted config key (for explain)")
    config_parser.add_argument("--format", choices=["yaml", "json"], default="yaml",
                               help="Output format for dump (default: yaml)")
    config_parser.add_argument("--source", action="store_true", help="Annotate value source layer in dump")
    config_parser.add_argument("-c", "--config", help="Path to config file")
    config_parser.add_argument("-w", "--workspace", help="Workspace directory")
```

- [ ] **Step 4: 在 _dispatch 加分支**

在 `if args.command == "evolution":` 块之后插入:

```python
    if args.command == "config":
        from echo_agent.cli.config_cmd import run_config_command
        import sys as _sys
        rc = run_config_command(
            action=args.action,
            key=getattr(args, "key", "") or "",
            fmt=getattr(args, "format", "yaml"),
            show_source=getattr(args, "source", False),
            config_path=args.config or args.top_config,
            workspace=args.workspace or args.top_workspace,
        )
        _sys.exit(rc)
```

- [ ] **Step 5: 运行确认通过**

Run: `pytest tests/test_config_cmd.py -v`
Expected: PASS(全部,新增 2 个解析测试通过)

- [ ] **Step 6: 手动冒烟**

Run: `python -m echo_agent config explain compression.triggerRatio`
Expected: 打印该字段说明,含默认值 0.7;退出码 0

- [ ] **Step 7: ruff 与提交**

```bash
ruff check echo_agent/__main__.py tests/test_config_cmd.py
git add echo_agent/__main__.py tests/test_config_cmd.py
git commit -m "在 CLI 注册 config 子命令并分发"
```

---

### Task 7: 死字段 backlog 生成 + 文档产物提交 + 一致性 CI 测试

收尾:从元数据自动生成死字段 backlog,生成并提交四份参考文档,加一个 CI 测试断言"已提交的文档产物与当前 schema 一致"(防漂移)。

**Files:**
- Modify: `echo_agent/config/docgen.py`(加 `render_backlog`)
- Modify: `echo_agent/cli/config_cmd.py`(`gen_docs` 写出 backlog)
- Create(生成产物,提交进仓):`docs/config-reference.yaml`、`docs/config-reference.en.yaml`、`docs/config-reference.md`、`docs/config-reference.en.md`、`docs/config-dead-fields-backlog.md`
- Test: `tests/test_config_backlog.py`、`tests/test_docs_consistency.py`

**Interfaces:**
- Consumes: `iter_fields`(Task 1)、`render_yaml`/`render_markdown`(Task 4)。
- Produces: `render_backlog() -> str`(从 dead 字段元数据按 disposition 分组渲染 Markdown)。

- [ ] **Step 1: 写 backlog 渲染失败测试**

```python
# tests/test_config_backlog.py
"""Tests for dead-field backlog generation."""
from __future__ import annotations

from echo_agent.config.docgen import render_backlog


def test_backlog_groups_by_disposition():
    out = render_backlog()
    assert "fix" in out.lower()
    assert "remove" in out.lower()


def test_backlog_lists_known_dead_fields():
    out = render_backlog()
    assert "storage.backend" in out
    assert "reasoningEffort" in out or "reasoning_effort" in out
    # reason 文案出现
    assert "SQLiteBackend" in out


def test_backlog_excludes_effective_fields():
    out = render_backlog()
    assert "triggerRatio" not in out
```

- [ ] **Step 2: 运行确认失败**

Run: `pytest tests/test_config_backlog.py -v`
Expected: FAIL — `render_backlog` 不存在

- [ ] **Step 3: 实现 render_backlog(加到 docgen.py)**

```python
def render_backlog() -> str:
    from echo_agent.config.metadata import iter_fields
    groups: dict[str, list] = {"fix": [], "remove": [], "keep": []}
    for f in iter_fields(Config):
        if f.extra.get("status") != "dead":
            continue
        disp = f.extra.get("disposition", "keep")
        groups.setdefault(disp, []).append(f)
    titles = {
        "fix": "## fix —— 该接线的功能/真 bug(子项目 C 处理,安全相关走快车道)",
        "remove": "## remove —— 纯孤儿字段,建议删除",
        "keep": "## keep —— 有意保留",
    }
    lines = ["# 配置死字段处置 backlog(自动生成,请勿手改)", ""]
    for disp in ("fix", "remove", "keep"):
        infos = groups.get(disp) or []
        if not infos:
            continue
        lines.append(titles[disp])
        lines.append("")
        lines.append("| 字段(snake) | reason |")
        lines.append("|---|---|")
        for f in infos:
            lines.append(f"| `{f.snake_path}` | {f.extra.get('reason','')} |")
        lines.append("")
    return "\n".join(lines) + "\n"
```

并更新 `gen_docs` 末尾写出 backlog:

```python
    (base / "config-dead-fields-backlog.md").write_text(render_backlog(), encoding="utf-8")
```

- [ ] **Step 4: 运行 backlog 测试确认通过**

Run: `pytest tests/test_config_backlog.py -v`
Expected: PASS(3 passed)

- [ ] **Step 5: 生成文档产物**

Run: `python -m echo_agent config gen-docs`
Expected: `docs/` 下生成 5 个文件;打印"已生成配置参考文档"

- [ ] **Step 6: 写一致性 CI 测试**

```python
# tests/test_docs_consistency.py
"""CI guard: committed reference docs must match current schema metadata."""
from __future__ import annotations

from pathlib import Path

from echo_agent.config.docgen import render_backlog, render_markdown, render_yaml

_DOCS = Path(__file__).resolve().parent.parent / "docs"


def _check(name: str, rendered: str):
    path = _DOCS / name
    assert path.exists(), f"缺少生成产物 {name},请运行 `python -m echo_agent config gen-docs`"
    assert path.read_text(encoding="utf-8") == rendered, (
        f"{name} 与当前 schema 不一致,请重新运行 `python -m echo_agent config gen-docs` 并提交"
    )


def test_yaml_zh_consistent():
    _check("config-reference.yaml", render_yaml("zh"))


def test_yaml_en_consistent():
    _check("config-reference.en.yaml", render_yaml("en"))


def test_md_zh_consistent():
    _check("config-reference.md", render_markdown("zh"))


def test_md_en_consistent():
    _check("config-reference.en.md", render_markdown("en"))


def test_backlog_consistent():
    _check("config-dead-fields-backlog.md", render_backlog())
```

- [ ] **Step 7: 运行一致性测试确认通过**

Run: `pytest tests/test_docs_consistency.py -v`
Expected: PASS(5 passed)。若失败,重跑 gen-docs 再提交。

- [ ] **Step 8: 全量回归 + ruff**

Run: `pytest -q && ruff check .`
Expected: 全绿

- [ ] **Step 9: 提交**

```bash
git add echo_agent/config/docgen.py echo_agent/cli/config_cmd.py \
        docs/config-reference.yaml docs/config-reference.en.yaml \
        docs/config-reference.md docs/config-reference.en.md \
        docs/config-dead-fields-backlog.md \
        tests/test_config_backlog.py tests/test_docs_consistency.py
git commit -m "生成配置参考文档与死字段 backlog,加一致性守护测试"
```

---

## 实现完成后

全部 7 个任务完成后:
1. `pytest -q` 与 `ruff check .` 全绿。
2. 同步更新中英 README 的配置章节,指向 `docs/config-reference.md`(项目要求 PR 同步中英 README)。
3. 子项目 A 收尾,`docs/config-dead-fields-backlog.md` 作为子项目 C 的输入。
