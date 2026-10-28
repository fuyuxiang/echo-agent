"""Read-only CLI actions must close the real bootstrap's unstarted resources."""

import pytest
import yaml

from echo_agent import app
from echo_agent.cli import evolution_cmd, skill_admission_cmd
from echo_agent.models.stub import StubProvider
from echo_agent.evolution.types import EvolutionRun


@pytest.mark.asyncio
@pytest.mark.parametrize("action", ["evolution-status", "skill-list"])
async def test_unstarted_cli_closes_provider_and_real_sqlite(tmp_path, monkeypatch, action):
    class RecordingProvider(StubProvider):
        closed = False

        async def aclose(self):
            self.closed = True

    provider = RecordingProvider("external model is not called in this test")
    monkeypatch.setattr("echo_agent.models.providers.create_provider", lambda *a, **k: provider)
    config = tmp_path / "echo-agent.yaml"
    config.write_text(yaml.safe_dump({
        "workspace": str(tmp_path),
        "models": {"providers": [{"name": "openai", "api_key": "test"}]},
        "memory": {"vector_enabled": False, "rerank_enabled": False},
        "plugins": {"enabled": False}, "scheduler": {"enabled": False},
        "evolution": {"enabled": False}, "checkpoint": {"enabled": False},
    }))
    real_bootstrap = app.bootstrap
    contexts = []

    async def bootstrap(**kwargs):
        ctx = await real_bootstrap(**kwargs)
        contexts.append(ctx)
        return ctx

    monkeypatch.setattr(app, "bootstrap", bootstrap)
    if action == "evolution-status":
        assert await evolution_cmd._status(str(config), None) == 1
    else:
        assert await skill_admission_cmd._list_staged(str(config), None) == 0
    # Assert after the action; clean up on failure so the failing regression
    # itself never leaves aiosqlite's worker thread behind.
    try:
        assert provider.closed
        assert contexts[0].storage._db is None
    finally:
        await contexts[0].agent.stop(close_shared_browser_sessions=False)
        await contexts[0].storage.close()


@pytest.mark.asyncio
async def test_readonly_evolution_status_does_not_start_threshold_execution(tmp_path, monkeypatch):
    config = tmp_path / "echo-agent.yaml"
    config.write_text(yaml.safe_dump({
        "workspace": str(tmp_path), "plugins": {"enabled": False},
        "memory": {"vector_enabled": False, "rerank_enabled": False},
        "evolution": {"enabled": True, "trigger_mode": "threshold", "threshold_trajectories": 1},
        "checkpoint": {"enabled": False},
    }))
    real_bootstrap = app.bootstrap
    triggered = []

    async def bootstrap(**kwargs):
        ctx = await real_bootstrap(**kwargs)

        async def count():
            return 1

        async def run(**kwargs):
            triggered.append(kwargs)
            return EvolutionRun()

        # Keep the real EvolutionEngine, scheduler, queries and persistence.
        # A qualifying count and recorded execution isolate model expense only.
        ctx.agent.evolution._scheduler._count_fn = count
        ctx.agent.evolution._scheduler._run_fn = run
        return ctx

    monkeypatch.setattr(app, "bootstrap", bootstrap)
    assert await evolution_cmd._status(str(config), None) == 0
    assert triggered == []
