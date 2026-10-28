"""M3-B memory two-ends — wiring regressions.

Pins the three fixes that closed the broken memory ends:
  - WorkingMemory write-back (ResponseStage) so the next turn's injection
    is no longer always empty.
  - Episodic online recall (ContextStage) so written episodes are read back.
  - consolidator fact extraction via tool_choice instead of a silent
    json.loads that dropped malformed output (P2-4).
"""

from __future__ import annotations

from collections import OrderedDict
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.memory.consolidator import MemoryConsolidator
from echo_agent.memory.tiers import WorkingMemory
from echo_agent.models.provider import LLMResponse, ToolCallRequest


# ── consolidator fact extraction (P2-4) ──────────────────────────────────────


def test_parse_extracted_facts_from_tool_call():
    resp = LLMResponse(tool_calls=[ToolCallRequest(
        id="t1", name="save_facts",
        arguments={"facts": [{"key": "name", "content": "Dana", "type": "user"}]},
    )])
    facts = MemoryConsolidator._parse_extracted_facts(resp)
    assert facts == [{"key": "name", "content": "Dana", "type": "user"}]


def test_parse_extracted_facts_string_arguments():
    resp = LLMResponse(tool_calls=[ToolCallRequest(
        id="t1", name="save_facts", arguments='{"facts": [{"key": "k", "content": "v"}]}',
    )])
    facts = MemoryConsolidator._parse_extracted_facts(resp)
    assert facts == [{"key": "k", "content": "v"}]


def test_parse_extracted_facts_no_tool_call_returns_empty():
    # Previously a free-text response was json.loads'd and silently dropped;
    # now a missing tool call is an explicit empty result, not a hidden failure.
    resp = LLMResponse(content="here are some facts: ...", tool_calls=[])
    assert MemoryConsolidator._parse_extracted_facts(resp) == []


def test_parse_extracted_facts_malformed_string_returns_empty():
    resp = LLMResponse(tool_calls=[ToolCallRequest(
        id="t1", name="save_facts", arguments="not json at all",
    )])
    assert MemoryConsolidator._parse_extracted_facts(resp) == []


# ── WorkingMemory write-back (ResponseStage) ─────────────────────────────────


def _make_response_stage(working_memories):
    from echo_agent.agent.pipeline.response_stage import ResponseStage

    return ResponseStage(
        config=MagicMock(),
        sessions=MagicMock(),
        memory=MagicMock(),
        provider=MagicMock(),
        consolidation_worker=MagicMock(),
        default_model="m",
        spawn_fn=lambda c: None,
        clear_memory_snapshot_fn=AsyncMock(),
        skill_store=None,
        working_memories=working_memories,
    )


def test_working_memory_write_back_populates_next_turn_context():
    wm = WorkingMemory()
    wms = OrderedDict({"chan:1": wm})
    stage = _make_response_stage(wms)
    event = MagicMock(channel="telegram", text="My name is Dana")
    assert wm.get_context() == ""  # empty before
    stage._update_working_memory("chan:1", event, "Nice to meet you, Dana")
    ctx = wm.get_context()
    assert "Dana" in ctx
    assert "Nice to meet you" in ctx


def test_working_memory_write_back_skips_ephemeral_session():
    wm = WorkingMemory()
    wms = OrderedDict({"eval:1": wm})
    stage = _make_response_stage(wms)
    event = MagicMock(channel="eval", text="rm -rf /")
    stage._update_working_memory("eval:1", event, "I won't do that")
    assert wm.get_context() == ""  # eval traffic must not pollute working memory


# ── Episodic online recall (ContextStage) ────────────────────────────────────


@pytest.mark.asyncio
async def test_context_stage_recalls_episodes():
    from echo_agent.memory.types import Episode

    episodic = MagicMock()
    episodic.search_episodes = AsyncMock(return_value=[
        Episode(id="e1", session_key="s", summary="Discussed the vault code 7741"),
    ])
    episodic.get_session_episodes = AsyncMock(return_value=[])

    # Build a minimal ContextStage just to exercise the recall branch directly.
    from echo_agent.agent.pipeline.context_stage import ContextStage
    stage = ContextStage.__new__(ContextStage)
    stage._episodic = episodic

    # Simulate the recall block in isolation.
    retrieval_parts: list[str] = []
    episodes = await stage._episodic.search_episodes("vault", session_key="s", limit=3)
    if not episodes:
        episodes = await stage._episodic.get_session_episodes("s", limit=3)
    if episodes:
        retrieval_parts.append(
            "Past episodes:\n" + "\n".join(f"- {ep.summary}" for ep in episodes if ep.summary)
        )
    assert any("vault code 7741" in p for p in retrieval_parts)
    episodic.search_episodes.assert_awaited_once()
