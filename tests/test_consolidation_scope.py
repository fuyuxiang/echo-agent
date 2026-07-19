from __future__ import annotations

import inspect

import pytest

from echo_agent.memory import consolidator as cons_mod
from echo_agent.memory.consolidator import MemoryConsolidator
from echo_agent.memory.contradiction import ContradictionDetector
from echo_agent.memory.eligibility import Audience
from echo_agent.memory.service import MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.tiers import EpisodicManager, SemanticManager
from echo_agent.memory.types import MemoryEntry, MemoryType
from echo_agent.storage.sqlite import SQLiteBackend


class _FakeLLMResponse:
    def __init__(self, content: str = "", tool_calls: list | None = None):
        self.content = content
        self.tool_calls = tool_calls or []


class _FakeToolCall:
    def __init__(self, id: str, name: str, arguments):
        self.id = id
        self.name = name
        self.arguments = arguments


@pytest.mark.asyncio
async def test_step3_excludes_superseded_sibling_from_contradiction(tmp_path):
    """Critical 复现:同 key 改口后旧版本 superseded、新版本 active。整合器 Step 3
    的比较集合若含 superseded 兄弟,detector 会把 active 新版本与 superseded 旧版本
    判矛盾并 mark_contradiction_unresolved,使新 active 条目被 eligibility 判 UNRESOLVED,
    从 TOOL/snapshot/retrieval 静默剔除。修复后 superseded 兄弟不进比较集合,
    新 active 不被误标,仍可召回。"""
    storage = SQLiteBackend(tmp_path / "db.sqlite")
    await storage.initialize()
    store = MemoryStore(memory_dir=tmp_path / "mem", storage=storage)

    async def mock_llm(**kwargs):
        tools = kwargs.get("tools", [])
        if tools:
            name = tools[0]["function"]["name"]
            if name == "save_memory":
                return _FakeLLMResponse(tool_calls=[_FakeToolCall("1", "save_memory", {
                    "history_entry": "[2024-01-01] test", "memory_update": "# Memory",
                })])
            if name == "check_contradiction":
                # detector 若拿到 superseded 兄弟作候选,会走到这里判"矛盾"
                return _FakeLLMResponse(tool_calls=[_FakeToolCall("1", "check_contradiction", {
                    "is_contradictory": True, "explanation": "home changed",
                })])
            if name == "save_facts":
                return _FakeLLMResponse(tool_calls=[_FakeToolCall("1", "save_facts", {
                    "facts": [{"type": "user", "key": "home", "content": "上海", "importance": 0.8}],
                })])
            return _FakeLLMResponse(tool_calls=[_FakeToolCall("1", name, {})])
        return _FakeLLMResponse(content="summary")

    consolidator = MemoryConsolidator(store, mock_llm, consolidation_threshold=1)
    consolidator.set_episodic_manager(EpisodicManager(storage))
    consolidator.set_semantic_manager(SemanticManager(MemoryService(store)))
    # store 传入 detector,才能触发 mark_contradiction_unresolved(生产接线一致)
    consolidator.set_contradiction_detector(ContradictionDetector(storage, store=store))

    # 预置同 scope 同 key 的旧事实(consolidated 优先级 2,promote 的 consolidated
    # 优先级相同 → append_version:北京 → superseded,上海 → active)。
    store.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                          source="consolidated", importance=0.8, source_session="sess1"))

    await consolidator.sleep_consolidate("sess1", [
        {"role": "user", "content": "我搬到上海了", "timestamp": "2024-01-01T00:00"},
        {"role": "assistant", "content": "记下了", "timestamp": "2024-01-01T00:01"},
    ], memory_scope="sess1")

    home = [e for e in store._entries.values() if e.key == "home"]
    active = [e for e in home if not e.is_superseded]
    assert len(active) == 1 and active[0].content == "上海"
    new_active = active[0]
    # 核心断言:新 active 版本不得被标 UNRESOLVED
    assert not store.is_unresolved(new_active.id), "新 active 版本被误标 unresolved"
    # 且仍可被 TOOL 召回
    hits = store.search_scored("上海", session_key="sess1", audience=Audience.TOOL)
    assert any(e.id == new_active.id for e, _ in hits), "新 active 版本被静默剔除"


def test_consolidate_chunk_accepts_memory_scope():
    sig = inspect.signature(cons_mod.MemoryConsolidator.consolidate_chunk)
    assert "memory_scope" in sig.parameters


def test_consolidate_chunk_has_no_save_memory_llm_chain_source():
    # R3:consolidate_chunk 不再重写 MEMORY.md——不读旧 MD、不发 save_memory LLM 链、
    # 不写 HISTORY.md。MEMORY.md 由 promote 后 render 确定性重渲染。
    src = inspect.getsource(cons_mod.MemoryConsolidator.consolidate_chunk)
    assert "read_long_term" not in src
    assert "save_memory" not in src
    assert "append_history" not in src
    assert "write_long_term" not in src
    # 模块级 save_memory 工具定义已删除
    assert not hasattr(cons_mod, "_SAVE_MEMORY_TOOL")


@pytest.mark.asyncio
async def test_consolidate_no_save_memory_llm_chain(tmp_path):
    """砍链A后:consolidate_chunk 不再发起 save_memory 工具调用。
    计数桩 LLM 记录每次调用的工具名,断言全程无 save_memory 出现。"""
    storage = SQLiteBackend(tmp_path / "db.sqlite")
    await storage.initialize()
    store = MemoryStore(memory_dir=tmp_path / "mem", storage=storage)

    called_tools: list[str] = []

    async def counting_llm(**kwargs):
        for t in kwargs.get("tools", []) or []:
            called_tools.append(t["function"]["name"])
        return _FakeLLMResponse(content="summary")

    consolidator = MemoryConsolidator(store, counting_llm, consolidation_threshold=1)
    ok = await consolidator.consolidate_chunk([
        {"role": "user", "content": "我住在上海", "timestamp": "2024-01-01T00:00"},
        {"role": "assistant", "content": "记下了", "timestamp": "2024-01-01T00:01"},
    ], memory_scope="sess1")

    # 非空 chunk 保留成功信号(供 episode 门)
    assert ok is True
    # 关键:consolidate_chunk 不发起 save_memory 工具调用
    assert "save_memory" not in called_tools


@pytest.mark.asyncio
async def test_promote_then_deterministic_render(tmp_path):
    """sleep_consolidate promote 上海后,MEMORY.<scope>.md 由 render 确定性生成
    (含"上海"、幂等),而非 LLM 自由文本。断言 read_long_term(scope) 含 active 事实。"""
    storage = SQLiteBackend(tmp_path / "db.sqlite")
    await storage.initialize()
    store = MemoryStore(memory_dir=tmp_path / "mem", storage=storage)

    async def mock_llm(**kwargs):
        tools = kwargs.get("tools", [])
        if tools:
            name = tools[0]["function"]["name"]
            if name == "save_facts":
                return _FakeLLMResponse(tool_calls=[_FakeToolCall("1", "save_facts", {
                    "facts": [{"type": "user", "key": "home", "content": "上海", "importance": 0.8}],
                })])
            return _FakeLLMResponse(tool_calls=[_FakeToolCall("1", name, {})])
        return _FakeLLMResponse(content="summary")

    consolidator = MemoryConsolidator(store, mock_llm, consolidation_threshold=1)
    consolidator.set_episodic_manager(EpisodicManager(storage))
    consolidator.set_semantic_manager(SemanticManager(MemoryService(store)))

    await consolidator.sleep_consolidate("sess1", [
        {"role": "user", "content": "我搬到上海了", "timestamp": "2024-01-01T00:00"},
        {"role": "assistant", "content": "记下了", "timestamp": "2024-01-01T00:01"},
    ], memory_scope="sess1")

    rendered = store.read_long_term("sess1")
    assert "上海" in rendered
    assert "**home**" in rendered  # 确定性 render 结构,非 LLM 自由文本

    # 幂等:同一 store 状态再渲染一次内容不变
    from echo_agent.memory.render import render_memory_md
    again = render_memory_md(store.list_all(session_key="sess1"))
    assert again.strip() == rendered.strip()
