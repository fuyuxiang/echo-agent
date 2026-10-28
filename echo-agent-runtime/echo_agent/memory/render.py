"""Deterministic MEMORY.md view rendered from ACTIVE store entries.

No LLM, pure function, idempotent — the same set of entries always renders
identically (explicit sort, no dict-order dependence). This is a human-facing
export view; it does NOT enter the prompt (the snapshot injects structured
segments directly). Single source of truth is the structured store.
"""
from __future__ import annotations
from echo_agent.memory.types import MemoryEntry, MemoryTier


def render_memory_md(entries: list[MemoryEntry], *, max_chars: int = 4000) -> str:
    active = [
        e for e in entries
        if not e.is_superseded and e.tier != MemoryTier.ARCHIVAL
    ]
    groups: dict[str, list[MemoryEntry]] = {}
    for e in active:
        prefix = e.key.split(":", 1)[0] if ":" in e.key else e.key
        groups.setdefault(prefix, []).append(e)
    # Render at entry boundaries.  Cutting the finished Markdown string at an
    # arbitrary character produced files ending halfway through a fact, which
    # looked like corrupt storage during incident review even though this is only
    # a view.  Keep the cap, but omit whole records and report how many.
    records: list[tuple[str, str]] = []
    for prefix in sorted(groups):
        for e in sorted(groups[prefix], key=lambda x: x.key):
            tags = f" [{', '.join(e.tags)}]" if e.tags else ""
            records.append((prefix, f"- **{e.key}**{tags}: {e.content}"))

    def _compose(selected: list[tuple[str, str]]) -> str:
        parts: list[str] = []
        previous = ""
        for prefix, line in selected:
            if prefix != previous:
                if parts:
                    parts.append("")
                parts.append(f"## {prefix}")
                previous = prefix
            parts.append(line)
        return "\n".join(parts).rstrip()

    full = _compose(records)
    if len(full) <= max_chars:
        return full

    selected: list[tuple[str, str]] = []
    for record in records:
        candidate = selected + [record]
        omitted = len(records) - len(candidate)
        notice = f"…(truncated) {omitted} complete entries omitted"
        body = _compose(candidate)
        if len(body) + 1 + len(notice) > max_chars:
            break
        selected = candidate

    omitted = len(records) - len(selected)
    notice = f"…(truncated) {omitted} complete entries omitted"
    body = _compose(selected)
    if not body:
        return notice[:max_chars]
    return f"{body}\n{notice}"
