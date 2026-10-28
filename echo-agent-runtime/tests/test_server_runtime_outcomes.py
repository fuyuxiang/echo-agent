"""Configuration availability is distinct from successful business execution."""
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
import yaml

from echo_agent.agent.loop import AgentLoop
from echo_agent.app import AppRuntime
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.models.stub import StubProvider
from echo_agent.storage.sqlite import SQLiteBackend


@pytest.mark.asyncio
async def test_unconfigured_model_is_incomplete_in_real_turn_ledger(tmp_path):
    config = Config.model_validate({'memory': {'enabled': False}, 'knowledge': {'enabled': False},
                                  'skills': {'enabled': False}, 'agent': {'heartbeat': {'enabled': False}}})
    storage = SQLiteBackend(tmp_path / 'state.db')
    await storage.initialize()
    agent = AgentLoop(MessageBus(), config, StubProvider('No model configured'), tmp_path, storage=storage)
    try:
        result = await agent.process_direct_result('hello', session_key='cli:unconfigured')
        assert result.task_incomplete
        rows = await storage.fetch_sql('SELECT status FROM turn_runs')
        assert rows and all(row['status'] != 'completed' for row in rows)
    finally:
        await agent.stop()
        await storage.close()


@pytest.mark.asyncio
async def test_shutdown_closes_all_registered_provider_owners_once():
    provider = SimpleNamespace(aclose=AsyncMock())
    other = SimpleNamespace(aclose=AsyncMock())
    ctx = SimpleNamespace(health=SimpleNamespace(stop=AsyncMock()), scheduler=None, task_dispatcher=None,
                          bus=SimpleNamespace(stop=AsyncMock()), channels=SimpleNamespace(stop_all=AsyncMock()),
                          agent=SimpleNamespace(stop=AsyncMock()), storage=SimpleNamespace(close=AsyncMock()),
                          providers=[provider, other, provider])
    runtime = AppRuntime(ctx)
    runtime._started = True
    await runtime.stop()
    provider.aclose.assert_awaited_once()
    other.aclose.assert_awaited_once()


def test_eval_cli_does_not_bootstrap_online_workspace(tmp_path, monkeypatch):
    from echo_agent.__main__ import _run_eval
    from echo_agent.models.provider import LLMProvider, LLMResponse
    class Model(LLMProvider):
        async def chat(self, *args, **kwargs):
            return LLMResponse(content='evaluation-ok')
        def get_default_model(self):
            return 'test'
    monkeypatch.setattr('echo_agent.models.providers.create_provider', lambda *a, **kw: Model())
    config = tmp_path / 'config.yaml'
    config.write_text(yaml.safe_dump({'workspace': str(tmp_path / 'online'),
        'models': {'default_model': 'test', 'providers': [{'name': 'openai', 'api_key': 'test', 'models': ['test']}]},
        'memory': {'enabled': False}, 'knowledge': {'enabled': False},
        'skills': {'enabled': False}, 'agent': {'heartbeat': {'enabled': False}}}))
    dataset = tmp_path / 'eval.yaml'
    dataset.write_text('- id: first\n  input: hi\n  expected_contains: [evaluation-ok]\n')
    args = SimpleNamespace(config=str(config), workspace=None, parallel=1, dataset=str(dataset), tag=None, output=None)
    assert _run_eval(args) == 0
    assert not (tmp_path / 'online').exists()


@pytest.mark.asyncio
async def test_runtime_shutdown_settles_all_owners_before_repeated_cancellation():
    import asyncio
    entered, release = asyncio.Event(), asyncio.Event()
    async def slow_stop():
        entered.set()
        await release.wait()
    storage = SimpleNamespace(close=AsyncMock())
    provider = SimpleNamespace(aclose=AsyncMock())
    ctx = SimpleNamespace(health=SimpleNamespace(stop=AsyncMock()), scheduler=None, task_dispatcher=None,
                          bus=SimpleNamespace(stop=slow_stop), channels=SimpleNamespace(stop_all=AsyncMock()),
                          agent=SimpleNamespace(stop=AsyncMock()), storage=storage, providers=[provider])
    runtime = AppRuntime(ctx)
    task = asyncio.create_task(runtime.stop())
    await entered.wait()
    task.cancel()
    await asyncio.sleep(0)
    task.cancel()
    release.set()
    with pytest.raises(asyncio.CancelledError):
        await task
    storage.close.assert_awaited_once()
    provider.aclose.assert_awaited_once()
    await runtime.stop()
    storage.close.assert_awaited_once()
