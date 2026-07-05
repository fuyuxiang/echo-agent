from __future__ import annotations

from typing import Any

# roles considered interactive → get a @eN ref
_INTERACTIVE = {"button", "link", "textbox", "checkbox", "radio", "combobox",
                "menuitem", "tab", "switch", "searchbox", "slider", "option"}


async def build_snapshot(page: Any, *, max_chars: int = 8000) -> tuple[str, dict[str, int]]:
    """Flatten the page accessibility tree into ref-annotated text + ref map."""
    try:
        tree = await page.accessibility.snapshot()
    except Exception:
        tree = None
    lines: list[str] = []
    ref_map: dict[str, int] = {}
    counter = 0

    def _walk(node: Any) -> None:
        nonlocal counter
        if not isinstance(node, dict):
            return
        role = node.get("role", "")
        name = (node.get("name") or "").strip()
        if role in _INTERACTIVE:
            counter += 1
            ref = f"@e{counter}"
            ref_map[ref] = counter
            lines.append(f"[{ref}] {role} '{name}'")
        elif name:
            lines.append(f"{role} '{name}'" if role else name)
        for child in node.get("children", []) or []:
            _walk(child)

    _walk(tree)
    text = "\n".join(lines)
    if len(text) > max_chars:
        text = text[:max_chars] + "\n…(快照内容过长已截断)"
    return text, ref_map


async def build_snapshot_with_locators(
    page: Any, *, max_chars: int = 8000
) -> tuple[str, dict[str, Any]]:
    """Flatten the AX tree into ref-annotated text AND a ``ref -> locator`` map,
    both produced from a *single* traversal so @eN and its locator can never
    drift apart.

    For every interactive node we build a Playwright locator with
    ``page.get_by_role(role, name=name).nth(k)`` where ``k`` is the 0-based
    occurrence index of that (role, name) pair in AX order. This replaces the
    old scheme where the snapshot text and a separate ``query_selector_all``
    positional map were built in two independent walks — that CSS query omitted
    roles like radio/menuitem/switch, which shifted every later ref by one and
    silently pointed clicks at the wrong element.
    """
    try:
        tree = await page.accessibility.snapshot()
    except Exception:
        tree = None
    lines: list[str] = []
    ref_map: dict[str, Any] = {}
    counter = 0
    role_name_seen: dict[tuple[str, str], int] = {}

    def _walk(node: Any) -> None:
        nonlocal counter
        if not isinstance(node, dict):
            return
        role = node.get("role", "")
        name = (node.get("name") or "").strip()
        if role in _INTERACTIVE:
            counter += 1
            ref = f"@e{counter}"
            key = (role, name)
            k = role_name_seen.get(key, 0)
            role_name_seen[key] = k + 1
            try:
                ref_map[ref] = page.get_by_role(role, name=name).nth(k)
            except Exception:
                # locator construction failed; ref simply won't resolve later
                pass
            lines.append(f"[{ref}] {role} '{name}'")
        elif name:
            lines.append(f"{role} '{name}'" if role else name)
        for child in node.get("children", []) or []:
            _walk(child)

    _walk(tree)
    text = "\n".join(lines)
    if len(text) > max_chars:
        text = text[:max_chars] + "\n…(快照内容过长已截断)"
    return text, ref_map
