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
