import pytest

from echo_agent.agent.browser import actions as act_mod
from echo_agent.agent.browser.session import BrowserSession


class _FakeLocator:
    def __init__(self):
        self.clicked = False
        self.filled = None

    async def click(self, **kwargs):
        self.clicked = True

    async def fill(self, text, **kwargs):
        self.filled = text


class _FakePage:
    def __init__(self):
        self.goto_url = None

    async def goto(self, url, **kwargs):
        self.goto_url = url

    async def screenshot(self, **kwargs):
        return b"PNGDATA"


def _session():
    return BrowserSession(context=object(), page=_FakePage(), last_active=0.0)


@pytest.mark.asyncio
async def test_navigate_blocks_ssrf(monkeypatch):
    monkeypatch.setattr(act_mod, "check_url_ssrf",
                        lambda url: _async_return("blocked: private address"))
    s = _session()
    err = await act_mod.navigate(s, "http://169.254.169.254/", allow_private=False)
    assert "blocked" in err
    assert s.page.goto_url is None  # never navigated


@pytest.mark.asyncio
async def test_navigate_allows_public(monkeypatch):
    monkeypatch.setattr(act_mod, "check_url_ssrf", lambda url: _async_return(None))
    s = _session()
    err = await act_mod.navigate(s, "https://example.com")
    assert err == ""
    assert s.page.goto_url == "https://example.com"


@pytest.mark.asyncio
async def test_click_unknown_ref_errors():
    s = _session()
    err = await act_mod.click(s, "@e99")
    assert "ref" in err.lower() and "@e99" in err


@pytest.mark.asyncio
async def test_click_uses_ref_map():
    s = _session()
    loc = _FakeLocator()
    s.ref_map = {"@e1": loc}
    err = await act_mod.click(s, "@e1")
    assert err == ""
    assert loc.clicked is True


@pytest.mark.asyncio
async def test_type_uses_ref_map():
    s = _session()
    loc = _FakeLocator()
    s.ref_map = {"@e1": loc}
    err = await act_mod.type_text(s, "@e1", "hello")
    assert err == ""
    assert loc.filled == "hello"


@pytest.mark.asyncio
async def test_screenshot_returns_bytes():
    s = _session()
    data = await act_mod.screenshot(s)
    assert data == b"PNGDATA"


# helper: wrap a value in an awaitable for monkeypatched async check_url_ssrf
def _async_return(value):
    async def _coro():
        return value
    return _coro()
