# 第三批半成品诚实化 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 echo-agent 四个半成品骨架"诚实化"——矛盾检测真正闭环(接线),inference 清理死方法/死字段,工作流与插件 sandbox 诚实标注其真实能力边界。

**Architecture:** 矛盾消解复用既有 `ContradictionDetector.resolve()→mark_superseded` 路径,在 sleep 整合中加受限自动消解(默认关闭),并在 memory 工具上加人工复核 action;其余三组件为清理与文档诚实化,不引入新行为。

**Tech Stack:** Python 3 (async)、loguru、pydantic(config schema)、pytest。

## Global Constraints

- 威胁模型:trusted-operator(操作者可信,不防恶意本地插件)。
- 不重造第一批已堵的"实现旁路"(工作流不接执行器)。
- 测试:全量 `python -m pytest tests/` 必须 0 fail;`ruff check .` 必须过;无新增依赖。
- 提交:master 直接提交;commit message 不带 `feat:`/`fix:` 等前缀,直接写改动描述;禁止任何 Claude/Anthropic 署名或 emoji。
- 设计依据:`docs/honest-capabilities-batch3-design-2026-06-24.md`。
- 现有受影响测试需同步更新:`tests/test_plugin_sandbox.py`、`tests/test_models_advanced.py`。

---

### Task 1: 新增 `memory.auto_resolve_contradictions` 配置开关

**Files:**
- Modify: `echo_agent/config/schema.py`（`MemoryConfig`，在 `contradiction_scan_on_store` 字段后，约 1905 行）
- Test: `tests/test_honest_capabilities_batch3.py`（新建）

**Interfaces:**
- Produces: `config.memory.auto_resolve_contradictions: bool`（默认 `False`）

- [ ] **Step 1: 写失败测试**

新建 `tests/test_honest_capabilities_batch3.py`：

```python
from echo_agent.config.schema import MemoryConfig


def test_auto_resolve_contradictions_defaults_false():
    cfg = MemoryConfig()
    assert cfg.auto_resolve_contradictions is False
```

- [ ] **Step 2: 运行测试确认失败**

Run: `python -m pytest tests/test_honest_capabilities_batch3.py::test_auto_resolve_contradictions_defaults_false -v`
Expected: FAIL — `AttributeError: 'MemoryConfig' object has no attribute 'auto_resolve_contradictions'`

- [ ] **Step 3: 加字段**

在 `echo_agent/config/schema.py` 的 `contradiction_scan_on_store` 字段定义之后插入：

```python
    auto_resolve_contradictions: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "memory/consolidator.py:auto_resolve",
            "desc_zh": "睡眠整合时自动消解同 key 矛盾(newest-wins),默认关闭只检测不消解",
            "desc_en": "Auto-resolve same-key contradictions (newest-wins) during sleep consolidation; off by default",
        },
    )
```

- [ ] **Step 4: 运行测试确认通过**

Run: `python -m pytest tests/test_honest_capabilities_batch3.py::test_auto_resolve_contradictions_defaults_false -v`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add echo_agent/config/schema.py tests/test_honest_capabilities_batch3.py
git commit -m "新增 memory.auto_resolve_contradictions 配置开关(默认关闭)"
```

---

### Task 2: 在 sleep 整合中加受限自动消解

**Files:**
- Modify: `echo_agent/memory/consolidator.py`（`__init__` 加标志位 + setter；`sleep_consolidate` Step 3 块，约 228-241 行）
- Modify: `echo_agent/agent/loop.py:421-424`（注入开关）
- Test: `tests/test_honest_capabilities_batch3.py`

**Interfaces:**
- Consumes: `ContradictionDetector.resolve(contradiction_id: str, resolution: str, winner_id: str | None)`（已存在，`contradiction.py:158`）；`Contradiction(id, memory_id_a, memory_id_b, description, resolution, resolved_at, created_at)`；`MemoryEntry.updated_at: str` / `created_at: str` / `key: str`。
- Produces: `MemoryConsolidator.set_auto_resolve_contradictions(enabled: bool)`；`sleep_consolidate` 返回 stats 新增键 `"resolved": int`。

- [ ] **Step 1: 写失败测试**

向 `tests/test_honest_capabilities_batch3.py` 追加。该测试构造两条同 key、不同内容、`updated_at` 不同的 promoted 记忆，断言开关开启时较旧的一条被 supersede：

```python
import pytest
from echo_agent.memory.consolidator import MemoryConsolidator
from echo_agent.memory.contradiction import ContradictionDetector
from echo_agent.memory.types import MemoryEntry, MemoryType, Contradiction


class _FakeStore:
    def __init__(self, entries):
        self._entries = {e.id: e for e in entries}
        self.superseded = []
        self.versions = {}

    def mark_superseded(self, entry_id, superseded_by):
        self.superseded.append((entry_id, superseded_by))
        if entry_id in self._entries:
            self._entries[entry_id].superseded_by = superseded_by
        return True

    def set_version(self, entry_id, version):
        self.versions[entry_id] = version
        return True


@pytest.mark.asyncio
async def test_auto_resolve_supersedes_older_same_key(monkeypatch):
    old = MemoryEntry(id="old1", key="pref:lang", content="Python",
                      created_at="2026-06-01T00:00:00", updated_at="2026-06-01T00:00:00")
    new = MemoryEntry(id="new1", key="pref:lang", content="Rust",
                      created_at="2026-06-02T00:00:00", updated_at="2026-06-02T00:00:00")
    store = _FakeStore([old, new])

    detector = ContradictionDetector(storage=_StubStorage(), store=store)
    consolidator = _make_consolidator(store)
    consolidator.set_contradiction_detector(detector)
    consolidator.set_auto_resolve_contradictions(True)

    stats = await consolidator._auto_resolve_same_key([new], [old, new])
    assert ("old1", "new1") in store.superseded
    assert stats == 1
```

辅助桩(同文件)：

```python
class _StubStorage:
    async def execute_sql(self, *a, **k):
        return None

    async def fetch_sql(self, *a, **k):
        # 真实 check_lightweight_sync(new, [old]) 产生 memory_id_a=new, memory_id_b=old
        return [{"memory_id_a": "new1", "memory_id_b": "old1"}]


def _make_consolidator(store):
    async def _noop_llm(**kwargs):
        raise AssertionError("LLM must not be called in auto-resolve path")
    c = MemoryConsolidator(memory_store=store, llm_call=_noop_llm)
    return c
```

- [ ] **Step 2: 运行测试确认失败**

Run: `python -m pytest tests/test_honest_capabilities_batch3.py::test_auto_resolve_supersedes_older_same_key -v`
Expected: FAIL — `AttributeError: ... has no attribute 'set_auto_resolve_contradictions'`（或 `_auto_resolve_same_key`）

- [ ] **Step 3: 在 consolidator `__init__` 加标志位与 setter**

`echo_agent/memory/consolidator.py` 的 `__init__` 末尾(`self._embed_fn = None` 之后)加：

```python
        self._auto_resolve_contradictions = False
```

在 `set_embed_fn` 附近加 setter：

```python
    def set_auto_resolve_contradictions(self, enabled: bool):
        self._auto_resolve_contradictions = enabled
```

- [ ] **Step 4: 实现 `_auto_resolve_same_key`**

在 `MemoryConsolidator` 中新增方法。只处理同 key 冲突,winner 取 `updated_at`(回退 `created_at`)较新者:

```python
    async def _auto_resolve_same_key(self, promoted: list, all_entries: list) -> int:
        """Auto-resolve only same-key content conflicts via newest-wins.

        Deliberately narrow: LLM/temporal/cross-prefix contradictions are left
        for human review. Returns count resolved.
        """
        if not self._contradiction_detector:
            return 0
        resolved = 0
        for new_entry in promoted:
            if not new_entry.key:
                continue
            for other in all_entries:
                if other.id == new_entry.id or other.key != new_entry.key:
                    continue
                if other.content.strip() == new_entry.content.strip():
                    continue
                if other.is_superseded or new_entry.is_superseded:
                    continue
                winner, _loser = self._newest_wins(new_entry, other)
                # check_lightweight_sync(new, [other]) yields memory_id_a=new_entry
                contradictions = self._contradiction_detector.check_lightweight_sync(
                    new_entry, [other]
                )
                for c in contradictions:
                    await self._contradiction_detector.store_contradiction(c)
                    resolution = "a_wins" if winner.id == c.memory_id_a else "b_wins"
                    await self._contradiction_detector.resolve(
                        c.id, resolution, winner_id=winner.id
                    )
                    resolved += 1
        return resolved

    @staticmethod
    def _newest_wins(a, b):
        def _ts(e):
            return e.updated_at or e.created_at or ""
        return (a, b) if _ts(a) >= _ts(b) else (b, a)
```

- [ ] **Step 5: 在 `sleep_consolidate` Step 3 之后调用**

在 `sleep_consolidate` 的 Step 3 块(约 228-241 行)后、Step 4 之前插入：

```python
        # Step 3b: Conservative auto-resolution (same-key newest-wins only).
        if self._auto_resolve_contradictions and self._contradiction_detector and promoted:
            try:
                all_entries = list(self.store._entries.values())
                stats["resolved"] = await self._auto_resolve_same_key(promoted, all_entries)
            except Exception as e:
                logger.warning("Auto-resolution failed: {}", e)
```

并把 stats 初始化(约 191 行)加上 `"resolved": 0`：

```python
        stats = {"episodes": 0, "promoted": 0, "contradictions": 0, "resolved": 0, "archived": 0, "forgotten": 0}
```

- [ ] **Step 6: 在 loop 注入开关**

`echo_agent/agent/loop.py` 第 421-424 块改为：

```python
        if config.memory.contradiction_detection and storage:
            from echo_agent.memory.contradiction import ContradictionDetector
            detector = ContradictionDetector(storage, vector_index, store=self.memory)
            self.consolidator.set_contradiction_detector(detector)
            self.consolidator.set_auto_resolve_contradictions(
                config.memory.auto_resolve_contradictions
            )
```

- [ ] **Step 7: 运行测试确认通过**

Run: `python -m pytest tests/test_honest_capabilities_batch3.py::test_auto_resolve_supersedes_older_same_key -v`
Expected: PASS

- [ ] **Step 8: 加护栏测试(开关关 / 跨 key 不消解)并确认通过**

向同文件追加：

```python
@pytest.mark.asyncio
async def test_auto_resolve_disabled_by_default():
    old = MemoryEntry(id="o", key="pref:lang", content="Python", updated_at="2026-06-01T00:00:00")
    new = MemoryEntry(id="n", key="pref:lang", content="Rust", updated_at="2026-06-02T00:00:00")
    store = _FakeStore([old, new])
    detector = ContradictionDetector(storage=_StubStorage(), store=store)
    consolidator = _make_consolidator(store)
    consolidator.set_contradiction_detector(detector)
    # auto_resolve left at default False
    assert consolidator._auto_resolve_contradictions is False


@pytest.mark.asyncio
async def test_auto_resolve_skips_different_key():
    a = MemoryEntry(id="a", key="pref:lang", content="Python", updated_at="2026-06-01T00:00:00")
    b = MemoryEntry(id="b", key="pref:editor", content="vim", updated_at="2026-06-02T00:00:00")
    store = _FakeStore([a, b])
    detector = ContradictionDetector(storage=_StubStorage(), store=store)
    consolidator = _make_consolidator(store)
    consolidator.set_contradiction_detector(detector)
    consolidator.set_auto_resolve_contradictions(True)
    resolved = await consolidator._auto_resolve_same_key([b], [a, b])
    assert resolved == 0
    assert store.superseded == []
```

Run: `python -m pytest tests/test_honest_capabilities_batch3.py -v`
Expected: 3 个矛盾相关测试全 PASS

- [ ] **Step 9: 提交**

```bash
git add echo_agent/memory/consolidator.py echo_agent/agent/loop.py tests/test_honest_capabilities_batch3.py
git commit -m "睡眠整合加受限自动消解:仅同key newest-wins,复用 resolve→mark_superseded,默认关闭"
```

---

### Task 3: memory 工具加 `list_contradictions` / `resolve_contradiction` 人工复核 action

**Files:**
- Modify: `echo_agent/agent/tools/memory.py`（`MemoryTool`：构造、enum、参数、dispatch、两个新方法）
- Modify: `echo_agent/agent/tools/__init__.py:30,143-145`（`discover_tools` 增 `contradiction_detector` 形参并注入 MemoryTool）
- Modify: `echo_agent/agent/loop.py`（保存 detector 到 self；`_register_tools`/`discover_tools` 调用处传入）
- Test: `tests/test_honest_capabilities_batch3.py`

**Interfaces:**
- Consumes: `ContradictionDetector.get_unresolved(limit: int) -> list[Contradiction]`；`ContradictionDetector.resolve(contradiction_id, resolution, winner_id)`。
- Produces: `MemoryTool(store, contradiction_detector=None)`；新 action `list_contradictions`、`resolve_contradiction`（参数 `contradiction_id`、`winner_id`）。

- [ ] **Step 1: 写失败测试**

```python
@pytest.mark.asyncio
async def test_memory_tool_lists_and_resolves_contradictions():
    from echo_agent.agent.tools.memory import MemoryTool

    class _Detector:
        def __init__(self):
            self.resolved = []
        async def get_unresolved(self, limit=10):
            return [Contradiction(id="c1", memory_id_a="a", memory_id_b="b",
                                  description="Key 'pref:lang' conflict")]
        async def resolve(self, cid, resolution, winner_id=None):
            self.resolved.append((cid, resolution, winner_id))

    det = _Detector()
    tool = MemoryTool(store=_FakeStore([]), contradiction_detector=det)

    listed = await tool.execute({"action": "list_contradictions"})
    assert "c1" in listed.output

    done = await tool.execute({"action": "resolve_contradiction",
                               "contradiction_id": "c1", "winner_id": "a"})
    assert done.success
    assert ("c1", "a_wins", "a") in det.resolved
```

- [ ] **Step 2: 运行测试确认失败**

Run: `python -m pytest tests/test_honest_capabilities_batch3.py::test_memory_tool_lists_and_resolves_contradictions -v`
Expected: FAIL — `TypeError: __init__() got an unexpected keyword argument 'contradiction_detector'`

- [ ] **Step 3: 改 MemoryTool 构造与 schema**

`echo_agent/agent/tools/memory.py`：

构造函数：

```python
    def __init__(self, store: MemoryStore, contradiction_detector: Any = None):
        self._store = store
        self._contradiction_detector = contradiction_detector
```

action enum（第 28 行）追加两项：

```python
                "enum": ["add", "replace", "remove", "search", "list",
                         "list_contradictions", "resolve_contradiction"],
```

parameters 的 properties 追加两个字段：

```python
            "contradiction_id": {
                "type": "string",
                "description": "Contradiction id (for resolve_contradiction)",
            },
            "winner_id": {
                "type": "string",
                "description": "Memory id that wins (for resolve_contradiction)",
            },
```

description 末尾补一句：

```python
        "list_contradictions (show unresolved memory conflicts for review), "
        "resolve_contradiction (pick winner_id to supersede the loser)."
```

- [ ] **Step 4: 加 dispatch 分支与方法**

`execute` 的 dispatch 链(约 113 行 `list` 分支后)加：

```python
        elif action == "list_contradictions":
            return await self._list_contradictions()
        elif action == "resolve_contradiction":
            return await self._resolve_contradiction(params)
```

新增两个方法：

```python
    async def _list_contradictions(self) -> ToolResult:
        if self._contradiction_detector is None:
            return ToolResult(success=False, error="Contradiction detection is disabled.")
        items = await self._contradiction_detector.get_unresolved(limit=20)
        if not items:
            return ToolResult(success=True, output="No unresolved contradictions.")
        lines = [
            f"- {c.id}: {c.description} (a={c.memory_id_a}, b={c.memory_id_b})"
            for c in items
        ]
        return ToolResult(success=True, output="\n".join(lines))

    async def _resolve_contradiction(self, params: dict[str, Any]) -> ToolResult:
        if self._contradiction_detector is None:
            return ToolResult(success=False, error="Contradiction detection is disabled.")
        cid = params.get("contradiction_id", "")
        winner_id = params.get("winner_id", "")
        if not cid or not winner_id:
            return ToolResult(success=False, error="contradiction_id and winner_id are required")
        unresolved = {c.id: c for c in await self._contradiction_detector.get_unresolved(limit=100)}
        c = unresolved.get(cid)
        if c is None:
            return ToolResult(success=False, error=f"No unresolved contradiction '{cid}'")
        if winner_id not in (c.memory_id_a, c.memory_id_b):
            return ToolResult(success=False, error="winner_id must be memory_id_a or memory_id_b")
        resolution = "a_wins" if winner_id == c.memory_id_a else "b_wins"
        await self._contradiction_detector.resolve(cid, resolution, winner_id=winner_id)
        return ToolResult(success=True, output=f"Resolved {cid}: {resolution}")
```

确保文件顶部已 `from typing import Any`（已存在）。

- [ ] **Step 5: 在 discover_tools 注入 detector**

`echo_agent/agent/tools/__init__.py`：`discover_tools` 形参加 `contradiction_detector: Any = None,`（紧随 `memory_store` 之后,第 28 行附近）；第 143-145 块改为：

```python
    if memory_store:
        from echo_agent.agent.tools.memory import MemoryTool
        tools.append(MemoryTool(store=memory_store, contradiction_detector=contradiction_detector))
```

- [ ] **Step 6: 在 loop 保存 detector 并传入**

`echo_agent/agent/loop.py`：在第 421-424 的 detector 构造块里,把 detector 存到实例：

```python
        self._contradiction_detector = None
        if config.memory.contradiction_detection and storage:
            from echo_agent.memory.contradiction import ContradictionDetector
            detector = ContradictionDetector(storage, vector_index, store=self.memory)
            self._contradiction_detector = detector
            self.consolidator.set_contradiction_detector(detector)
            self.consolidator.set_auto_resolve_contradictions(
                config.memory.auto_resolve_contradictions
            )
```

`_register_tools` 中 `discover_tools(...)` 调用加一行参数：

```python
            contradiction_detector=getattr(self, "_contradiction_detector", None),
```

注意:`_init_advanced_memory`(loop.py:197,含上面 421 块)在 `_register_tools`(loop.py:261)之前执行,detector 在注册工具时已就绪,`getattr` 能取到。

- [ ] **Step 7: 运行测试确认通过**

Run: `python -m pytest tests/test_honest_capabilities_batch3.py::test_memory_tool_lists_and_resolves_contradictions -v`
Expected: PASS

- [ ] **Step 8: 提交**

```bash
git add echo_agent/agent/tools/memory.py echo_agent/agent/tools/__init__.py echo_agent/agent/loop.py tests/test_honest_capabilities_batch3.py
git commit -m "memory 工具加 list_contradictions/resolve_contradiction 人工复核 action"
```

---

### Task 4: InferenceController 清理(删孤儿方法 + 死字段)

**Files:**
- Modify: `echo_agent/models/inference.py`（删 `check_hallucination_markers`、`build_verification_prompt`、`layer_system_prompts`、字段 `max_output_tokens`；`validate_response` docstring）
- Modify: `tests/test_models_advanced.py`（删引用被删成员的测试）
- Test: `tests/test_honest_capabilities_batch3.py`

**Interfaces:**
- Produces: `InferenceController` 不再有 `check_hallucination_markers`/`build_verification_prompt`/`layer_system_prompts`；`InferenceConstraints` 不再有 `max_output_tokens`。保留:`filter_tools`、`validate_response`、`needs_confirmation`、`set_constraints`。

- [ ] **Step 1: 写"成员已移除"断言测试**

向 `tests/test_honest_capabilities_batch3.py` 追加：

```python
def test_inference_orphans_removed():
    from echo_agent.models.inference import InferenceController, InferenceConstraints
    ctrl = InferenceController()
    assert not hasattr(ctrl, "check_hallucination_markers")
    assert not hasattr(ctrl, "build_verification_prompt")
    assert not hasattr(ctrl, "layer_system_prompts")
    assert "max_output_tokens" not in InferenceConstraints.__dataclass_fields__
```

- [ ] **Step 2: 运行确认失败**

Run: `python -m pytest tests/test_honest_capabilities_batch3.py::test_inference_orphans_removed -v`
Expected: FAIL（成员仍存在）

- [ ] **Step 3: 删除 inference.py 中的三个方法与字段**

`echo_agent/models/inference.py`：
- 删 `InferenceConstraints` 的 `max_output_tokens: int = 4096` 行（第 23 行）。
- 删方法 `check_hallucination_markers`（71-84 行）、`build_verification_prompt`（89-95 行）、`layer_system_prompts`（97-100 行）。
- 同步删文件顶部 `import re`（仅这些方法用到，删后确认 `validate_response` 不再用 `re`——它用的是 `json.loads`，不用 re；用 `grep -n "re\." echo_agent/models/inference.py` 核对无残留再删 import）。
- `validate_response` docstring 改为明确 advisory：

```python
    def validate_response(self, response: LLMResponse) -> list[str]:
        """Advisory soft-check: returns a list of issue strings for logging.

        This NEVER blocks or mutates the response — callers (inference_stage)
        only log the issues. Tool allow/block enforcement happens in
        filter_tools(); this is observability, not a gate.
        """
```

- [ ] **Step 4: 清理 test_models_advanced.py 中失效测试**

删除引用被删成员的测试:
- `tests/test_models_advanced.py:143` 处断言 `c.max_output_tokens == 4096` 的测试方法（整方法删除）。
- 两处 `check_hallucination_markers` 测试（228、233 行所在方法）。
- `test_build_verification_prompt_format`（246 行）。
- `test_layer_system_prompts_skips_empty`（255 行）。

用 `python -m pytest tests/test_models_advanced.py -v` 确认无残留对这些成员的引用(收集期不报 AttributeError/import 错误)。

- [ ] **Step 5: 运行确认通过**

Run: `python -m pytest tests/test_honest_capabilities_batch3.py::test_inference_orphans_removed tests/test_models_advanced.py -v`
Expected: 全 PASS

- [ ] **Step 6: 提交**

```bash
git add echo_agent/models/inference.py tests/test_models_advanced.py tests/test_honest_capabilities_batch3.py
git commit -m "清理 InferenceController:删3个零调用方孤儿方法与死字段 max_output_tokens,validate_response 标注 advisory"
```

---

### Task 5: WorkflowEngine 诚实化(标注仅编排)

**Files:**
- Modify: `echo_agent/agent/tools/workflow.py`（`WorkflowTool.description`、`advance` action 描述）
- Modify: `echo_agent/tasks/workflow.py`（`WorkflowEngine` 类 docstring）
- Test: `tests/test_honest_capabilities_batch3.py`

**Interfaces:**
- Produces: 无签名变化,仅文档/描述文本。

- [ ] **Step 1: 写描述断言测试**

```python
def test_workflow_tool_describes_orchestration_only():
    from echo_agent.agent.tools.workflow import WorkflowTool
    desc = WorkflowTool.description.lower()
    assert "orchestrat" in desc
    # 必须诚实声明不自动执行 step 工具
    assert "does not execute" in desc or "external" in desc
```

- [ ] **Step 2: 运行确认失败**

Run: `python -m pytest tests/test_honest_capabilities_batch3.py::test_workflow_tool_describes_orchestration_only -v`
Expected: FAIL

- [ ] **Step 3: 改 WorkflowTool.description**

`echo_agent/agent/tools/workflow.py` 第 14-17 行：

```python
    description = (
        "Orchestrate multi-step workflows with DAG-based step dependencies. "
        "This engine ONLY orchestrates step state and dependency resolution — it "
        "does NOT execute the steps' tools itself. Step execution must be driven "
        "externally by calling 'advance'. "
        "Actions: create, start, status, advance, pause, resume, cancel, list."
    )
```

`advance` action 的 enum 描述无需单列,但在 parameters 的 `action` description 后补充说明（第 24 行 `"description": "Action to perform"` 改）：

```python
                "description": "Action to perform. 'advance' must be driven externally to progress steps.",
```

- [ ] **Step 4: 改 WorkflowEngine docstring**

`echo_agent/tasks/workflow.py` 第 22-23 行类 docstring：

```python
class WorkflowEngine:
    """Manages workflow lifecycle and DAG step resolution.

    Orchestration only: this engine creates and tracks step tasks and resolves
    DAG dependencies, but does NOT execute the steps' tools. TaskManager has no
    executor — queued step tasks carry tool_name/tool_params in metadata for an
    external driver to run, then call advance() to progress the workflow.
    """
```

- [ ] **Step 5: 运行确认通过**

Run: `python -m pytest tests/test_honest_capabilities_batch3.py::test_workflow_tool_describes_orchestration_only -v`
Expected: PASS

- [ ] **Step 6: 提交**

```bash
git add echo_agent/agent/tools/workflow.py echo_agent/tasks/workflow.py tests/test_honest_capabilities_batch3.py
git commit -m "工作流诚实化:WorkflowTool/引擎 docstring 标注仅编排不执行,执行需外部驱动 advance"
```

---

### Task 6: PluginSandbox 降级(删运行时不可强制的 check 方法 + 标注 advisory)

**Files:**
- Modify: `echo_agent/plugins/sandbox.py`（删 4 个 typed check 方法；模块/类 docstring 标注 advisory）
- Modify: `tests/test_plugin_sandbox.py`（被删方法的调用改为通用 `check_permission`）
- Test: `tests/test_honest_capabilities_batch3.py`

**Interfaces:**
- Produces: `PluginSandbox` 删 `check_network`/`check_subprocess`/`check_filesystem_read`/`check_filesystem_write`。保留 `check_permission(required: str) -> bool`、`check_tool_register`、`check_hook_register`、`violations`、`is_legacy`。`VALID_PERMISSIONS` 不变(声明值保留),但语义降级为 advisory。

- [ ] **Step 1: 写"方法已移除 + check_permission 仍可用"测试**

向 `tests/test_honest_capabilities_batch3.py` 追加：

```python
def test_sandbox_runtime_typed_checks_removed():
    from echo_agent.plugins.sandbox import PluginSandbox
    from echo_agent.plugins.manifest import PluginManifest
    sb = PluginSandbox("t", PluginManifest(name="t", permissions=["network"]), trusted=False)
    # typed runtime wrappers gone
    assert not hasattr(sb, "check_network")
    assert not hasattr(sb, "check_subprocess")
    assert not hasattr(sb, "check_filesystem_read")
    assert not hasattr(sb, "check_filesystem_write")
    # generic advisory check still works for declaration introspection
    assert sb.check_permission("network") is True
    assert sb.check_permission("subprocess") is False
    # registration-time enforcement preserved
    assert hasattr(sb, "check_tool_register")
    assert hasattr(sb, "check_hook_register")
```

- [ ] **Step 2: 运行确认失败**

Run: `python -m pytest tests/test_honest_capabilities_batch3.py::test_sandbox_runtime_typed_checks_removed -v`
Expected: FAIL（方法仍存在）

- [ ] **Step 3: 删 sandbox.py 中 4 个 typed 方法 + 改 docstring**

`echo_agent/plugins/sandbox.py`：
- 删 `check_network`（104-105 行）、`check_filesystem_write`（107-108 行）、`check_filesystem_read`（110-111 行）、`check_subprocess`（113-114 行）。保留 `check_permission`、`check_tool_register`、`check_hook_register`。
- 模块顶部 docstring 改为诚实声明 advisory 性质:

```python
"""Plugin sandbox — declaration tracking for plugin permissions.

trusted-operator threat model: plugins run in-process and are trusted. This
class is NOT a security boundary. tool.register / hook.register are enforced
at registration time (plugins/manager.py). network / subprocess / filesystem.*
are advisory manifest metadata only — they document intent and are surfaced
via check_permission() for introspection, but are NOT enforced at runtime
(in-process Python cannot constrain a plugin that does not voluntarily ask).
"""
```

- 在 `VALID_PERMISSIONS` 定义上方加注释：

```python
# network / subprocess / filesystem.* are advisory (declaration-only), NOT
# runtime-enforced. tool.register / hook.register are enforced at registration.
```

- [ ] **Step 4: 改 test_plugin_sandbox.py 用 check_permission**

把被删方法的调用逐处替换为 `check_permission(...)`：

- 第 21 行 `sandbox.check_network()` → `sandbox.check_permission("network")`
- 第 29-31 行 → `check_permission("network")` / `check_permission("subprocess")` / `check_permission("filesystem.write")`
- 第 39、45、46、53、68 行 `check_network()` → `check_permission("network")`
- 第 73-74 行 `check_network()`/`check_subprocess()` → `check_permission("network")`/`check_permission("subprocess")`
- 第 82-83 行 `check_filesystem_read()`/`check_filesystem_write()` → `check_permission("filesystem.read")`/`check_permission("filesystem.write")`
- `check_tool_register`/`check_hook_register` 保持不变。

- [ ] **Step 5: 运行确认通过**

Run: `python -m pytest tests/test_honest_capabilities_batch3.py::test_sandbox_runtime_typed_checks_removed tests/test_plugin_sandbox.py -v`
Expected: 全 PASS

- [ ] **Step 6: 提交**

```bash
git add echo_agent/plugins/sandbox.py tests/test_plugin_sandbox.py tests/test_honest_capabilities_batch3.py
git commit -m "PluginSandbox 降级:删运行时不可强制的 typed check 方法,标注 advisory 非安全边界,保留注册期强制"
```

---

### Task 7: 全量验证与收尾

**Files:** 无新增,仅验证。

- [ ] **Step 1: 全量测试**

Run: `python -m pytest tests/ -q`
Expected: 0 failed（记录通过数；如有失败,定位是否由本批改动引入并修复）

- [ ] **Step 2: lint**

Run: `ruff check .`
Expected: 无错误（如有,修复 import 残留等）

- [ ] **Step 3: 确认无被删成员的残留引用**

Run:
```bash
grep -rn "check_network\|check_subprocess\|check_filesystem_read\|check_filesystem_write" echo_agent/ tests/ | grep -v check_permission
grep -rn "check_hallucination_markers\|build_verification_prompt\|layer_system_prompts\|max_output_tokens" echo_agent/ tests/
```
Expected: 两条命令均无输出（`max_output_tokens` 在其它配置类如 RouterConfig 可能仍有——只确认 `InferenceConstraints` 相关无残留,其余忽略）

- [ ] **Step 4: 确认无新增依赖**

Run: `git diff master --stat -- pyproject.toml requirements*.txt 2>/dev/null; git status`
Expected: 依赖文件无改动

- [ ] **Step 5: 收尾提交(若 Step 1-3 有修复)**

```bash
git add -A
git commit -m "第三批诚实化:全量测试与 lint 收尾"
```

若 Step 1-3 无需修复,跳过本步。





