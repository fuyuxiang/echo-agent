"""Credential pool — round-robin API key rotation with error tracking."""

from __future__ import annotations

import threading
from dataclasses import dataclass, field
from typing import Any


@dataclass
class _KeyState:
    key: str
    error_count: int = 0
    exhausted: bool = False


class CredentialPool:

    def __init__(self, keys: list[str]):
        if not keys:
            raise ValueError("CredentialPool requires at least one key")
        self._keys = [_KeyState(key=k) for k in keys]
        self._index = 0
        self._lock = threading.Lock()

    @property
    def size(self) -> int:
        return len(self._keys)

    def get_next(self) -> str:
        with self._lock:
            attempts = 0
            while attempts < len(self._keys):
                state = self._keys[self._index]
                self._index = (self._index + 1) % len(self._keys)
                if not state.exhausted:
                    return state.key
                attempts += 1
            self._reset_all()
            return self._keys[0].key

    def report_error(self, key: str) -> None:
        with self._lock:
            for state in self._keys:
                if state.key == key:
                    state.error_count += 1
                    if state.error_count >= 3:
                        state.exhausted = True
                    break

    def report_success(self, key: str) -> None:
        with self._lock:
            for state in self._keys:
                if state.key == key:
                    state.error_count = 0
                    state.exhausted = False
                    break

    def _reset_all(self) -> None:
        for state in self._keys:
            state.exhausted = False
            state.error_count = 0
