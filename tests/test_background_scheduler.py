"""Tests for the tiered, bounded background task scheduler."""

from __future__ import annotations

import asyncio

import pytest

from echo_agent.agent.background import BackgroundScheduler, Tier


@pytest.mark.asyncio
async def test_durable_task_runs_and_completes():
    sched = BackgroundScheduler(max_concurrency=2)
    done = asyncio.Event()

    async def work():
        done.set()

    sched.spawn(work(), tier=Tier.DURABLE)
    await asyncio.wait_for(done.wait(), timeout=1.0)
    await sched.aclose()


@pytest.mark.asyncio
async def test_discardable_dropped_when_saturated():
    sched = BackgroundScheduler(max_concurrency=1)
    block = asyncio.Event()

    async def slow():
        await block.wait()

    sched.spawn(slow(), tier=Tier.DISCARDABLE)  # occupy the only slot
    await asyncio.sleep(0.01)
    ran = []

    async def extra():
        ran.append(1)

    sched.spawn(extra(), tier=Tier.DISCARDABLE)  # saturated -> dropped
    await asyncio.sleep(0.01)
    assert sched.stats()["dropped"] >= 1
    assert ran == []
    block.set()
    await sched.aclose()


@pytest.mark.asyncio
async def test_durable_queues_instead_of_dropping():
    sched = BackgroundScheduler(max_concurrency=1)
    block = asyncio.Event()
    order = []

    async def first():
        await block.wait()
        order.append("first")

    async def second():
        order.append("second")

    sched.spawn(first(), tier=Tier.DURABLE)
    await asyncio.sleep(0.01)
    sched.spawn(second(), tier=Tier.DURABLE)  # queued, never dropped
    assert sched.stats()["dropped"] == 0
    block.set()
    await sched.aclose()
    assert "second" in order


@pytest.mark.asyncio
async def test_durable_failure_retries():
    sched = BackgroundScheduler(max_concurrency=2, durable_retries=2)
    attempts = []

    async def flaky():
        attempts.append(1)
        if len(attempts) < 2:
            raise RuntimeError("boom")

    # DURABLE retry requires a factory: a bare coroutine can only be awaited
    # once, so retries pass a zero-arg callable that yields a fresh coroutine.
    sched.spawn(lambda: flaky(), tier=Tier.DURABLE)
    await sched.aclose()
    assert len(attempts) >= 2  # retried until success


@pytest.mark.asyncio
async def test_durable_factory_giving_up_does_not_raise():
    sched = BackgroundScheduler(max_concurrency=2, durable_retries=1)
    attempts = []

    async def always_fail():
        attempts.append(1)
        raise RuntimeError("boom")

    sched.spawn(lambda: always_fail(), tier=Tier.DURABLE)
    await sched.aclose()
    # initial attempt + 1 retry = 2, then gives up without propagating
    assert len(attempts) == 2


@pytest.mark.asyncio
async def test_aclose_cancels_discardable_but_awaits_durable():
    sched = BackgroundScheduler(max_concurrency=4)
    durable_done = asyncio.Event()
    never = asyncio.Event()
    discardable_finished = []

    async def durable_work():
        durable_done.set()

    async def discardable_blocks():
        await never.wait()
        discardable_finished.append(1)

    sched.spawn(discardable_blocks(), tier=Tier.DISCARDABLE)
    sched.spawn(durable_work(), tier=Tier.DURABLE)
    await asyncio.sleep(0.01)
    await sched.aclose(timeout=1.0)
    assert durable_done.is_set()  # durable was awaited to completion
    assert discardable_finished == []  # discardable was cancelled mid-flight


@pytest.mark.asyncio
async def test_discardable_accepts_factory_too():
    sched = BackgroundScheduler(max_concurrency=2)
    ran = asyncio.Event()

    async def work():
        ran.set()

    sched.spawn(lambda: work(), tier=Tier.DISCARDABLE)
    await asyncio.wait_for(ran.wait(), timeout=1.0)
    await sched.aclose()
