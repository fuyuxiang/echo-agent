"""Interleaved model replies must stay attached to their own trajectory."""

from types import SimpleNamespace

import pytest

from echo_agent.evolution.recorder import TrajectoryRecorder
from echo_agent.evolution.store import TrajectoryStore
from echo_agent.plugins.hooks import HookRegistry
from echo_agent.storage.sqlite import SQLiteBackend


@pytest.mark.asyncio
async def test_interleaved_llm_hooks_do_not_copy_response_between_sessions(tmp_path):
    storage = SQLiteBackend(tmp_path / "trajectories.db")
    await storage.initialize()
    store = TrajectoryStore(storage)
    await store.init_schema()
    recorder = TrajectoryRecorder(store)
    hooks = HookRegistry()
    recorder.attach(hooks)
    try:
        for session in ("alice", "bob"):
            await recorder.begin_turn(session_key=session, chat_id=session, channel="gateway", task_input="private task")
        await hooks.dispatch_modify("post_llm_call", SimpleNamespace(content="Alice private answer"), [], session_key="alice")
        await hooks.dispatch_modify("post_llm_call", SimpleNamespace(content="Bob private answer"), [], session_key="bob")
        await hooks.dispatch_modify("post_llm_call", SimpleNamespace(content="Alice final"), [], session_key="alice")
        # A legacy plugin lacking provenance cannot attribute an answer while
        # more than one session is active, so it must leave both untouched.
        await hooks.dispatch_modify("post_llm_call", SimpleNamespace(content="unattributed"), [])
        alice = await recorder.end_turn(session_key="alice", response_text="")
        bob = await recorder.end_turn(session_key="bob", response_text="")
        assert alice.final_response == "Alice final"
        assert alice.iterations == 2
        assert bob.final_response == "Bob private answer"
        assert bob.iterations == 1
        persisted = await store.list_trajectories(limit=10)
        assert {t.session_id: t.final_response for t in persisted} == {
            "alice": "Alice final", "bob": "Bob private answer",
        }
    finally:
        await storage.close()


@pytest.mark.asyncio
async def test_default_trajectory_redaction_does_not_persist_tool_payloads(tmp_path):
    storage = SQLiteBackend(tmp_path / "trajectories.db")
    await storage.initialize()
    store = TrajectoryStore(storage)
    await store.init_schema()
    recorder = TrajectoryRecorder(store, redact_args=True)
    secret = "test-private-token-never-persist"
    try:
        await recorder.begin_turn(session_key="alice", chat_id="alice", channel="gateway", task_input="task")
        await recorder._on_post_tool_call(
            SimpleNamespace(success=False, error=f"request token={secret}", text=f"response {secret}"),
            "http_request", {"headers": {"Authorization": secret}}, SimpleNamespace(session_key="alice"),
        )
        trajectory = await recorder.end_turn(session_key="alice", response_text="failed")
        assert trajectory.tools_called[0].args_digest
        rows = await storage.fetch_sql("SELECT data FROM evolution_trajectories")
        assert secret not in rows[0]["data"]
    finally:
        await storage.close()
