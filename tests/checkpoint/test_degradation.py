"""Three-layer degradation: disabled / no git / snapshot failure never blocks writes."""
from pathlib import Path
from types import SimpleNamespace

import pytest

from echo_agent.checkpoint.hook import install_checkpoint, make_pre_tool_checkpoint_hook


class _Hooks:
    def __init__(self):
        self.registered = []

    def register(self, name, cb, plugin=""):
        self.registered.append((name, plugin))


def _cfg(enabled=True):
    return SimpleNamespace(checkpoint=SimpleNamespace(
        enabled=enabled, store_path="~/.echo-agent/checkpoints/store",
        max_snapshots_per_workspace=20, max_total_size_mb=500, max_file_size_mb=10,
    ))


# Layer 1: disabled -> no manager, no hook.
def test_install_skips_when_disabled(tmp_path: Path):
    hooks = _Hooks()
    mgr = install_checkpoint(_cfg(enabled=False), tmp_path, hooks)
    assert mgr is None
    assert hooks.registered == []


# Layer 2: git missing -> silently disabled, no hook.
def test_install_skips_when_git_missing(tmp_path: Path, monkeypatch):
    monkeypatch.setattr("echo_agent.checkpoint.hook.git_available", lambda: False)
    hooks = _Hooks()
    mgr = install_checkpoint(_cfg(enabled=True), tmp_path, hooks)
    assert mgr is None
    assert hooks.registered == []


# Layer 2b: git present -> manager built and pre_tool_call hook registered.
def test_install_registers_hook_when_git_present(tmp_path: Path, monkeypatch):
    monkeypatch.setattr("echo_agent.checkpoint.hook.git_available", lambda: True)
    hooks = _Hooks()
    mgr = install_checkpoint(_cfg(enabled=True), tmp_path, hooks)
    assert mgr is not None
    assert hooks.registered == [("pre_tool_call", "checkpoint")]


# Layer 3: any snapshot exception is swallowed, the hook never raises (never blocks the write).
@pytest.mark.asyncio
async def test_hook_fail_open_on_snapshot_error():
    class _BoomManager:
        def new_turn(self, trace_id):
            pass

        async def ensure_checkpoint(self, reason):
            raise RuntimeError("boom")

    hook = make_pre_tool_checkpoint_hook(_BoomManager())
    ctx = SimpleNamespace(trace_id="t-1")
    # write_file would trigger ensure_checkpoint, which raises; hook must return None regardless.
    assert await hook("write_file", {}, ctx) is None


@pytest.mark.asyncio
async def test_hook_fail_open_on_new_turn_error():
    class _BoomManager:
        def new_turn(self, trace_id):
            raise RuntimeError("boom")

        async def ensure_checkpoint(self, reason):  # pragma: no cover - unreachable
            raise AssertionError("should not be reached")

    hook = make_pre_tool_checkpoint_hook(_BoomManager())
    ctx = SimpleNamespace(trace_id="t-1")
    assert await hook("write_file", {}, ctx) is None
