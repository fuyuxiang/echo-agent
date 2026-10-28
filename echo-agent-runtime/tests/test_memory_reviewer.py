"""Comprehensive tests for echo_agent.memory.reviewer — MemoryReviewer."""

from __future__ import annotations

import uuid
from typing import Any
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.memory.reviewer import MemoryReviewer, _MAX_REVIEW_ITERATIONS
from echo_agent.memory.service import WriteResult
from echo_agent.memory.types import MemoryEntry, MemoryType


# ---------------------------------------------------------------------------
# Factories / helpers
# ---------------------------------------------------------------------------

def _make_entry(**overrides: Any) -> MemoryEntry:
    defaults = dict(
        id=uuid.uuid4().hex[:12],
        type=MemoryType.USER,
        key="test_key",
        content="test content",
        importance=0.5,
    )
    defaults.update(overrides)
    return MemoryEntry(**defaults)


def _make_tool_call(
    arguments: dict[str, Any],
    tc_id: str = "tc_1",
    name: str = "memory_manage",
) -> MagicMock:
    tc = MagicMock()
    tc.id = tc_id
    tc.name = name
    tc.arguments = arguments
    tc.to_openai_format.return_value = {
        "id": tc_id,
        "type": "function",
        "function": {"name": name, "arguments": arguments},
    }
    return tc


def _make_response(
    content: str = "",
    tool_calls: list | None = None,
) -> MagicMock:
    resp = MagicMock()
    resp.content = content
    resp.tool_calls = tool_calls or []
    resp.has_tool_calls = bool(tool_calls)
    resp.finish_reason = "stop"
    return resp


def _build_reviewer(
    session_key: str = "sess_1",
) -> tuple[MemoryReviewer, MagicMock, MagicMock, MagicMock]:
    """Return (reviewer, mock_provider, mock_service, mock_store).

    reviewer 现在收 service;读操作(find/resolve)走 service.store,写操作走
    service.add/replace/remove(AsyncMock)。默认写返回成功 WriteResult。
    """
    provider = AsyncMock()
    store = MagicMock()
    service = MagicMock()
    service.store = store
    service.add = AsyncMock(return_value=WriteResult(ok=True, entry=_make_entry()))
    service.replace = AsyncMock(return_value=WriteResult(ok=True, entry=_make_entry()))
    service.remove = AsyncMock(return_value=WriteResult(ok=True, entry=_make_entry()))
    reviewer = MemoryReviewer(provider=provider, service=service, model="test-model", session_key=session_key)
    return reviewer, provider, service, store


# ---------------------------------------------------------------------------
# TestResolveEntry
# ---------------------------------------------------------------------------

class TestResolveEntry:
    """Tests for MemoryReviewer._resolve_entry."""

    def test_key_found(self):
        reviewer, _, _service, store = _build_reviewer()
        existing = _make_entry(key="lang")
        store.find_by_key.return_value = existing

        entry, err = reviewer._resolve_entry("lang", "", MemoryType.USER)

        assert entry is existing
        assert err is None
        store.find_by_key.assert_called_once_with("lang", MemoryType.USER, session_key="sess_1")

    def test_key_not_found_old_text_single_match(self):
        reviewer, _, _service, store = _build_reviewer()
        existing = _make_entry(key="pref")
        store.find_by_key.return_value = None
        store.find_by_content_matches.return_value = [existing]

        entry, err = reviewer._resolve_entry("missing_key", "some old text", MemoryType.USER)

        assert entry is existing
        assert err is None
        store.find_by_content_matches.assert_called_once_with(
            "some old text", mem_type=MemoryType.USER, limit=6, session_key="sess_1",
        )

    def test_old_text_multiple_matches_returns_error(self):
        reviewer, _, _service, store = _build_reviewer()
        m1 = _make_entry(key="a")
        m2 = _make_entry(key="b")
        store.find_by_key.return_value = None
        store.find_by_content_matches.return_value = [m1, m2]

        entry, err = reviewer._resolve_entry("", "ambiguous", MemoryType.ENVIRONMENT)

        assert entry is None
        assert err is not None
        assert "multiple matching memories" in err

    def test_neither_key_nor_old_text(self):
        reviewer, _, _service, store = _build_reviewer()

        entry, err = reviewer._resolve_entry("", "", MemoryType.USER)

        assert entry is None
        assert err is None
        store.find_by_key.assert_not_called()
        store.find_by_content_matches.assert_not_called()

    def test_key_not_found_old_text_no_matches(self):
        reviewer, _, _service, store = _build_reviewer()
        store.find_by_key.return_value = None
        store.find_by_content_matches.return_value = []

        entry, err = reviewer._resolve_entry("nope", "also nope", MemoryType.USER)

        assert entry is None
        assert err is None


# ---------------------------------------------------------------------------
# TestExecute
# ---------------------------------------------------------------------------

class TestExecute:
    """Tests for MemoryReviewer._execute (async, 经 service)。"""

    # -- add --

    @pytest.mark.asyncio
    async def test_add_success(self):
        reviewer, _, service, _store = _build_reviewer()
        service.add.return_value = WriteResult(ok=True, entry=_make_entry(key="color", content="blue"))

        result = await reviewer._execute({"action": "add", "target": "user", "key": "color", "content": "blue"})

        assert result == "Added [user] color"
        service.add.assert_awaited_once()
        ctx = service.add.call_args[0][0]
        kwargs = service.add.call_args[1]
        assert ctx.actor == "reviewer"
        assert ctx.memory_scope == "sess_1"
        assert kwargs["key"] == "color"
        assert kwargs["content"] == "blue"
        assert kwargs["type"] == MemoryType.USER
        assert kwargs["source"] == "model_inferred"

    @pytest.mark.asyncio
    async def test_add_missing_key(self):
        reviewer, _, _, _ = _build_reviewer()
        result = await reviewer._execute({"action": "add", "target": "user", "content": "blue"})
        assert result.startswith("Error")

    @pytest.mark.asyncio
    async def test_add_missing_content(self):
        reviewer, _, _, _ = _build_reviewer()
        result = await reviewer._execute({"action": "add", "target": "user", "key": "color"})
        assert result.startswith("Error")

    @pytest.mark.asyncio
    async def test_add_service_rejects_invalid(self):
        reviewer, _, service, _store = _build_reviewer()
        service.add.return_value = WriteResult(ok=False, reason="invalid")

        result = await reviewer._execute({"action": "add", "target": "user", "key": "k", "content": "c"})

        assert result.startswith("Error")
        assert "invalid" in result

    @pytest.mark.asyncio
    async def test_add_environment_type_and_scope(self):
        reviewer, _, service, _store = _build_reviewer()
        service.add.return_value = WriteResult(ok=True, entry=_make_entry(key="proj"))

        await reviewer._execute({"action": "add", "target": "environment", "key": "proj", "content": "python"})

        kwargs = service.add.call_args[1]
        assert kwargs["type"] == MemoryType.ENVIRONMENT
        # scope 门禁/来源写入由 service 决定,reviewer 只传 actor/scope 上下文。
        ctx = service.add.call_args[0][0]
        assert ctx.actor == "reviewer"

    # -- replace --

    @pytest.mark.asyncio
    async def test_replace_existing_entry(self):
        reviewer, _, service, store = _build_reviewer()
        existing = _make_entry(id="e1", key="lang")
        store.find_by_key.return_value = existing
        service.replace.return_value = WriteResult(ok=True, entry=existing)

        result = await reviewer._execute({
            "action": "replace", "target": "user", "key": "lang", "content": "rust",
        })

        assert result == "Updated [user] lang"
        service.replace.assert_awaited_once()
        assert service.replace.call_args[0][1] == "e1"
        assert service.replace.call_args[1] == {"content": "rust", "source": "model_inferred"}

    @pytest.mark.asyncio
    async def test_replace_no_entry_creates_new(self):
        reviewer, _, service, store = _build_reviewer()
        store.find_by_key.return_value = None
        store.find_by_content_matches.return_value = []
        service.add.return_value = WriteResult(ok=True, entry=_make_entry(key="theme"))

        result = await reviewer._execute({
            "action": "replace", "target": "user", "key": "theme", "content": "dark",
        })

        assert "Added (new)" in result
        assert "[user]" in result
        service.add.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_replace_resolve_error(self):
        reviewer, _, service, store = _build_reviewer()
        store.find_by_key.return_value = None
        m1 = _make_entry(key="a")
        m2 = _make_entry(key="b")
        store.find_by_content_matches.return_value = [m1, m2]

        result = await reviewer._execute({
            "action": "replace", "target": "user", "old_text": "ambig", "content": "new",
        })

        assert result.startswith("Error")
        assert "multiple matching" in result
        service.replace.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_replace_missing_content(self):
        reviewer, _, _, _ = _build_reviewer()
        result = await reviewer._execute({"action": "replace", "target": "user", "key": "k"})
        assert result == "Error: content required"

    @pytest.mark.asyncio
    async def test_replace_rejected_provenance_keeps_existing(self):
        reviewer, _, service, store = _build_reviewer()
        existing = _make_entry(id="e1", key="lang", source="user_stated")
        store.find_by_key.return_value = existing
        service.replace.return_value = WriteResult(ok=False, reason="rejected_provenance")

        result = await reviewer._execute({
            "action": "replace", "target": "user", "key": "lang", "content": "go",
        })

        # 被拒仅拒绝,不谎称已写;不计入 actions(非 Error 前缀但保留原内容)。
        assert "Kept existing (higher provenance)" in result
        assert "lang" in result

    # -- remove --

    @pytest.mark.asyncio
    async def test_remove_success(self):
        reviewer, _, service, store = _build_reviewer()
        existing = _make_entry(id="e1", key="old_pref")
        store.find_by_key.return_value = existing
        service.remove.return_value = WriteResult(ok=True, entry=existing)

        result = await reviewer._execute({"action": "remove", "target": "user", "key": "old_pref"})

        assert result == "Removed [user] old_pref"
        service.remove.assert_awaited_once()
        assert service.remove.call_args[0][1] == "e1"

    @pytest.mark.asyncio
    async def test_remove_no_matching_entry(self):
        reviewer, _, service, store = _build_reviewer()
        store.find_by_key.return_value = None
        store.find_by_content_matches.return_value = []

        result = await reviewer._execute({"action": "remove", "target": "user", "key": "gone"})

        assert result == "Error: no matching memory found"
        service.remove.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_remove_rejected_provenance_keeps_existing(self):
        reviewer, _, service, store = _build_reviewer()
        existing = _make_entry(id="e1", key="home", source="user_stated")
        store.find_by_key.return_value = existing
        service.remove.return_value = WriteResult(ok=False, reason="rejected_provenance")

        result = await reviewer._execute({"action": "remove", "target": "user", "key": "home"})

        assert "Kept existing (higher provenance)" in result
        assert "home" in result

    @pytest.mark.asyncio
    async def test_remove_resolve_error(self):
        reviewer, _, service, store = _build_reviewer()
        store.find_by_key.return_value = None
        m1 = _make_entry(key="x")
        m2 = _make_entry(key="y")
        store.find_by_content_matches.return_value = [m1, m2]

        result = await reviewer._execute({
            "action": "remove", "target": "environment", "old_text": "ambig",
        })

        assert "multiple matching" in result

    # -- unknown --

    @pytest.mark.asyncio
    async def test_unknown_action(self):
        reviewer, _, _, _ = _build_reviewer()
        result = await reviewer._execute({"action": "dance", "target": "user"})
        assert result == "Error: unknown action 'dance'"

    @pytest.mark.asyncio
    async def test_importance_clamped(self):
        reviewer, _, service, _store = _build_reviewer()
        service.add.return_value = WriteResult(ok=True, entry=_make_entry(key="k"))

        await reviewer._execute({"action": "add", "target": "user", "key": "k", "content": "c", "importance": 5.0})

        assert service.add.call_args[1]["importance"] == 1.0


# ---------------------------------------------------------------------------
# TestReview
# ---------------------------------------------------------------------------

class TestReview:
    """Tests for MemoryReviewer.review (async)."""

    @pytest.mark.asyncio
    async def test_no_actions(self):
        reviewer, provider, _, _ = _build_reviewer()
        provider.chat_with_retry.return_value = _make_response(content="No memory changes needed.")

        actions = await reviewer.review([{"role": "user", "content": "hi"}])

        assert actions == []
        provider.chat_with_retry.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_single_add_action(self):
        reviewer, provider, service, _store = _build_reviewer()
        service.add.return_value = WriteResult(ok=True, entry=_make_entry(key="color"))

        tc = _make_tool_call({"action": "add", "target": "user", "key": "color", "content": "blue"})
        first_resp = _make_response(content="I'll save that.", tool_calls=[tc])
        second_resp = _make_response(content="Done.")

        provider.chat_with_retry.side_effect = [first_resp, second_resp]

        actions = await reviewer.review([{"role": "user", "content": "I like blue"}])

        assert len(actions) == 1
        assert "Added [user] color" in actions[0]

    @pytest.mark.asyncio
    async def test_multiple_actions_one_iteration(self):
        reviewer, provider, service, _store = _build_reviewer()
        service.add.side_effect = [
            WriteResult(ok=True, entry=_make_entry(key="lang")),
            WriteResult(ok=True, entry=_make_entry(key="editor")),
        ]

        tc1 = _make_tool_call(
            {"action": "add", "target": "user", "key": "lang", "content": "python"}, tc_id="tc_1",
        )
        tc2 = _make_tool_call(
            {"action": "add", "target": "environment", "key": "editor", "content": "vim"}, tc_id="tc_2",
        )
        first_resp = _make_response(tool_calls=[tc1, tc2])
        second_resp = _make_response(content="All done.")

        provider.chat_with_retry.side_effect = [first_resp, second_resp]

        actions = await reviewer.review([{"role": "user", "content": "setup"}])

        assert len(actions) == 2

    @pytest.mark.asyncio
    async def test_max_iterations_reached(self):
        reviewer, provider, service, _store = _build_reviewer()
        service.add.return_value = WriteResult(ok=True, entry=_make_entry(key="k"))

        tc = _make_tool_call({"action": "add", "target": "user", "key": "k", "content": "v"})
        looping_resp = _make_response(tool_calls=[tc])
        provider.chat_with_retry.return_value = looping_resp

        actions = await reviewer.review([{"role": "user", "content": "loop"}])

        assert provider.chat_with_retry.await_count == _MAX_REVIEW_ITERATIONS
        assert len(actions) == _MAX_REVIEW_ITERATIONS

    @pytest.mark.asyncio
    async def test_llm_exception_breaks_loop(self):
        reviewer, provider, _, _ = _build_reviewer()
        provider.chat_with_retry.side_effect = RuntimeError("API down")

        actions = await reviewer.review([{"role": "user", "content": "hi"}])

        assert actions == []
        provider.chat_with_retry.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_error_results_not_added_to_actions(self):
        reviewer, provider, _service, store = _build_reviewer()
        # add with missing key -> Error
        tc = _make_tool_call({"action": "add", "target": "user", "content": "no key"})
        first_resp = _make_response(tool_calls=[tc])
        second_resp = _make_response(content="Oops.")

        provider.chat_with_retry.side_effect = [first_resp, second_resp]

        actions = await reviewer.review([{"role": "user", "content": "test"}])

        assert actions == []

    @pytest.mark.asyncio
    async def test_review_appends_review_prompt(self):
        """The review prompt is appended as the last user message."""
        reviewer, provider, _, _ = _build_reviewer()
        provider.chat_with_retry.return_value = _make_response(content="Nothing to save.")

        convo = [{"role": "user", "content": "hello"}]
        await reviewer.review(convo)

        sent_messages = provider.chat_with_retry.call_args[1]["messages"]
        # The review prompt is the user message right before the assistant reply
        review_msg = next(m for m in sent_messages if m["role"] == "user" and "Review the conversation" in m["content"])
        assert review_msg is not None
        # Original conversation should not be mutated
        assert len(convo) == 1

    @pytest.mark.asyncio
    async def test_tool_results_appended_to_messages(self):
        """Tool results are fed back as tool-role messages."""
        reviewer, provider, service, _store = _build_reviewer()
        service.add.return_value = WriteResult(ok=True, entry=_make_entry(key="k"))

        tc = _make_tool_call({"action": "add", "target": "user", "key": "k", "content": "v"})
        first_resp = _make_response(content="Saving.", tool_calls=[tc])
        second_resp = _make_response(content="Done.")
        provider.chat_with_retry.side_effect = [first_resp, second_resp]

        await reviewer.review([{"role": "user", "content": "hi"}])

        second_call_msgs = provider.chat_with_retry.call_args_list[1][1]["messages"]
        tool_msgs = [m for m in second_call_msgs if m.get("role") == "tool"]
        assert len(tool_msgs) == 1
        assert tool_msgs[0]["tool_call_id"] == "tc_1"
        assert "Added [user] k" in tool_msgs[0]["content"]


# ---------------------------------------------------------------------------
# TestServiceWiring — reviewer 经 MemoryService 的端到端等价 (real store/service)
# ---------------------------------------------------------------------------

class TestServiceWiring:
    """reviewer 改走 service 后,provenance 守卫由 service 八步写序统一强制。"""

    @pytest.mark.asyncio
    async def test_reviewer_remove_blocks_user_stated_via_service(self, tmp_path):
        from echo_agent.memory.store import MemoryStore
        from echo_agent.memory.service import MemoryService

        store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
        e = store.add(MemoryEntry(
            type=MemoryType.USER, key="home", content="上海",
            source="user_stated", source_session="s",
        ))
        r = MemoryReviewer(provider=AsyncMock(), service=MemoryService(store), session_key="s")

        await r._execute({"action": "remove", "target": "user", "key": "home"})

        # reviewer 恒 model_inferred,不得删除 user_stated 高优先级条目。
        assert store.get(e.id) is not None
