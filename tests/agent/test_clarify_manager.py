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
    answer, interrupted = await mgr.wait_for_answer(req.id)
    await task
    assert answer == "A"
    assert interrupted is False


@pytest.mark.asyncio
async def test_resolve_unknown_id_returns_false():
    mgr = ClarifyManager()
    assert mgr.resolve("nope", "x") is False


@pytest.mark.asyncio
async def test_wait_for_unknown_id_returns_empty():
    mgr = ClarifyManager()
    assert await mgr.wait_for_answer("nope") == ("", False)


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
        mgr.resolve(req.id, "")  # session-interrupt sentinel

    task = asyncio.create_task(interrupt())
    answer, interrupted = await mgr.wait_for_answer(req.id)
    await task
    assert answer == ""
    assert interrupted is False


@pytest.mark.asyncio
async def test_wait_returns_answer_and_not_interrupted():
    mgr = ClarifyManager()
    req = mgr.request("q", ["A"], session_key="s1")

    async def ans():
        await asyncio.sleep(0.01)
        mgr.resolve(req.id, "A")

    t = asyncio.create_task(ans())
    answer, interrupted = await mgr.wait_for_answer(req.id)
    await t
    assert answer == "A"
    assert interrupted is False


@pytest.mark.asyncio
async def test_empty_answer_is_not_interrupt():
    mgr = ClarifyManager()
    req = mgr.request("q", ["A"], session_key="s1")

    async def ans():
        await asyncio.sleep(0.01)
        mgr.resolve(req.id, "")   # user actively answered empty

    t = asyncio.create_task(ans())
    answer, interrupted = await mgr.wait_for_answer(req.id)
    await t
    assert answer == ""
    assert interrupted is False


@pytest.mark.asyncio
async def test_cancel_session_interrupts_pending():
    mgr = ClarifyManager()
    r1 = mgr.request("q1", session_key="s1")
    r2 = mgr.request("q2", session_key="s1")
    r3 = mgr.request("q3", session_key="s2")

    async def do_cancel():
        await asyncio.sleep(0.01)
        n = mgr.cancel_session("s1")
        assert n == 2

    async def wait_one(cid):
        return await mgr.wait_for_answer(cid)

    t = asyncio.create_task(do_cancel())
    a1, i1 = await wait_one(r1.id)
    a2, i2 = await wait_one(r2.id)
    await t
    assert (a1, i1) == ("", True)
    assert (a2, i2) == ("", True)
    # s2 未受影响,仍 pending
    assert mgr.get(r3.id) is not None
