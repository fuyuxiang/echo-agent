from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path

from loguru import logger

_UNIT_SEC = {"s": 1, "m": 60, "h": 3600, "d": 86400}


def parse_interval(s: str) -> int:
    s = (s or "").strip()
    m = re.fullmatch(r"(\d+)([smhd]?)", s)
    if not m:
        return 0
    value = int(m.group(1))
    unit = m.group(2) or "s"
    return value * _UNIT_SEC[unit]


@dataclass
class InspectItem:
    name: str
    interval_sec: int
    check: str


class InspectStore:
    def __init__(self, inspect_path: Path, state_path: Path) -> None:
        self._inspect_path = inspect_path
        self._state_path = state_path

    def load_items(self) -> list[InspectItem]:
        try:
            text = self._inspect_path.read_text(encoding="utf-8")
        except Exception:
            return []
        items: list[InspectItem] = []
        # split on level-2 headings
        blocks = re.split(r"^##\s+", text, flags=re.MULTILINE)
        for block in blocks[1:]:  # blocks[0] is preamble before first ##
            lines = block.splitlines()
            name = lines[0].strip() if lines else ""
            if not name:
                continue
            interval_sec = 0
            check = ""
            for line in lines[1:]:
                stripped = line.strip()
                low = stripped.lower()
                if low.startswith("- interval:"):
                    interval_sec = parse_interval(stripped.split(":", 1)[1])
                elif low.startswith("- check:"):
                    check = stripped.split(":", 1)[1].strip()
            if interval_sec > 0 and check:
                items.append(InspectItem(name=name, interval_sec=interval_sec, check=check))
        return items

    def load_state(self) -> dict[str, dict]:
        try:
            return json.loads(self._state_path.read_text(encoding="utf-8"))
        except Exception:
            return {}

    def save_state(self, state: dict) -> None:
        try:
            self._state_path.parent.mkdir(parents=True, exist_ok=True)
            self._state_path.write_text(
                json.dumps(state, ensure_ascii=False, indent=2), encoding="utf-8"
            )
        except Exception as e:
            logger.debug("inspect state save failed (ignored): {}", e)

    def due_items(self, items, state, now_sec, max_items):
        due = []
        for item in items:
            last = state.get(item.name, {}).get("last_checked_at")
            if last is None or (now_sec - last) >= item.interval_sec:
                due.append(item)
            if len(due) >= max_items:
                break
        return due
