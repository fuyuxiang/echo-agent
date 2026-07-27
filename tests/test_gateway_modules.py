"""Tests for gateway modules: editor, health, media, session_policy."""
from __future__ import annotations

import time
from datetime import datetime, timedelta
from unittest.mock import AsyncMock, MagicMock, patch

import pytest


# ══════════════════════════════════════════════════════════════════════════════
# 1. ProgressiveEditor
# ══════════════════════════════════════════════════════════════════════════════


class TestProgressiveEditor:
    def _make(self):
        from echo_agent.gateway.editor import ProgressiveEditor

        bus = MagicMock()
        bus.publish_outbound = AsyncMock()
        editor = ProgressiveEditor(bus)
        return editor, bus

    @pytest.mark.asyncio
    async def test_start_edit(self):
        editor, bus = self._make()
        event_id = await editor.start_edit("telegram", "chat1", "Initial text")
        assert event_id  # non-empty string
        bus.publish_outbound.assert_called_once()
        published = bus.publish_outbound.call_args[0][0]
        assert published.text == "Initial text"
        assert published.is_final is False
        assert published.message_kind == "streaming"

    @pytest.mark.asyncio
    async def test_start_edit_tracks_last_edit(self):
        editor, bus = self._make()
        event_id = await editor.start_edit("telegram", "chat1", "text")
        key = f"telegram:chat1:{event_id}"
        assert key in editor._last_edit
        assert editor._last_edit[key] <= time.time()

    @pytest.mark.asyncio
    async def test_finalize(self):
        editor, bus = self._make()
        event_id = await editor.start_edit("telegram", "chat1", "draft")
        bus.publish_outbound.reset_mock()

        await editor.finalize("telegram", "chat1", event_id, "Final text")
        bus.publish_outbound.assert_called_once()
        published = bus.publish_outbound.call_args[0][0]
        assert published.text == "Final text"
        assert published.is_final is True
        assert published.message_kind == "final"
        assert published.edit_message_id == event_id

    @pytest.mark.asyncio
    async def test_finalize_cleans_up_state(self):
        editor, bus = self._make()
        event_id = await editor.start_edit("telegram", "chat1", "draft")
        key = f"telegram:chat1:{event_id}"
        assert key in editor._last_edit

        await editor.finalize("telegram", "chat1", event_id, "done")
        assert key not in editor._last_edit
        assert key not in editor._pending

    @pytest.mark.asyncio
    async def test_finalize_cancels_flush_task(self):
        editor, bus = self._make()
        event_id = await editor.start_edit("telegram", "chat1", "draft")
        key = f"telegram:chat1:{event_id}"

        # Simulate a pending flush task
        mock_task = MagicMock()
        mock_task.done.return_value = False
        mock_task.cancel = MagicMock()
        editor._flush_tasks[key] = mock_task

        await editor.finalize("telegram", "chat1", event_id, "final")
        mock_task.cancel.assert_called_once()

    @pytest.mark.asyncio
    async def test_update_immediate_when_interval_passed(self):
        editor, bus = self._make()
        event_id = await editor.start_edit("telegram", "chat1", "init")
        key = f"telegram:chat1:{event_id}"
        # Force last_edit to be far in the past
        editor._last_edit[key] = time.time() - 10
        bus.publish_outbound.reset_mock()

        await editor.update("telegram", "chat1", event_id, "updated text")
        bus.publish_outbound.assert_called_once()
        published = bus.publish_outbound.call_args[0][0]
        assert published.text == "updated text"
        assert published.edit_message_id == event_id

    @pytest.mark.asyncio
    async def test_update_debounced_when_too_fast(self):
        editor, bus = self._make()
        event_id = await editor.start_edit("telegram", "chat1", "init")
        key = f"telegram:chat1:{event_id}"
        # last_edit is recent (just set by start_edit)
        bus.publish_outbound.reset_mock()

        await editor.update("telegram", "chat1", event_id, "fast update")
        # Should NOT publish immediately
        bus.publish_outbound.assert_not_called()
        # Should be in pending
        assert editor._pending[key] == "fast update"


# ══════════════════════════════════════════════════════════════════════════════
# 2. GatewayHealthProvider
# ══════════════════════════════════════════════════════════════════════════════


class TestGatewayHealthProvider:
    def _make_gateway(self, is_running=True, channels=None, has_rate_limiter=True,
                      media_size=10.5, sessions=3, stale_sessions=0):
        gw = MagicMock()
        gw.is_running = is_running

        if channels:
            gw.channel_manager = MagicMock()
            gw.channel_manager.active_channels = channels
        else:
            gw.channel_manager = None

        if has_rate_limiter:
            gw.rate_limiter = MagicMock()
            gw.rate_limiter.get_stats.return_value = {"rpm": 10, "limit": 30}
        else:
            gw.rate_limiter = None

        gw.media_cache = MagicMock()
        gw.media_cache.get_size_mb.return_value = media_size

        gw.session_manager = MagicMock()
        # ``active_sessions`` is now a recency window, not an all-time total, so
        # rows need an ``updated_at`` to count. Fresh rows sit inside the window;
        # stale ones are old enough to fall out of it but still raise the total.
        now = datetime.now()
        rows = [
            {"updated_at": (now - timedelta(minutes=1)).isoformat()}
            for _ in range(sessions)
        ]
        rows += [
            {"updated_at": (now - timedelta(hours=2)).isoformat()}
            for _ in range(stale_sessions)
        ]
        gw.session_manager.list_sessions.return_value = rows
        # No async method
        gw.session_manager.list_sessions_async = None

        gw.hooks = MagicMock()
        gw.hooks.handler_count = 5
        gw.delivery_router = MagicMock()
        gw.delivery_router.rule_count = 2
        # Real sized mapping so the ws_clients count reflects attached clients.
        gw._ws_clients = {"gateway:cli:alice": object()}

        return gw

    @pytest.mark.asyncio
    async def test_check_healthy(self):
        from echo_agent.gateway.health import GatewayHealthProvider

        gw = self._make_gateway(is_running=True, channels=["telegram", "discord"])
        provider = GatewayHealthProvider(gw)
        result = await provider.check()

        assert result["status"] == "healthy"
        assert result["server_running"] is True
        assert result["active_channels"] == {"telegram": "active", "discord": "active"}
        assert result["rate_limiter"] == {"rpm": 10, "limit": 30}
        assert result["media_cache_mb"] == 10.5
        assert result["active_sessions"] == 3
        assert result["total_sessions"] == 3
        assert result["hooks_loaded"] == 5
        assert result["delivery_rules"] == 2
        # Attached interactive clients (TUI/web) surface for the ops "is the CLI
        # connected?" view — one client was wired in _make_gateway.
        assert result["ws_clients"] == 1

    @pytest.mark.asyncio
    async def test_check_active_sessions_excludes_stale(self):
        """Rows outside ACTIVE_SESSION_WINDOW raise the total but not the active
        count — the whole point of the window, since the old all-time total only
        ever grew and could never report an idle deployment."""
        from echo_agent.gateway.health import GatewayHealthProvider

        gw = self._make_gateway(channels=["telegram"], sessions=2, stale_sessions=5)
        result = await GatewayHealthProvider(gw).check()

        assert result["active_sessions"] == 2
        assert result["total_sessions"] == 7

    @pytest.mark.asyncio
    async def test_check_unparseable_timestamp_is_not_active(self):
        """A malformed ``updated_at`` must not raise: health checks have to keep
        answering even when one stored row is garbage."""
        from echo_agent.gateway.health import GatewayHealthProvider

        gw = self._make_gateway(channels=["telegram"], sessions=1)
        gw.session_manager.list_sessions.return_value += [
            {"updated_at": "not-a-timestamp"},
            {},
        ]
        result = await GatewayHealthProvider(gw).check()

        assert result["active_sessions"] == 1
        assert result["total_sessions"] == 3

    @pytest.mark.asyncio
    async def test_check_unhealthy(self):
        from echo_agent.gateway.health import GatewayHealthProvider

        gw = self._make_gateway(is_running=False, channels=["telegram"])
        provider = GatewayHealthProvider(gw)
        result = await provider.check()
        assert result["status"] == "unhealthy"

    @pytest.mark.asyncio
    async def test_check_degraded(self):
        from echo_agent.gateway.health import GatewayHealthProvider

        gw = self._make_gateway(is_running=True, channels=[])
        # channel_manager exists but active_channels is empty
        gw.channel_manager = MagicMock()
        gw.channel_manager.active_channels = []
        provider = GatewayHealthProvider(gw)
        result = await provider.check()
        assert result["status"] == "degraded"

    @pytest.mark.asyncio
    async def test_check_no_channel_manager(self):
        from echo_agent.gateway.health import GatewayHealthProvider

        gw = self._make_gateway(is_running=True)
        gw.channel_manager = None
        provider = GatewayHealthProvider(gw)
        result = await provider.check()
        # No channels at all => degraded
        assert result["status"] == "degraded"
        assert result["active_channels"] == {}

    @pytest.mark.asyncio
    async def test_check_no_rate_limiter(self):
        from echo_agent.gateway.health import GatewayHealthProvider

        gw = self._make_gateway(is_running=True, channels=["telegram"], has_rate_limiter=False)
        provider = GatewayHealthProvider(gw)
        result = await provider.check()
        assert result["rate_limiter"] == {}


# ══════════════════════════════════════════════════════════════════════════════
# 3. MediaCache
# ══════════════════════════════════════════════════════════════════════════════


class TestMediaCache:
    def _make(self, tmp_path, max_size_mb=500):
        from echo_agent.gateway.media import MediaCache
        return MediaCache(cache_dir=tmp_path, max_size_mb=max_size_mb)

    def test_init_creates_dir(self, tmp_path):
        cache_dir = tmp_path / "media_cache"
        from echo_agent.gateway.media import MediaCache
        MediaCache(cache_dir=cache_dir)
        assert cache_dir.exists()

    @pytest.mark.asyncio
    async def test_download_cache_hit(self, tmp_path):
        cache = self._make(tmp_path)
        import hashlib
        url = "https://cdn.example.com/image.png"
        url_hash = hashlib.sha256(url.encode()).hexdigest()[:16]
        platform_dir = tmp_path / "telegram"
        platform_dir.mkdir()
        cached_file = platform_dir / f"{url_hash}.png"
        cached_file.write_bytes(b"cached image data")

        result = await cache.download(url, "telegram")
        assert result is not None
        assert result == cached_file
        # Should not make any HTTP request (file already exists)

    @pytest.mark.asyncio
    async def test_download_cache_miss(self, tmp_path):
        cache = self._make(tmp_path)
        url = "https://cdn.example.com/photo.jpg"

        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.headers = {"Content-Type": "image/jpeg"}
        mock_resp.read = AsyncMock(return_value=b"jpeg data bytes")
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)

        mock_session = AsyncMock()
        mock_session.get = MagicMock(return_value=mock_resp)
        mock_session.__aenter__ = AsyncMock(return_value=mock_session)
        mock_session.__aexit__ = AsyncMock(return_value=False)

        with patch("echo_agent.gateway.media.aiohttp.ClientSession", return_value=mock_session):
            result = await cache.download(url, "telegram")
            assert result is not None
            assert result.exists()
            assert result.read_bytes() == b"jpeg data bytes"

    @pytest.mark.asyncio
    async def test_download_http_failure(self, tmp_path):
        cache = self._make(tmp_path)
        url = "https://cdn.example.com/gone.png"

        mock_resp = AsyncMock()
        mock_resp.status = 404
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)

        mock_session = AsyncMock()
        mock_session.get = MagicMock(return_value=mock_resp)
        mock_session.__aenter__ = AsyncMock(return_value=mock_session)
        mock_session.__aexit__ = AsyncMock(return_value=False)

        with patch("echo_agent.gateway.media.aiohttp.ClientSession", return_value=mock_session):
            result = await cache.download(url, "telegram")
            assert result is None

    @pytest.mark.asyncio
    async def test_cleanup_removes_oldest(self, tmp_path):
        # Create cache with very small max
        cache = self._make(tmp_path, max_size_mb=0)  # 0 bytes max => everything over

        platform_dir = tmp_path / "test"
        platform_dir.mkdir()

        # Create some files with different mtimes
        f1 = platform_dir / "old.png"
        f1.write_bytes(b"x" * 100)
        f2 = platform_dir / "new.png"
        f2.write_bytes(b"y" * 100)

        removed = await cache.cleanup()
        assert removed >= 1

    @pytest.mark.asyncio
    async def test_cleanup_no_action_within_limit(self, tmp_path):
        cache = self._make(tmp_path, max_size_mb=500)
        platform_dir = tmp_path / "test"
        platform_dir.mkdir()
        (platform_dir / "small.png").write_bytes(b"x" * 10)

        removed = await cache.cleanup()
        assert removed == 0

    def test_get_size_mb(self, tmp_path):
        cache = self._make(tmp_path)
        platform_dir = tmp_path / "test"
        platform_dir.mkdir()
        (platform_dir / "f1.bin").write_bytes(b"x" * 1024 * 1024)  # 1 MB

        size = cache.get_size_mb()
        assert 0.9 < size < 1.1

    def test_get_size_mb_empty(self, tmp_path):
        cache = self._make(tmp_path)
        assert cache.get_size_mb() == 0.0

    def test_get_cached_found(self, tmp_path):
        cache = self._make(tmp_path)
        import hashlib
        url = "https://cdn.example.com/file.pdf"
        url_hash = hashlib.sha256(url.encode()).hexdigest()[:16]
        sub = tmp_path / "platform"
        sub.mkdir()
        target = sub / f"{url_hash}.pdf"
        target.write_bytes(b"pdf data")

        result = cache.get_cached(url)
        assert result == target

    def test_get_cached_not_found(self, tmp_path):
        cache = self._make(tmp_path)
        result = cache.get_cached("https://cdn.example.com/missing.png")
        assert result is None


# ══════════════════════════════════════════════════════════════════════════════
# 4. SessionResetPolicy
# ══════════════════════════════════════════════════════════════════════════════


class TestSessionResetPolicy:
    def _make_config(self, mode="idle", daily_reset_hour=4, idle_timeout_minutes=60):
        config = MagicMock()
        config.mode = mode
        config.daily_reset_hour = daily_reset_hour
        config.idle_timeout_minutes = idle_timeout_minutes
        return config

    def _make_session(self, updated_at=None):
        from datetime import datetime

        session = MagicMock()
        session.updated_at = updated_at or datetime.now()
        session.metadata = {}
        session.clear = MagicMock()
        return session

    def test_mode_none_never_resets(self):
        from echo_agent.gateway.session_policy import SessionResetPolicy

        config = self._make_config(mode="none")
        policy = SessionResetPolicy(config)
        session = self._make_session(updated_at=datetime(2020, 1, 1))
        assert policy.should_reset(session) is False

    def test_mode_idle_resets_after_timeout(self):
        from echo_agent.gateway.session_policy import SessionResetPolicy

        config = self._make_config(mode="idle", idle_timeout_minutes=30)
        policy = SessionResetPolicy(config)
        # Session updated 31 minutes ago
        session = self._make_session(updated_at=datetime.now() - timedelta(minutes=31))
        assert policy.should_reset(session) is True

    def test_mode_idle_no_reset_within_timeout(self):
        from echo_agent.gateway.session_policy import SessionResetPolicy

        config = self._make_config(mode="idle", idle_timeout_minutes=30)
        policy = SessionResetPolicy(config)
        # Session updated 5 minutes ago
        session = self._make_session(updated_at=datetime.now() - timedelta(minutes=5))
        assert policy.should_reset(session) is False

    def test_mode_daily_resets_across_boundary(self):
        from echo_agent.gateway.session_policy import SessionResetPolicy

        config = self._make_config(mode="daily", daily_reset_hour=4)
        policy = SessionResetPolicy(config)

        now = datetime.now()
        # If it's past 4am today, session last active yesterday before 4am should reset
        if now.hour >= 4:
            last_active = now.replace(hour=3, minute=0, second=0) - timedelta(days=1)
        else:
            # Before 4am today; boundary was yesterday at 4am
            last_active = now - timedelta(days=1, hours=1)

        session = self._make_session(updated_at=last_active)
        assert policy.should_reset(session) is True

    def test_mode_daily_no_reset_same_period(self):
        from echo_agent.gateway.session_policy import SessionResetPolicy

        config = self._make_config(mode="daily", daily_reset_hour=4)
        policy = SessionResetPolicy(config)
        # Freeze "now" at midday so the recent session cannot straddle the 4am
        # daily boundary regardless of the wall-clock time the test runs at.
        fixed_now = datetime(2024, 1, 1, 12, 0, 0)
        with patch("echo_agent.gateway.session_policy.datetime") as mock_dt:
            mock_dt.now.return_value = fixed_now
            session = self._make_session(updated_at=fixed_now - timedelta(minutes=5))
            assert policy.should_reset(session) is False

    def test_mode_both_idle_triggers(self):
        from echo_agent.gateway.session_policy import SessionResetPolicy

        config = self._make_config(mode="both", idle_timeout_minutes=10)
        policy = SessionResetPolicy(config)
        session = self._make_session(updated_at=datetime.now() - timedelta(minutes=15))
        assert policy.should_reset(session) is True

    def test_mode_both_no_trigger(self):
        from echo_agent.gateway.session_policy import SessionResetPolicy

        config = self._make_config(mode="both", idle_timeout_minutes=60, daily_reset_hour=4)
        policy = SessionResetPolicy(config)
        # Freeze "now" at midday so neither the idle window nor the 4am daily
        # boundary trips regardless of the wall-clock time the test runs at.
        fixed_now = datetime(2024, 1, 1, 12, 0, 0)
        with patch("echo_agent.gateway.session_policy.datetime") as mock_dt:
            mock_dt.now.return_value = fixed_now
            session = self._make_session(updated_at=fixed_now - timedelta(minutes=5))
            assert policy.should_reset(session) is False

    @pytest.mark.asyncio
    async def test_reset_clears_session(self):
        from echo_agent.gateway.session_policy import SessionResetPolicy

        config = self._make_config(mode="idle")
        policy = SessionResetPolicy(config)
        session = self._make_session()
        manager = MagicMock()
        manager.save = AsyncMock()

        await policy.reset(session, manager)
        session.clear.assert_called_once()
        manager.save.assert_called_once_with(session)
        assert "last_reset_at" in session.metadata
        assert session.metadata["reset_count"] == 1

    @pytest.mark.asyncio
    async def test_reset_increments_count(self):
        from echo_agent.gateway.session_policy import SessionResetPolicy

        config = self._make_config(mode="idle")
        policy = SessionResetPolicy(config)
        session = self._make_session()
        session.metadata = {"reset_count": 5}
        manager = MagicMock()
        manager.save = AsyncMock()

        await policy.reset(session, manager)
        assert session.metadata["reset_count"] == 6
