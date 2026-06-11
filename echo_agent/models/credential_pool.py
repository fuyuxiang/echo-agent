"""Credential pool — round-robin API key rotation with error tracking."""

from __future__ import annotations

import threading
import time
from dataclasses import dataclass


_DEFAULT_COOLDOWN_SECONDS = 300.0


@dataclass
class _KeyState:
    key: str
    error_count: int = 0
    exhausted: bool = False
    cooldown_until: float = 0.0


class CredentialPool:

    def __init__(self, keys: list[str], cooldown_seconds: float = _DEFAULT_COOLDOWN_SECONDS):
        if not keys:
            raise ValueError("CredentialPool requires at least one key")
        self._keys = [_KeyState(key=k) for k in keys]
        self._index = 0
        self._lock = threading.Lock()
        self._cooldown_seconds = max(0.0, float(cooldown_seconds))

    @property
    def size(self) -> int:
        return len(self._keys)

    def __repr__(self) -> str:
        return f"CredentialPool(size={self.size})"

    def get_next(self) -> str:
        with self._lock:
            now = time.monotonic()
            attempts = 0
            while attempts < len(self._keys):
                state = self._keys[self._index]
                self._index = (self._index + 1) % len(self._keys)
                if state.exhausted and state.cooldown_until and now >= state.cooldown_until:
                    # Cooldown elapsed: give the key another chance instead of
                    # leaving it permanently blacklisted.
                    state.exhausted = False
                    state.error_count = 0
                    state.cooldown_until = 0.0
                if not state.exhausted:
                    return state.key
                attempts += 1
            # All keys exhausted: reset and rotate from current index so traffic
            # spreads across the pool instead of always hitting key #0.
            self._reset_all()
            state = self._keys[self._index]
            self._index = (self._index + 1) % len(self._keys)
            return state.key

    def report_error(self, key: str) -> None:
        with self._lock:
            for state in self._keys:
                if state.key == key:
                    state.error_count += 1
                    if state.error_count >= 3:
                        state.exhausted = True
                        if self._cooldown_seconds > 0:
                            state.cooldown_until = time.monotonic() + self._cooldown_seconds
                    break

    def report_success(self, key: str) -> None:
        with self._lock:
            for state in self._keys:
                if state.key == key:
                    state.error_count = 0
                    state.exhausted = False
                    state.cooldown_until = 0.0
                    break

    def _reset_all(self) -> None:
        for state in self._keys:
            state.exhausted = False
            state.error_count = 0
            state.cooldown_until = 0.0
