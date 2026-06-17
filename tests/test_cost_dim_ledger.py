"""Dimensional cost ledger: migration, incremental accumulation, isolation."""

from __future__ import annotations

import pytest

from echo_agent.storage.sqlite import SQLiteBackend


@pytest.mark.asyncio
async def test_migration_creates_cost_ledger_dim(tmp_path):
    storage = SQLiteBackend(tmp_path / "db.sqlite")
    await storage.initialize()
    rows = await storage.fetch_sql(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='cost_ledger_dim'"
    )
    assert len(rows) == 1
    await storage.close()
