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
    parts: list[str] = []
    for prefix in sorted(groups):
        parts.append(f"## {prefix}")
        for e in sorted(groups[prefix], key=lambda x: x.key):
            tags = f" [{', '.join(e.tags)}]" if e.tags else ""
            parts.append(f"- **{e.key}**{tags}: {e.content}")
        parts.append("")
    out = "\n".join(parts).rstrip()
    if len(out) > max_chars:
        out = out[:max_chars].rstrip() + "\n…(truncated)"
    return out
