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
        # url after all redirects; defaults to the requested URL unless a
        # redirect target is injected to simulate a 30x hop.
        self._redirect_to = None
        self.url = ""

    async def goto(self, url, **kwargs):
        self.goto_url = url
        self.url = self._redirect_to or url

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
async def test_navigate_blocks_redirect_to_internal(monkeypatch):
    # Initial URL passes SSRF, but the page follows a 30x to an internal
    # address (page.url). The final-URL recheck must block and NOT return the
    # page content as a success.
    def _check(url):
        # public initial URL allowed, internal redirect target blocked
        if "169.254.169.254" in url:
            return _async_return("blocked: link-local address")
        return _async_return(None)

    monkeypatch.setattr(act_mod, "check_url_ssrf", _check)
    s = _session()
    s.page._redirect_to = "http://169.254.169.254/latest/meta-data/"
    err = await act_mod.navigate(s, "https://public.example.com")
    assert err != ""
    assert "redirect" in err.lower() or "blocked" in err.lower()


@pytest.mark.asyncio
async def test_navigate_allow_private_skips_recheck(monkeypatch):
    # allow_private=True must skip both initial and final-URL checks entirely.
    def _boom(url):  # pragma: no cover - must never be called
        raise AssertionError("check_url_ssrf should not run when allow_private")

    monkeypatch.setattr(act_mod, "check_url_ssrf", _boom)
    s = _session()
    s.page._redirect_to = "http://10.0.0.1/"
    err = await act_mod.navigate(s, "http://10.0.0.1/", allow_private=True)
    assert err == ""


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
