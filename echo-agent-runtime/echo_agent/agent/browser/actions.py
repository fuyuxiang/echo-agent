"""Browser actions: navigation, interaction, capture, evaluation.

Each action returns an error string ("" on success) rather than raising, so the
tool layer can hand the model an actionable message and keep the session alive.
"""

from __future__ import annotations

import re
from typing import Any

from loguru import logger

from echo_agent.agent.browser.snapshot import (
    VERIFY_REF_JS,
    RefHandle,
    build_page_snapshot,
)
from echo_agent.agent.tools.web import check_url_ssrf

# Keys accepted by press. Playwright takes any key name plus modifier combos
# ("Control+a"); we validate loosely to reject obvious garbage while still
# allowing single printable characters and modifier chords.
_KEY_RE = re.compile(r"^(?:[A-Za-z][A-Za-z0-9]*|\S)(?:\+(?:[A-Za-z][A-Za-z0-9]*|\S))*$")

# Expressions that read credentials or drive navigation are refused: evaluate is
# an arbitrary-code escape hatch inside a page that may itself be attacker
# controlled, so prompt injection must not be able to use it to exfiltrate
# tokens or hop to an internal address behind the SSRF gate.
#
# This is DEFENCE IN DEPTH against careless and lightly-obfuscated expressions,
# NOT a sandbox: any string blacklist over a Turing-complete language can be
# defeated (`document["c"+"ookie"]`, `atob(...)`, aliasing through a helper).
# The real boundary is the approval gate plus browser.allow_evaluate — see
# BrowserTool.execution_mode.
# Needles are matched after stripping every non-alphanumeric character, so
# quoting, bracket access and comments between the tokens no longer slip past.
_EVAL_DENY = (
    ("documentcookie", "读取 cookie"),
    ("localstorage", "读取 localStorage"),
    ("sessionstorage", "读取 sessionStorage"),
    ("indexeddb", "读取 IndexedDB"),
    ("locationhref", "读写 location"),
    ("locationreplace", "脚本跳转"),
    ("locationassign", "脚本跳转"),
    ("windowopen", "脚本开窗"),
    ("navigatorcredentials", "读取凭据"),
    ("navigatorsendbeacon", "外发数据"),
    ("xmlhttprequest", "外发数据"),
    ("importscripts", "加载外部脚本"),
    ("serviceworker", "注册 service worker"),
    ("documentwrite", "注入内容"),
)
# Constructs whose only purpose here is to hide one of the above. Matched against
# the raw expression because the stripped form loses the call syntax.
_EVAL_SUSPICIOUS = (
    (re.compile(r"\batob\s*\("), "base64 解码后执行"),
    (re.compile(r"\bFunction\s*\("), "动态构造函数"),
    (re.compile(r"\beval\s*\("), "嵌套 eval"),
    (re.compile(r"\bfromCharCode\b"), "字符码拼接"),
    (re.compile(r"\bunescape\s*\("), "转义还原后执行"),
)
_EVAL_STRIP_RE = re.compile(r"[^a-z0-9]+")
_MAX_EVAL_RESULT_CHARS = 4000

# Result scrubbing. Even an allowed expression can return a page value that
# happens to contain a bearer token or session id; those must not land in the
# model's context (and from there in a transcript or a log).
_SECRET_PATTERNS = (
    re.compile(r"\b(?:ey[A-Za-z0-9_-]{8,}\.){2}[A-Za-z0-9_-]{8,}\b"),  # JWT
    re.compile(r"\b(?:sk|pk|ghp|gho|xox[abps])[-_][A-Za-z0-9_-]{16,}\b"),
    re.compile(r"(?i)\b(?:bearer|token|api[-_]?key|secret|password|passwd|authorization)"
               r"\b\s*[:=]\s*[\"']?([A-Za-z0-9_\-./+=]{12,})[\"']?"),
)


async def navigate(session: Any, url: str, *, timeout_sec: int = 30,
                   allow_private: bool = False) -> str:
    if not allow_private:
        error = await check_url_ssrf(url)
        if error:
            return f"navigation blocked: {error}"
    try:
        await session.page.goto(url, timeout=timeout_sec * 1000)
    except Exception as e:
        # The context-level route interceptor aborts SSRF targets (main nav,
        # redirect hops, subresources) with net::ERR_BLOCKED_BY_CLIENT, which
        # surfaces here as a goto failure. Distinguish it from a genuine network
        # error so the model gets an actionable reason.
        if "ERR_BLOCKED_BY_CLIENT" in str(e):
            return "navigation blocked: 目标地址被拦截（SSRF 防护）"
        return f"navigation failed: {e}"
    return ""


_STALE_REF_HINT = ("页面结构已变化，{ref} 现在指向的不是快照里的 {want}。"
                   "请重新 snapshot 再操作（避免误触其它元素）")


async def _resolve_ref(session: Any, ref: str) -> tuple[Any, str]:
    """Resolve a ``@eN`` to a locator, refusing it if the DOM has drifted.

    The handle's locator is a lazy absolute XPath: it re-resolves at action time,
    so inserting a node earlier in the document silently shifts it onto a
    *different* element than the one the snapshot described. On a dynamic page
    that turns a "click 取消" into a click on 删除. Re-reading the identity at the
    recorded path and comparing it against capture time closes that window down
    to the few ms between this probe and the action itself.
    """
    handle = session.ref_map.get(ref)
    if handle is None:
        known = len(session.ref_map)
        return None, (f"ref {ref} 不存在（当前快照共 {known} 个可交互元素），"
                      "请重新 snapshot 后使用最新的 @eN")
    if not isinstance(handle, RefHandle):
        # Tolerate a bare locator (older snapshots / test doubles): no identity
        # was captured, so there is nothing to verify against.
        return handle, ""
    stale = await _ref_is_stale(handle)
    if stale:
        want = f"{handle.role} {handle.name!r}"
        return None, _STALE_REF_HINT.format(ref=ref, want=want) + f"（{stale}）"
    return handle.locator, ""


async def _ref_is_stale(handle: RefHandle) -> str:
    """Return a short reason when *handle* no longer matches what was captured.

    Empty string means "verified, or could not be checked" — a probe that itself
    fails must not block the action, or a detached frame would make the whole
    page unusable instead of just letting the action report its own error.
    """
    try:
        probe = await handle.frame.evaluate(VERIFY_REF_JS, handle.xpath)
    except Exception as e:
        logger.debug("ref verify probe failed for {}: {}", handle.xpath, e)
        return ""
    if not isinstance(probe, dict):
        return ""
    if not probe.get("found"):
        return "元素已不存在"
    role = str(probe.get("role") or "")
    name = str(probe.get("name") or "")
    if role != handle.role:
        return f"该位置现在是 {role or '非交互元素'}"
    # Names are compared strictly on purpose. Loosening it enough to tolerate a
    # live counter ("重新发送 (30s)" → "(29s)") would also make sibling rows
    # ("删除第1行" / "删除第2行") look like the same element, which is the exact
    # misclick this guard exists to prevent. A changed label costs one extra
    # snapshot; a wrong click can be irreversible.
    if name != handle.name and not _names_compatible(handle.name, name):
        return f"名称已从 {handle.name!r} 变为 {name!r}"
    return ""


def _names_compatible(captured: str, current: str) -> bool:
    """True when two accessible names are close enough to be the same element."""
    if not captured or not current:
        # An element that never had a name (icon button) cannot be distinguished
        # by name; the role match above is all the evidence available.
        return True
    a, b = captured.strip(), current.strip()
    if a == b:
        return True
    # Truncated capture (…) or one being a prefix of the other: same label with
    # a changing tail (counts, elapsed time).
    a_core = a.rstrip("…")
    return bool(a_core) and (b.startswith(a_core) or a.startswith(b.rstrip("…")))


async def click(session: Any, ref: str, *, button: str = "left",
                click_count: int = 1, timeout_ms: int = 10000) -> str:
    loc, err = await _resolve_ref(session, ref)
    if err:
        return err
    try:
        await loc.click(button=button, click_count=click_count, timeout=timeout_ms)
    except Exception as e:
        return f"click failed: {e}"
    return ""


async def type_text(session: Any, ref: str, text: str, *,
                    press_enter: bool = False, timeout_ms: int = 10000) -> str:
    """Fill a field, then optionally type-and-Enter.

    ``fill`` is used for the value (fast, clears first) but it does not emit key
    events, so frameworks that only react to keystrokes never see the input. When
    *press_enter* is set we additionally dispatch a real Enter on the element,
    which is how most search boxes submit.
    """
    loc, err = await _resolve_ref(session, ref)
    if err:
        return err
    try:
        await loc.fill(text, timeout=timeout_ms)
    except Exception as e:
        return f"type failed: {e}"
    if press_enter:
        try:
            await loc.press("Enter", timeout=timeout_ms)
        except Exception as e:
            return f"type ok but Enter failed: {e}"
    return ""


async def press_key(session: Any, key: str, *, ref: str = "",
                    timeout_ms: int = 10000) -> str:
    """Press a key on a specific element, or on the page when no ref is given."""
    if not key or not _KEY_RE.match(key):
        return f"无效按键: {key!r}（示例: Enter / Tab / Escape / ArrowDown / Control+a）"
    if ref:
        loc, err = await _resolve_ref(session, ref)
        if err:
            return err
        try:
            await loc.press(key, timeout=timeout_ms)
        except Exception as e:
            return f"press failed: {e}"
        return ""
    try:
        await session.page.keyboard.press(key)
    except Exception as e:
        return f"press failed: {e}"
    return ""


async def scroll(session: Any, direction: str, *, amount: int = 0) -> str:
    """Scroll the page. ``amount`` is in pixels; 0 means one viewport step."""
    deltas = {
        "down": (0, 1), "up": (0, -1),
        "right": (1, 0), "left": (-1, 0),
    }
    if direction in ("top", "bottom"):
        try:
            target = "0" if direction == "top" else "document.body.scrollHeight"
            await session.page.evaluate(f"window.scrollTo(0, {target})")
        except Exception as e:
            return f"scroll failed: {e}"
        return ""
    if direction not in deltas:
        return f"无效滚动方向: {direction}（可选 up/down/left/right/top/bottom）"
    step = amount if amount > 0 else 600
    dx, dy = deltas[direction]
    try:
        await session.page.mouse.wheel(dx * step, dy * step)
    except Exception as e:
        return f"scroll failed: {e}"
    return ""


async def go_back(session: Any, *, timeout_sec: int = 30) -> str:
    try:
        await session.page.go_back(timeout=timeout_sec * 1000)
    except Exception as e:
        if "ERR_BLOCKED_BY_CLIENT" in str(e):
            return "back blocked: 目标地址被拦截（SSRF 防护）"
        return f"back failed: {e}"
    return ""


async def go_forward(session: Any, *, timeout_sec: int = 30) -> str:
    try:
        await session.page.go_forward(timeout=timeout_sec * 1000)
    except Exception as e:
        if "ERR_BLOCKED_BY_CLIENT" in str(e):
            return "forward blocked: 目标地址被拦截（SSRF 防护）"
        return f"forward failed: {e}"
    return ""


async def reload(session: Any, *, timeout_sec: int = 30) -> str:
    try:
        await session.page.reload(timeout=timeout_sec * 1000)
    except Exception as e:
        return f"reload failed: {e}"
    return ""


async def hover(session: Any, ref: str, *, timeout_ms: int = 10000) -> str:
    loc, err = await _resolve_ref(session, ref)
    if err:
        return err
    try:
        await loc.hover(timeout=timeout_ms)
    except Exception as e:
        return f"hover failed: {e}"
    return ""


async def select_option(session: Any, ref: str, values: list[str], *,
                        timeout_ms: int = 10000) -> str:
    loc, err = await _resolve_ref(session, ref)
    if err:
        return err
    if not values:
        return "select 需要至少一个 value（选项文本或 value 属性）"
    try:
        # Try by value first, then by visible label — the model usually copies
        # the label it saw in the snapshot, which is not the value attribute.
        try:
            await loc.select_option(value=values, timeout=timeout_ms)
        except Exception:
            await loc.select_option(label=values, timeout=timeout_ms)
    except Exception as e:
        return f"select failed: {e}"
    return ""


async def upload_files(session: Any, ref: str, paths: list[str], *,
                       timeout_ms: int = 10000) -> str:
    loc, err = await _resolve_ref(session, ref)
    if err:
        return err
    if not paths:
        return "upload 需要至少一个文件路径"
    try:
        await loc.set_input_files(paths, timeout=timeout_ms)
    except Exception as e:
        return f"upload failed: {e}"
    return ""


async def wait_for(session: Any, *, text: str = "", state: str = "",
                   timeout_sec: int = 15) -> str:
    """Wait for page text to appear, or for a load state to settle."""
    timeout_ms = timeout_sec * 1000
    if text:
        try:
            await session.page.get_by_text(text, exact=False).first.wait_for(
                state="visible", timeout=timeout_ms)
        except Exception as e:
            return f"wait failed: 未在 {timeout_sec}s 内出现文本 {text!r} ({e})"
        return ""
    target = state or "networkidle"
    if target not in ("load", "domcontentloaded", "networkidle"):
        return f"无效等待状态: {target}（可选 load/domcontentloaded/networkidle）"
    try:
        await session.page.wait_for_load_state(target, timeout=timeout_ms)
    except Exception as e:
        return f"wait failed: {e}"
    return ""


def check_eval_expression(expression: str, *, allow_unsafe: bool = False) -> str:
    """Return a refusal reason for a disallowed expression, "" when allowed.

    See _EVAL_DENY: this filters mistakes and casual obfuscation, it does not
    contain a determined attacker. Callers must not treat a "" return as proof
    the expression is safe.
    """
    if not expression.strip():
        return "expression 不能为空"
    if allow_unsafe:
        return ""
    stripped = _EVAL_STRIP_RE.sub("", expression.lower())
    for needle, reason in _EVAL_DENY:
        if needle in stripped:
            return (f"evaluate 被拒绝：表达式涉及{reason}。"
                    "如确需此操作，请在配置中开启 browser.allow_unsafe_evaluate")
    for pattern, reason in _EVAL_SUSPICIOUS:
        if pattern.search(expression):
            return (f"evaluate 被拒绝：表达式涉及{reason}，无法静态判断其行为。"
                    "请改写为直接表达式，或在配置中开启 browser.allow_unsafe_evaluate")
    return ""


def scrub_eval_result(text: str) -> str:
    """Redact credential-shaped substrings from an evaluate result."""
    def _mask(m: re.Match[str]) -> str:
        whole = m.group(0)
        if m.groups():
            # Keyed form (token: xxx): keep the key, drop the value.
            secret = m.group(1)
            return whole.replace(secret, "***")
        return "***"

    for pattern in _SECRET_PATTERNS:
        text = pattern.sub(_mask, text)
    return text


async def evaluate(session: Any, expression: str, *,
                   allow_unsafe: bool = False) -> tuple[str, str]:
    """Evaluate JS in the page. Returns ``(result_text, error)``."""
    refusal = check_eval_expression(expression, allow_unsafe=allow_unsafe)
    if refusal:
        return "", refusal
    try:
        result = await session.page.evaluate(expression)
    except Exception as e:
        return "", f"evaluate failed: {e}"
    if result is None:
        # repr(None) would show the model "None" for a JS undefined/void result.
        text = "undefined"
    elif isinstance(result, str):
        # An empty string is a real result; quote it so it is not mistaken for
        # "no result at all".
        text = result or '""'
    else:
        text = repr(result)
    if len(text) > _MAX_EVAL_RESULT_CHARS:
        text = text[:_MAX_EVAL_RESULT_CHARS] + "…(结果过长已截断)"
    # Scrub AFTER truncation so a redaction can never be cut in half, leaving a
    # partial secret behind.
    return scrub_eval_result(text), ""


async def screenshot(session: Any, *, full_page: bool = False) -> bytes:
    try:
        return await session.page.screenshot(full_page=full_page)
    except Exception as e:
        logger.debug("screenshot failed: {}", e)
        return b""


async def get_images(session: Any, *, limit: int = 50) -> list[dict[str, str]]:
    """List images on the page so the model can pick one for vision analysis."""
    try:
        rows = await session.page.evaluate(
            """(limit) => Array.from(document.images)
                 .filter(i => i.currentSrc && i.naturalWidth > 32 && i.naturalHeight > 32)
                 .slice(0, limit)
                 .map(i => ({url: i.currentSrc, alt: (i.alt || '').trim(),
                             w: i.naturalWidth, h: i.naturalHeight}))""",
            limit,
        )
    except Exception as e:
        logger.debug("get_images failed: {}", e)
        return []
    return rows if isinstance(rows, list) else []


async def refresh_snapshot(session: Any, *, max_chars: int = 8000) -> str:
    """Rebuild the snapshot text AND populate session.ref_map with locators.

    ``build_page_snapshot`` returns (text, {ref: locator}) from a single in-page
    DOM traversal, so the @eN in the text and the locator that click/type will
    drive always describe the same element. Any dialog the page raised since the
    last snapshot is surfaced here — it was auto-answered to keep the page from
    blocking, and the model needs to know it happened.
    """
    text, ref_map = await build_page_snapshot(session.page, max_chars=max_chars)
    session.ref_map = ref_map
    prefix_parts: list[str] = []
    for entry in session.take_dialogs():
        prefix_parts.append(
            f"(已自动{'确认' if session.dialog_policy == 'accept' else '取消'}"
            f"弹窗 {entry.get('type', '')}: {entry.get('message', '')})"
        )
    if prefix_parts:
        return "\n".join(prefix_parts) + "\n" + text
    return text
