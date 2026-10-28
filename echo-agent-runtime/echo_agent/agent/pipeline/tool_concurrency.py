"""Decide which tool calls in one batch may run concurrently.

Pure functions, no IO. The inference loop computes each tool's metadata
(read_only flag from execution_mode, approval verdict, path args) and asks
this module to split the batch into a concurrent group and a serial group.
"""
from __future__ import annotations

from dataclasses import dataclass

_PATH_KEYS = ("path", "file", "filename", "directory", "dir")


@dataclass
class ToolPlan:
    index: int
    name: str
    read_only: bool
    paths: list[str]
    approved: bool = True


def extract_paths(params: dict) -> list[str]:
    out: list[str] = []
    if not isinstance(params, dict):
        return out
    for key in _PATH_KEYS:
        val = params.get(key)
        if isinstance(val, str) and val.strip():
            out.append(val.strip())
    return out


def _norm(p: str) -> str:
    # Normalize for parent/child comparison: collapse trailing slash, posix sep.
    return p.replace("\\", "/").rstrip("/")


def paths_overlap(a: list[str], b: list[str]) -> bool:
    for pa in a:
        na = _norm(pa)
        for pb in b:
            nb = _norm(pb)
            if na == nb:
                return True
            # parent/child: one is a path-segment prefix of the other
            if nb.startswith(na + "/") or na.startswith(nb + "/"):
                return True
    return False


def partition_concurrent(
    candidates: list[ToolPlan],
) -> tuple[list[ToolPlan], list[ToolPlan]]:
    # Side-effect tools always serial; collect their paths first so an
    # overlapping reader can be demoted.
    side_effect_paths: list[str] = []
    for c in candidates:
        if not c.read_only:
            side_effect_paths.extend(c.paths)

    concurrent: list[ToolPlan] = []
    serial: list[ToolPlan] = []
    for c in candidates:
        if (
            c.read_only
            and c.approved
            and not paths_overlap(c.paths, side_effect_paths)
        ):
            concurrent.append(c)
        else:
            serial.append(c)
    return concurrent, serial
