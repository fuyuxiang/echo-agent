"""Shared fakes for browser tests.

These fakes mirror the *real* Playwright surface the code calls: ``frame.evaluate``
for snapshots (``page.accessibility`` was removed upstream in 1.57 and must never
be faked here again — doing so is what let a broken snapshot path ship green),
``locator``/``mouse``/``keyboard`` for actions, and context-level events.
"""

from __future__ import annotations

from typing import Any


def make_payload(entries: list[dict[str, Any]], *, url: str = "https://example.com/",
                 title: str = "Example") -> dict[str, Any]:
    return {"url": url, "title": title, "entries": entries}


def element(role: str, name: str, xpath: str, states: list[str] | None = None) -> dict[str, Any]:
    return {"kind": "element", "role": role, "name": name, "xpath": xpath,
            "states": states or []}


class FakeLocator:
    def __init__(self, selector: str = ""):
        self.selector = selector
        self.clicked = 0
        self.click_kwargs: dict[str, Any] = {}
        self.filled: str | None = None
        self.pressed: list[str] = []
        self.hovered = False
        self.selected: dict[str, Any] = {}
        self.files: list[str] | None = None
        self.raise_on: set[str] = set()

    def _maybe_raise(self, op: str) -> None:
        if op in self.raise_on:
            raise RuntimeError(f"{op} boom")

    async def click(self, **kwargs):
        self._maybe_raise("click")
        self.clicked += 1
        self.click_kwargs = kwargs

    async def fill(self, text, **kwargs):
        self._maybe_raise("fill")
        self.filled = text

    async def press(self, key, **kwargs):
        self._maybe_raise("press")
        self.pressed.append(key)

    async def hover(self, **kwargs):
        self._maybe_raise("hover")
        self.hovered = True

    async def select_option(self, **kwargs):
        self._maybe_raise("select_option")
        if "value" in kwargs and "reject_value" in self.raise_on:
            raise RuntimeError("no such value")
        self.selected = kwargs

    async def set_input_files(self, paths, **kwargs):
        self._maybe_raise("set_input_files")
        self.files = list(paths)

    def nth(self, index):
        return self

    @property
    def first(self):
        return self

    async def wait_for(self, **kwargs):
        self._maybe_raise("wait_for")


class FakeKeyboard:
    def __init__(self):
        self.pressed: list[str] = []

    async def press(self, key):
        self.pressed.append(key)


class FakeMouse:
    def __init__(self):
        self.wheels: list[tuple[float, float]] = []

    async def wheel(self, dx, dy):
        self.wheels.append((dx, dy))


class FakePage:
    """Minimal stand-in for playwright's async Page.

    ``snapshot_payloads`` is a list of dicts returned by successive
    ``evaluate`` calls that carry the snapshot JS; anything else routes to
    ``eval_results`` (or ``eval_default``).
    """

    def __init__(self, snapshot_payloads: list[dict[str, Any]] | None = None,
                 *, url: str = "https://example.com/"):
        self.url = url
        self.goto_url: str | None = None
        self.goto_exc: Exception | None = None
        self.reloaded = 0
        self.back = 0
        self.forward = 0
        self.wait_states: list[str] = []
        self.screenshot_kwargs: dict[str, Any] = {}
        self.screenshot_data = b"PNGDATA"
        self.screenshot_exc: Exception | None = None
        self.keyboard = FakeKeyboard()
        self.mouse = FakeMouse()
        self.locators: dict[str, FakeLocator] = {}
        self.evaluated: list[str] = []
        self.eval_results: dict[str, Any] = {}
        self.eval_default: Any = None
        self.eval_exc: Exception | None = None
        self._snapshot_payloads = list(snapshot_payloads or [])
        self._text_locator = FakeLocator("text")
        self.nav_kwargs: dict[str, Any] = {}
        self.wait_kwargs: dict[str, Any] = {}

    # -- snapshot / evaluate -------------------------------------------------
    @property
    def frames(self):
        return [self]

    async def evaluate(self, expression, *args):
        # The snapshot traversal is a big JS arrow function; identify it by a
        # marker only that script contains.
        if "INTERACTIVE_ROLE_ATTRS" in expression:
            if self._snapshot_payloads:
                payload = self._snapshot_payloads.pop(0)
                if isinstance(payload, Exception):
                    raise payload
                return payload
            return make_payload([])
        self.evaluated.append(expression)
        if self.eval_exc is not None:
            raise self.eval_exc
        if "document.images" in expression:
            return self.eval_results.get("images", [])
        for needle, value in self.eval_results.items():
            if needle in expression:
                return value
        return self.eval_default

    def locator(self, selector):
        return self.locators.setdefault(selector, FakeLocator(selector))

    def get_by_text(self, text, **kwargs):
        return self._text_locator

    # -- navigation ---------------------------------------------------------
    async def goto(self, url, **kwargs):
        self.nav_kwargs = kwargs
        if self.goto_exc is not None:
            raise self.goto_exc
        self.goto_url = url
        self.url = url

    async def go_back(self, **kwargs):
        self.nav_kwargs = kwargs
        self.back += 1

    async def go_forward(self, **kwargs):
        self.nav_kwargs = kwargs
        self.forward += 1

    async def reload(self, **kwargs):
        self.nav_kwargs = kwargs
        self.reloaded += 1

    async def wait_for_load_state(self, state, **kwargs):
        self.wait_kwargs = kwargs
        self.wait_states.append(state)

    async def screenshot(self, **kwargs):
        if self.screenshot_exc is not None:
            raise self.screenshot_exc
        self.screenshot_kwargs = kwargs
        return self.screenshot_data

    def on(self, event, handler):
        pass


class FakeContext:
    def __init__(self, page: FakePage | None = None):
        self.closed = False
        self.routes: list[str] = []
        self.events: dict[str, Any] = {}
        self.storage_paths: list[str] = []
        self.storage_exc: Exception | None = None
        self._page = page or FakePage()

    async def new_page(self):
        return self._page

    async def route(self, pattern, handler):
        self.routes.append(pattern)

    def on(self, event, handler):
        self.events[event] = handler

    async def storage_state(self, path=None):
        if self.storage_exc is not None:
            raise self.storage_exc
        if path:
            from pathlib import Path
            Path(path).write_text('{"cookies": [], "origins": []}')
            self.storage_paths.append(str(path))
        return {"cookies": [], "origins": []}

    async def close(self):
        self.closed = True


class FakeBrowser:
    def __init__(self, page: FakePage | None = None):
        self.closed = False
        self.contexts: list[FakeContext] = []
        self.context_kwargs: list[dict[str, Any]] = []
        self.new_context_exc: Exception | None = None
        self._page = page

    async def new_context(self, **kwargs):
        self.context_kwargs.append(kwargs)
        if self.new_context_exc is not None:
            exc, self.new_context_exc = self.new_context_exc, None
            raise exc
        # Real playwright rejects a storage_state file it cannot parse; mirror
        # that so the fallback-to-blank-context path is actually exercised.
        state = kwargs.get("storage_state")
        if isinstance(state, str):
            import json
            from pathlib import Path
            try:
                json.loads(Path(state).read_text())
            except Exception as e:
                raise RuntimeError(f"Error reading storage state: {e}") from e
        ctx = FakeContext(self._page)
        self.contexts.append(ctx)
        return ctx

    async def close(self):
        self.closed = True


class FakePlaywright:
    def __init__(self, page: FakePage | None = None, launch_exc: Exception | None = None):
        self.stopped = False
        self.launch_count = 0
        self._page = page
        self._launch_exc = launch_exc
        self.browser: FakeBrowser | None = None
        outer = self

        class _Chromium:
            @staticmethod
            async def launch(**kwargs):
                outer.launch_count += 1
                if outer._launch_exc is not None:
                    raise outer._launch_exc
                outer.browser = FakeBrowser(outer._page)
                return outer.browser

        self.chromium = _Chromium()

    async def stop(self):
        self.stopped = True


def patch_playwright(monkeypatch, pw: FakePlaywright) -> FakePlaywright:
    from echo_agent.agent.browser import session as sess_mod

    async def _start():
        return pw

    monkeypatch.setattr(sess_mod, "_start_playwright", _start)
    return pw


class Cfg:
    """Config double matching BrowserToolConfig's effective fields."""

    enabled = True
    max_sessions = 3
    max_total_sessions = 10
    session_idle_timeout_sec = 300
    max_snapshot_chars = 8000
    headless = True
    nav_timeout_sec = 30
    allow_private_addresses = False
    dialog_policy = "dismiss"
    allow_unsafe_evaluate = False
    persist_login_state = False
    viewport_width = 1280
    viewport_height = 800
    user_agent = ""
