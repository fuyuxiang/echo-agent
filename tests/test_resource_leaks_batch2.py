from __future__ import annotations

import asyncio
from datetime import datetime, timedelta

import pytest


@pytest.mark.asyncio
async def test_expire_session_loads_on_cache_miss(tmp_path):
    """SQLite 模式下,过期会话即使不在内存缓存,cleanup_expired 也应落库为 expired。"""
    from echo_agent.session.manager import SessionManager
    from echo_agent.storage.sqlite import SQLiteBackend

    backend = SQLiteBackend(tmp_path / "sessions.db")
    await backend.initialize()
    try:
        mgr = SessionManager(
            sessions_dir=tmp_path / "sessions",
            storage=backend,
            expiry_hours=1,
        )
        # 造一个 active 会话并落库,updated_at 设为 2 小时前(已过期)。
        sess = await mgr.get_or_create("telegram:c1")
        sess.updated_at = datetime.now() - timedelta(hours=2)
        await mgr.save(sess)
        # 清空内存缓存,模拟长驻进程里该 key 早已淘汰出缓存。
        mgr._cache.clear()

        count = await mgr.cleanup_expired()

        assert count == 1
        data = await backend.load_session("telegram:c1")
        assert data is not None
        assert data["status"] == "expired"
    finally:
        await backend.close()
