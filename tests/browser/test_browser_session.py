import time

import pytest

from echo_agent.agent.browser import session as sess_mod
from echo_agent.agent.browser.session import BrowserSessionManager


class _FakeContext:
    def __init__(self):
        self.closed = False

    async def new_page(self):
        return object()

    async def close(self):
        self.closed = True


class _FakeBrowser:
    def __init__(self):
        self.closed = False

    async def new_context(self):
        return _FakeContext()

    async def close(self):
        self.closed = True


class _FakePW:
    def __init__(self):
        self.stopped = False
        self._browser = _FakeBrowser()

    class chromium:
        @staticmethod
        async def launch(**kwargs):
            return _FakeBrowser()

    async def stop(self):
        self.stopped = True


async def _fake_start():
    return _FakePW()


@pytest.fixture(autouse=True)
def _patch_pw(monkeypatch):
    # patch the manager's playwright bootstrap to avoid real browser
    monkeypatch.setattr(sess_mod, "_start_playwright", _fake_start)


@pytest.mark.asyncio
async def test_open_returns_id_and_stores_session():
    m = BrowserSessionManager()
    sid = await m.open()
    assert sid.startswith("sess_")
    assert m.get(sid) is not None


@pytest.mark.asyncio
async def test_close_removes_session_and_closes_context():
    m = BrowserSessionManager()
    sid = await m.open()
    ctx = m.get(sid).context
    assert await m.close(sid) is True
    assert m.get(sid) is None
    assert ctx.closed is True


@pytest.mark.asyncio
async def test_close_unknown_returns_false():
    m = BrowserSessionManager()
    assert await m.close("nope") is False


@pytest.mark.asyncio
async def test_reap_idle_closes_stale_keeps_active():
    m = BrowserSessionManager()
    stale = await m.open()
    fresh = await m.open()
    m.get(stale).last_active = time.monotonic() - 1000
    await m._reap_idle(idle_timeout_sec=300)
    assert m.get(stale) is None
    assert m.get(fresh) is not None


@pytest.mark.asyncio
async def test_enforce_limit():
    m = BrowserSessionManager()
    await m.open()
    await m.open()
    assert m._enforce_limit(max_sessions=3) is True  # 2 < 3, room
    await m.open()
    assert m._enforce_limit(max_sessions=3) is False  # 3 >= 3, full


@pytest.mark.asyncio
async def test_close_all_empties():
    m = BrowserSessionManager()
    await m.open()
    await m.open()
    await m.close_all()
    assert m.get_count() == 0


@pytest.mark.asyncio
async def test_close_all_stops_browser_and_playwright():
    m = BrowserSessionManager()
    await m.open()
    browser = m._browser
    pw = m._pw
    assert browser is not None and pw is not None
    await m.close_all()
    assert browser.closed is True
    assert pw.stopped is True
    assert m._browser is None
    assert m._pw is None


@pytest.mark.asyncio
async def test_close_all_reopens_after_stop():
    # After close_all resets the process handles, a fresh open must re-bootstrap.
    m = BrowserSessionManager()
    await m.open()
    await m.close_all()
    sid = await m.open()
    assert m.get(sid) is not None
    assert m._browser is not None and m._pw is not None
