from __future__ import annotations

import inspect


def test_bounded_retrieve_passes_both_keys():
    from echo_agent.agent.pipeline import context_stage
    src = inspect.getsource(context_stage.ContextStage._bounded_retrieve)
    assert "memory_scope=event.memory_scope" in src
    # Episodes are conversation-reset-bounded. The stable delivery/session key
    # would leak pre-reset episodes into the new conversation epoch.
    assert "episode_session_key=context_key or event.session_key" in src


def test_sync_retrieve_passes_both_keys():
    from echo_agent.agent.pipeline import context_stage
    src = inspect.getsource(context_stage.ContextStage.build)
    # sync 路径 retrieve 也须双键:可见性 memory_scope、episode 用 reset-bounded context_key
    assert "memory_scope=event.memory_scope" in src
    assert "episode_session_key=context_key" in src


def test_prefetch_passes_both_keys():
    from echo_agent.memory import prefetch
    src = inspect.getsource(prefetch.RetrievalPrefetcher.prefetch)
    assert "episode_session_key=session_key" in src
    assert "memory_scope=" in src
