"""Tests for the hook registry."""

import pytest

from echo_agent.plugins.hooks import HookRegistry, HookResult


@pytest.fixture
def registry():
    return HookRegistry()


@pytest.mark.asyncio
async def test_dispatch_empty(registry):
    results = await registry.dispatch("pre_tool_call", "exec", {}, None)
    assert results == []


@pytest.mark.asyncio
async def test_dispatch_single_hook(registry):
    async def hook(tool_name, params, ctx):
        return HookResult(modified={"injected": True})

    registry.register("pre_tool_call", hook, plugin="test")
    results = await registry.dispatch("pre_tool_call", "exec", {}, None)
    assert len(results) == 1
    assert results[0].modified == {"injected": True}


@pytest.mark.asyncio
async def test_dispatch_fail_open(registry):
    """A hook that raises should not crash dispatch."""
    async def bad_hook(tool_name, params, ctx):
        raise RuntimeError("boom")

    async def good_hook(tool_name, params, ctx):
        return HookResult(modified="ok")

    registry.register("pre_tool_call", bad_hook, plugin="bad")
    registry.register("pre_tool_call", good_hook, plugin="good")

    results = await registry.dispatch("pre_tool_call", "exec", {}, None)
    assert len(results) == 1
    assert results[0].modified == "ok"


@pytest.mark.asyncio
async def test_dispatch_stop_propagation(registry):
    async def first(tool_name, params, ctx):
        return HookResult(modified="first", stop_propagation=True)

    async def second(tool_name, params, ctx):
        return HookResult(modified="second")

    registry.register("pre_tool_call", first, plugin="p1")
    registry.register("pre_tool_call", second, plugin="p2")

    results = await registry.dispatch("pre_tool_call", "exec", {}, None)
    assert len(results) == 1
    assert results[0].modified == "first"


@pytest.mark.asyncio
async def test_dispatch_modify(registry):
    async def add_key(value, *args, **kwargs):
        return HookResult(modified={**value, "extra": "added"})

    registry.register("pre_tool_call", add_key, plugin="test")
    result = await registry.dispatch_modify("pre_tool_call", {"original": True})
    assert result == {"original": True, "extra": "added"}


@pytest.mark.asyncio
async def test_dispatch_modify_chain(registry):
    async def step1(value, *args, **kwargs):
        return HookResult(modified=value + 1)

    async def step2(value, *args, **kwargs):
        return HookResult(modified=value * 10)

    registry.register("post_tool_call", step1, plugin="p1")
    registry.register("post_tool_call", step2, plugin="p2")

    result = await registry.dispatch_modify("post_tool_call", 5)
    assert result == 60  # (5+1) * 10


@pytest.mark.asyncio
async def test_dispatch_modify_no_modification(registry):
    async def observer(value, *args, **kwargs):
        return None

    registry.register("post_tool_call", observer, plugin="obs")
    result = await registry.dispatch_modify("post_tool_call", "unchanged")
    assert result == "unchanged"


@pytest.mark.asyncio
async def test_cancel_hook(registry):
    async def blocker(tool_name, params, ctx):
        return HookResult(cancel=True, cancel_reason="blocked by test")

    registry.register("pre_tool_call", blocker, plugin="test")
    results = await registry.dispatch("pre_tool_call", "exec", {}, None)
    assert results[0].cancel is True
    assert results[0].cancel_reason == "blocked by test"


def test_has_hooks(registry):
    assert not registry.has_hooks("pre_tool_call")

    async def noop(*args):
        return None

    registry.register("pre_tool_call", noop, plugin="test")
    assert registry.has_hooks("pre_tool_call")


def test_unregister_plugin(registry):
    async def noop(*args):
        return None

    registry.register("pre_tool_call", noop, plugin="p1")
    registry.register("post_tool_call", noop, plugin="p1")
    registry.register("pre_tool_call", noop, plugin="p2")

    registry.unregister_plugin("p1")
    assert not registry.has_hooks("post_tool_call")
    assert registry.has_hooks("pre_tool_call")  # p2 still there


def test_sync_callback_wrapped(registry):
    def sync_hook(tool_name, params, ctx):
        return HookResult(modified="sync_works")

    registry.register("pre_tool_call", sync_hook, plugin="sync")
    assert registry.has_hooks("pre_tool_call")


@pytest.mark.asyncio
async def test_sync_callback_execution(registry):
    """Sync callbacks should be wrapped and executed correctly."""
    def sync_hook(tool_name, params, ctx):
        return HookResult(modified="sync_result")

    registry.register("pre_tool_call", sync_hook, plugin="sync")
    results = await registry.dispatch("pre_tool_call", "exec", {}, None)
    assert len(results) == 1
    assert results[0].modified == "sync_result"


def test_register_unknown_hook(registry):
    """Unknown hook names should still be stored (with a warning)."""
    async def noop(*args):
        return None

    registry.register("unknown_hook_xyz", noop, plugin="test")
    assert registry.has_hooks("unknown_hook_xyz")


@pytest.mark.asyncio
async def test_dispatch_modify_with_exception(registry):
    """dispatch_modify should skip failing callbacks and preserve value."""
    async def bad(value, *args, **kwargs):
        raise ValueError("oops")

    async def good(value, *args, **kwargs):
        return HookResult(modified=value + "_modified")

    registry.register("post_tool_call", bad, plugin="bad")
    registry.register("post_tool_call", good, plugin="good")

    result = await registry.dispatch_modify("post_tool_call", "original")
    assert result == "original_modified"


@pytest.mark.asyncio
async def test_dispatch_modify_stop_propagation(registry):
    """dispatch_modify should stop when stop_propagation is set."""
    async def stopper(value, *args, **kwargs):
        return HookResult(modified="stopped", stop_propagation=True)

    async def never_reached(value, *args, **kwargs):
        return HookResult(modified="should_not_appear")

    registry.register("post_tool_call", stopper, plugin="p1")
    registry.register("post_tool_call", never_reached, plugin="p2")

    result = await registry.dispatch_modify("post_tool_call", "start")
    assert result == "stopped"


@pytest.mark.asyncio
async def test_dispatch_none_return_ignored(registry):
    """Callbacks returning None should not appear in results."""
    async def returns_none(*args):
        return None

    async def returns_result(*args):
        return HookResult(modified="yes")

    registry.register("pre_tool_call", returns_none, plugin="p1")
    registry.register("pre_tool_call", returns_result, plugin="p2")

    results = await registry.dispatch("pre_tool_call", "exec", {}, None)
    assert len(results) == 1
    assert results[0].modified == "yes"


def test_get_registered_hooks(registry):
    async def noop(*args):
        return None

    registry.register("pre_tool_call", noop, plugin="p1")
    registry.register("pre_tool_call", noop, plugin="p2")
    registry.register("on_agent_start", noop, plugin="p1")

    info = registry.get_registered_hooks()
    assert "pre_tool_call" in info
    assert info["pre_tool_call"] == ["p1", "p2"]
    assert info["on_agent_start"] == ["p1"]
