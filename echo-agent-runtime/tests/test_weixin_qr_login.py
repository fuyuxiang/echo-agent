"""Tests for WeixinChannel.qr_login (static QR-code login flow for the CLI).

Covers:
  1. HTTP error fetching the QR code
  2. response carrying an error code (logged, still proceeds to poll)
  3. missing qrcode in the response
  4. confirmed status -> returns credentials
  5. expired status -> refreshes, then times out
  6. timeout with no confirmation
"""

from __future__ import annotations

from contextlib import asynccontextmanager

import pytest

from echo_agent.channels import weixin as wx
from echo_agent.channels.weixin import WeixinChannel


class _FakeResp:
    def __init__(self, *, status: int = 200, json_data: dict | None = None, text: str = ""):
        self.status = status
        self._json = json_data or {}
        self._text = text

    async def json(self, content_type=None):
        return self._json

    async def text(self):
        return self._text


class _FakeSession:
    """Minimal aiohttp.ClientSession stand-in returning queued responses."""

    def __init__(self, responses: list[_FakeResp]):
        self._responses = responses
        self.calls = 0

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        return False

    @asynccontextmanager
    async def get(self, url, *, params=None, headers=None):
        resp = self._responses[min(self.calls, len(self._responses) - 1)]
        self.calls += 1
        yield resp


@pytest.fixture
def _patch_env(monkeypatch):
    """Patch ClientSession factory, sleep and time so the loop is deterministic."""
    holder: dict = {}

    def install(responses: list[_FakeResp]):
        session = _FakeSession(responses)
        holder["session"] = session
        monkeypatch.setattr(wx.aiohttp, "ClientSession", lambda *a, **k: session)

        async def fake_sleep(_):
            return None

        monkeypatch.setattr(wx.asyncio, "sleep", fake_sleep)
        # time advances by 1s per call so a small timeout terminates the loop.
        ticks = iter(range(0, 10_000))
        monkeypatch.setattr(wx.time, "time", lambda: next(ticks))
        return session

    return install


class TestQrLogin:
    @pytest.mark.asyncio
    async def test_http_error_returns_none(self, _patch_env):
        _patch_env([_FakeResp(status=500, text="oops")])
        assert await WeixinChannel.qr_login() is None

    @pytest.mark.asyncio
    async def test_missing_qrcode_returns_none(self, _patch_env):
        _patch_env([_FakeResp(json_data={"errcode": 0})])
        assert await WeixinChannel.qr_login() is None

    @pytest.mark.asyncio
    async def test_error_code_then_missing_qrcode(self, _patch_env):
        _patch_env([_FakeResp(json_data={"errcode": 42, "errmsg": "bad"})])
        assert await WeixinChannel.qr_login() is None

    @pytest.mark.asyncio
    async def test_confirmed_returns_credentials(self, _patch_env):
        _patch_env([
            _FakeResp(json_data={"qrcode": "QR1", "qrcode_img_content": "data:img"}),
            _FakeResp(json_data={
                "status": "confirmed",
                "account_id": "bot@im.bot",
                "token": "bot@im.bot:tok",
                "user_id": "user@x",
            }),
        ])
        creds = await WeixinChannel.qr_login(timeout_seconds=10)
        assert creds == {
            "account_id": "bot@im.bot",
            "token": "bot@im.bot:tok",
            "base_url": "https://ilinkai.weixin.qq.com",
            "user_id": "user@x",
        }

    @pytest.mark.asyncio
    async def test_expired_refreshes_then_times_out(self, _patch_env):
        # initial fetch, then every status poll reports "expired"; the refresh
        # GET returns a fresh qrcode. After 3 expiries the loop bails out.
        responses = [
            _FakeResp(json_data={"qrcode": "QR1", "qrcode_img_content": "x"}),
            _FakeResp(json_data={"status": "expired"}),
            _FakeResp(json_data={"qrcode": "QR2", "qrcode_img_content": "y"}),
        ]
        _patch_env(responses)
        assert await WeixinChannel.qr_login(timeout_seconds=100) is None

    @pytest.mark.asyncio
    async def test_scaned_then_timeout(self, _patch_env):
        _patch_env([
            _FakeResp(json_data={"qrcode": "QR1", "qrcode_img_content": "x"}),
            _FakeResp(json_data={"status": "scaned"}),
        ])
        # timeout_seconds small so the 1s-per-tick clock ends the loop quickly.
        assert await WeixinChannel.qr_login(timeout_seconds=5) is None
