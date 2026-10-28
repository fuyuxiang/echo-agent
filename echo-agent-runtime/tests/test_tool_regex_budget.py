"""User-supplied regexes must have a request-wide, off-loop time budget."""

import asyncio

import pytest

from echo_agent.tools import ToolExecutionContext


@pytest.mark.asyncio
@pytest.mark.parametrize("name", ["files", "spill", "sessions"])
async def test_regex_timeout_is_an_explicit_failure(tmp_path, monkeypatch, name):
    text = "a" * 30 + "!"
    params = {"pattern": "(a|aa)+$"}
    ctx = ToolExecutionContext(session_key="test:regex")
    if name == "files":
        from echo_agent.agent.tools import search
        monkeypatch.setattr(search, "_SOFT_BUDGET_SECONDS", 0.02)
        (tmp_path / "input.txt").write_text(text)
        tool = search.SearchFilesTool(str(tmp_path))
    elif name == "spill":
        from echo_agent.agent.tools import read_spill
        from echo_agent.spill.store import SpillStore
        monkeypatch.setattr(read_spill, "_SEARCH_TIMEOUT_SECONDS", 0.02, raising=False)
        params["path"] = str(SpillStore(tmp_path).save(ctx.session_key, "source", text))
        tool = read_spill.ReadSpillTool(tmp_path)
    else:
        from echo_agent.agent.tools import session_search
        from echo_agent.session.manager import SessionManager
        monkeypatch.setattr(session_search, "_SEARCH_TIMEOUT_SECONDS", 0.02, raising=False)
        manager = SessionManager(tmp_path)
        session = await manager.get_or_create(ctx.session_key)
        session.add_message("user", text)
        await manager.save(session)
        tool = session_search.SessionSearchTool(manager)
        params = {"query": params["pattern"], "session_key": ctx.session_key}

    # The heartbeat must run while a CPU-heavy match is in progress. In
    # particular, asyncio.wait_for alone cannot interrupt an inline C matcher.
    progressed = asyncio.Event()
    async def heartbeat():
        await asyncio.sleep(0.005)
        progressed.set()
    ticker = asyncio.create_task(heartbeat())
    result = await tool.execute(params, ctx)
    was_responsive = progressed.is_set()
    await ticker
    assert result.success is False
    assert result.error_kind == "timeout"
    assert "budget" in result.error.lower()
    assert was_responsive


@pytest.mark.asyncio
async def test_session_regex_budget_is_shared_across_all_messages(tmp_path, monkeypatch):
    from echo_agent.agent.tools import session_search
    from echo_agent.session.manager import SessionManager

    monkeypatch.setattr(session_search, "_SEARCH_TIMEOUT_SECONDS", 0.03, raising=False)
    manager = SessionManager(tmp_path)
    session = await manager.get_or_create("test:many")
    # Each individual match fits the budget; a fresh budget per message would
    # silently allow this request to use an unbounded multiple of that budget.
    for _ in range(50):
        session.add_message("user", "a" * 18 + "!")
    await manager.save(session)
    result = await session_search.SessionSearchTool(manager).execute({"query": "(a|aa)+$"})
    assert result.success is False
    assert result.error_kind == "timeout"
