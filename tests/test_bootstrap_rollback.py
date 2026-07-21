"""Contract: a bootstrap failure must release the instance lock and SQLite
connection it already opened, rather than leaking them."""
from __future__ import annotations

from pathlib import Path

import pytest

from echo_agent import app as app_module
from echo_agent.runtime_lock import acquire_instance_lock
from echo_agent.storage.sqlite import SQLiteBackend


@pytest.mark.asyncio
async def test_bootstrap_rolls_back_lock_and_storage_on_failure(tmp_path: Path, monkeypatch):
    opened: dict[str, SQLiteBackend] = {}

    real_init = SQLiteBackend.initialize

    async def spy_init(self):
        opened["storage"] = self
        await real_init(self)

    monkeypatch.setattr(SQLiteBackend, "initialize", spy_init)

    # Force a failure *after* provider construction (TaskManager is imported and
    # instantiated at echo_agent/app.py:176-178, past the provider loop).
    def boom(*a, **k):
        raise RuntimeError("injected bootstrap failure")

    monkeypatch.setattr("echo_agent.tasks.manager.TaskManager", boom)

    with pytest.raises(RuntimeError, match="injected bootstrap failure"):
        await app_module.bootstrap(
            overrides={"workspace": str(tmp_path)},
            single_instance=True,
            force=False,
            role="run",
        )

    # SQLite connection must be closed (is_connected -> _db is not None).
    assert opened["storage"].is_connected is False

    # The workspace instance lock must be free — re-acquiring must not conflict.
    lock = acquire_instance_lock(tmp_path, role="run")
    lock.release()
