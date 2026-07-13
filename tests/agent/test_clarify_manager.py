import asyncio
import pytest
from echo_agent.agent.clarify_manager import ClarifyManager


@pytest.mark.asyncio
async def test_request_wait_resolve_roundtrip():
    mgr = ClarifyManager()
    req = mgr.request("选哪个?", ["A", "B"], user_id="u1")
    assert req.id and req.answer is None

    async def answer_later():
        await asyncio.sleep(0.01)
        assert mgr.resolve(req.id, "A") is True

    task = asyncio.create_task(answer_later())
    answer = await mgr.wait_for_answer(req.id)
    await task
    assert answer == "A"


@pytest.mark.asyncio
async def test_resolve_unknown_id_returns_false():
    mgr = ClarifyManager()
    assert mgr.resolve("nope", "x") is False


@pytest.mark.asyncio
async def test_wait_for_unknown_id_returns_empty():
    mgr = ClarifyManager()
    assert await mgr.wait_for_answer("nope") == ""


@pytest.mark.asyncio
async def test_resolve_twice_second_is_false():
    mgr = ClarifyManager()
    req = mgr.request("q")
    assert mgr.resolve(req.id, "first") is True
    assert mgr.resolve(req.id, "second") is False


@pytest.mark.asyncio
async def test_sentinel_empty_answer_unblocks():
    mgr = ClarifyManager()
    req = mgr.request("q", ["A"])

    async def interrupt():
        await asyncio.sleep(0.01)
        mgr.resolve(req.id, "")  # 会话中断哨兵

    task = asyncio.create_task(interrupt())
    answer = await mgr.wait_for_answer(req.id)
    await task
    assert answer == ""
