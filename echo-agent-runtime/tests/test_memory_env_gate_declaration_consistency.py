"""ENV 门禁与「暴露给模型的能力声明」一致性回归。

线上症状:微信通道收到「环境记忆写入被禁用了,跳过存档」。根因不是门禁本身,
而是三处声明层无条件把 environment 摆上菜单——系统提示词、memory 工具 schema、
reviewer/consolidator 的函数声明——而 MemoryService 的 ENV 门禁默认关闭。
模型按提示词写 ENV → 必被拒 → 把英文错误翻译成中文发给用户。

本文件锁定:门禁关闭时,任何交给模型的声明都不得出现 environment 选项。
"""

from __future__ import annotations

from echo_agent.agent.context import (
    _MEMORY_GUIDANCE_TARGETS_BOTH,
    _MEMORY_GUIDANCE_TARGETS_USER_ONLY,
    build_memory_context,
    build_memory_guidance,
)
from echo_agent.memory.consolidator import _build_extract_facts_tool
from echo_agent.memory.reviewer import _build_review_prompt, _build_tool_defs


# ── 系统提示词 ───────────────────────────────────────────────────────────────

def test_guidance_omits_environment_when_gate_closed():
    text = build_memory_guidance(allow_env_writes=False)
    assert _MEMORY_GUIDANCE_TARGETS_USER_ONLY in text
    assert _MEMORY_GUIDANCE_TARGETS_BOTH not in text
    assert 'as "environment" memories' not in text


def test_guidance_includes_environment_when_gate_open():
    text = build_memory_guidance(allow_env_writes=True)
    assert _MEMORY_GUIDANCE_TARGETS_BOTH in text
    assert _MEMORY_GUIDANCE_TARGETS_USER_ONLY not in text


def test_guidance_keeps_shared_tail_in_both_modes():
    """收窄只替换 target 分类段,search/replace/记住 等其余引导不得丢。"""
    for allow in (True, False):
        text = build_memory_guidance(allow_env_writes=allow)
        assert "Use `search` to check if relevant memories exist" in text
        assert "SELF-AWARENESS" in text
        assert "CRITICAL" in text


def test_memory_context_propagates_gate_to_guidance():
    closed = build_memory_context(memory_store=None, allow_env_writes=False)
    assert 'as "environment" memories' not in closed
    opened = build_memory_context(memory_store=None, allow_env_writes=True)
    assert 'as "environment" memories' in opened


def test_memory_context_still_carries_snapshot_when_gate_closed():
    text = build_memory_context(
        memory_store=None, snapshot="snap-data", allow_env_writes=False
    )
    assert "snap-data" in text


# ── reviewer 声明 ────────────────────────────────────────────────────────────

def test_reviewer_tool_def_hides_environment_when_gate_closed():
    target = _build_tool_defs(False)[0]["function"]["parameters"]["properties"]["target"]
    assert target["enum"] == ["user"]


def test_reviewer_tool_def_exposes_environment_when_gate_open():
    target = _build_tool_defs(True)[0]["function"]["parameters"]["properties"]["target"]
    assert target["enum"] == ["user", "environment"]


def test_reviewer_prompt_omits_environment_when_gate_closed():
    text = _build_review_prompt(False)
    assert 'save as "environment" type' not in text
    assert 'target="user"' in text
    # 收窄不得吃掉后半段 guidelines。
    assert "Only save information that would be useful" in text
    assert "No memory changes needed." in text


def test_reviewer_prompt_includes_environment_when_gate_open():
    text = _build_review_prompt(True)
    assert 'save as "environment" type' in text
    assert "No memory changes needed." in text


# ── consolidator 声明 ────────────────────────────────────────────────────────

def _fact_type_prop(allow: bool):
    fn = _build_extract_facts_tool(allow)[0]["function"]
    return fn["parameters"]["properties"]["facts"]["items"]["properties"]["type"]


def test_extract_facts_hides_environment_when_gate_closed():
    assert _fact_type_prop(False)["enum"] == ["user"]


def test_extract_facts_exposes_environment_when_gate_open():
    assert _fact_type_prop(True)["enum"] == ["user", "environment"]


def test_extract_facts_keeps_required_fields_in_both_modes():
    for allow in (True, False):
        fn = _build_extract_facts_tool(allow)[0]["function"]
        items = fn["parameters"]["properties"]["facts"]["items"]
        assert items["required"] == ["key", "content"]
        assert fn["name"] == "save_facts"


# ── 实配线:门禁状态确实从 service 传到声明层 ────────────────────────────────
# 上面各测试直接调 _build_* 工厂;这里验证 reviewer/consolidator 真的从
# service 读到了门禁状态,而不是构造完仍用门禁开放态的模块级常量。

def test_reviewer_reads_gate_from_service(tmp_path):
    from echo_agent.memory.reviewer import MemoryReviewer
    from echo_agent.memory.service import MemoryService
    from echo_agent.memory.store import MemoryStore

    def _reviewer(allow):
        store = MemoryStore(memory_dir=tmp_path / f"mem-{allow}")
        service = MemoryService(store, allow_env_writes=allow)
        return MemoryReviewer(provider=object(), service=service, session_key="s")

    closed = _reviewer(False)
    assert closed._tool_defs[0]["function"]["parameters"]["properties"]["target"]["enum"] == ["user"]
    assert 'save as "environment" type' not in closed._review_prompt

    opened = _reviewer(True)
    assert opened._tool_defs[0]["function"]["parameters"]["properties"]["target"]["enum"] == [
        "user", "environment",
    ]
    assert 'save as "environment" type' in opened._review_prompt


def test_consolidator_reads_gate_through_semantic_manager(tmp_path):
    from echo_agent.memory.consolidator import MemoryConsolidator
    from echo_agent.memory.service import MemoryService
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.tiers import SemanticManager

    async def _llm(**_kwargs):  # pragma: no cover - 不触发
        raise AssertionError("not called")

    def _type_enum(allow):
        store = MemoryStore(memory_dir=tmp_path / f"mem-{allow}")
        service = MemoryService(store, allow_env_writes=allow)
        consolidator = MemoryConsolidator(memory_store=store, llm_call=_llm)
        consolidator.set_semantic_manager(SemanticManager(service))
        fn = consolidator._extract_facts_tool[0]["function"]
        return fn["parameters"]["properties"]["facts"]["items"]["properties"]["type"]["enum"]

    assert _type_enum(False) == ["user"]
    assert _type_enum(True) == ["user", "environment"]


def test_discover_tools_default_config_hides_environment(tmp_path):
    """默认配置(allow_model_environment_writes=False)下,discover_tools 装出来的
    memory 工具交给模型的 schema 里不得有 environment——这是线上那条微信告警的
    直接成因,故在真实装配路径上钉住。"""
    from echo_agent.agent.tools import discover_tools
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import Config
    from echo_agent.memory.store import MemoryStore

    config = Config()
    assert config.memory.allow_model_environment_writes is False, "本测试以默认关闭为前提"

    tools = discover_tools(
        config=config,
        workspace=tmp_path,
        bus=MessageBus(),
        memory_store=MemoryStore(memory_dir=tmp_path / "mem"),
    )
    memory_tool = next(t for t in tools if t.name == "memory")
    props = memory_tool.to_schema()["function"]["parameters"]["properties"]
    assert props["target"]["enum"] == ["user"]


def test_consolidator_without_semantic_manager_falls_back_to_open(tmp_path):
    """未装配 manager(旧用法/测试)时保守按开放处理——真正的拒绝仍由 service 兜底。"""
    from echo_agent.memory.consolidator import MemoryConsolidator
    from echo_agent.memory.store import MemoryStore

    async def _llm(**_kwargs):  # pragma: no cover
        raise AssertionError("not called")

    consolidator = MemoryConsolidator(
        memory_store=MemoryStore(memory_dir=tmp_path / "mem"), llm_call=_llm
    )
    fn = consolidator._extract_facts_tool[0]["function"]
    enum = fn["parameters"]["properties"]["facts"]["items"]["properties"]["type"]["enum"]
    assert enum == ["user", "environment"]
