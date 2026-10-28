import time

import pytest

from echo_agent.agent.browser import session as sess_mod
from echo_agent.agent.browser.session import BrowserSession, _ssrf_route_handler


class FakeRequest:
    def __init__(self, url):
        self.url = url


class FakeRoute:
    def __init__(self, url):
        self.request = FakeRequest(url)
        self.aborted_with = None
        self.continued = False

    async def abort(self, code="failed"):
        self.aborted_with = code

    async def continue_(self):
        self.continued = True


def _mk_session(**kw):
    return BrowserSession(context=None, page=None, last_active=time.monotonic(), **kw)


@pytest.mark.asyncio
async def test_private_url_is_aborted_before_send():
    session = _mk_session()
    route = FakeRoute("http://127.0.0.1:8080/admin")
    await _ssrf_route_handler(session, route)
    assert route.aborted_with == "blockedbyclient"
    assert route.continued is False


@pytest.mark.asyncio
async def test_public_url_continues(monkeypatch):
    # Stub the resolver so the test doesn't depend on live DNS for example.com
    # (offline / sandboxed CI would otherwise see it blocked as unresolvable).
    async def allow(url):
        return None
    monkeypatch.setattr(sess_mod, "check_url_ssrf", allow)
    session = _mk_session()
    route = FakeRoute("https://example.com/")
    await _ssrf_route_handler(session, route)
    assert route.continued is True
    assert route.aborted_with is None


@pytest.mark.asyncio
async def test_allow_private_bypasses_check():
    session = _mk_session(allow_private=True)
    route = FakeRoute("http://169.254.169.254/latest/meta-data/")
    await _ssrf_route_handler(session, route)
    assert route.continued is True


@pytest.mark.asyncio
async def test_check_exception_fails_safe_abort(monkeypatch):
    async def boom(url):
        raise RuntimeError("resolver down")
    monkeypatch.setattr(sess_mod, "check_url_ssrf", boom)
    session = _mk_session()
    route = FakeRoute("https://example.com/")
    await _ssrf_route_handler(session, route)
    assert route.aborted_with == "blockedbyclient"


@pytest.mark.asyncio
async def test_host_cache_hit_skips_second_check(monkeypatch):
    calls = {"n": 0}
    async def counting(url):
        calls["n"] += 1
        return None
    monkeypatch.setattr(sess_mod, "check_url_ssrf", counting)
    session = _mk_session()
    await _ssrf_route_handler(session, FakeRoute("https://example.com/a"))
    await _ssrf_route_handler(session, FakeRoute("https://example.com/b"))
    assert calls["n"] == 1


@pytest.mark.asyncio
async def test_data_url_continues_without_ssrf_check(monkeypatch):
    """data: URLs are inline payloads, not a network SSRF vector — they must
    pass through untouched and never consult the SSRF resolver."""
    async def boom(url):
        raise AssertionError("check_url_ssrf must not be called for data: URLs")
    monkeypatch.setattr(sess_mod, "check_url_ssrf", boom)
    session = _mk_session()
    route = FakeRoute("data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB")
    await _ssrf_route_handler(session, route)
    assert route.continued is True
    assert route.aborted_with is None


@pytest.mark.asyncio
async def test_blob_url_continues_without_ssrf_check(monkeypatch):
    """blob: URLs reference in-page objects, not a network destination — they
    must pass through untouched and never consult the SSRF resolver."""
    async def boom(url):
        raise AssertionError("check_url_ssrf must not be called for blob: URLs")
    monkeypatch.setattr(sess_mod, "check_url_ssrf", boom)
    session = _mk_session()
    route = FakeRoute("blob:https://example.com/550e8400-e29b-41d4-a716-446655440000")
    await _ssrf_route_handler(session, route)
    assert route.continued is True
    assert route.aborted_with is None
