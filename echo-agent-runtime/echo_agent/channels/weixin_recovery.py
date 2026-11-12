"""Persistent, bounded recovery for explicit iLink send refusals.

An undocumented -2 is not proof of expired credentials. Keep the credentials,
allow at most four rejected attempts per account/peer in 24 hours, and retain a
future recovery time even when that budget is exhausted. A restart must neither
erase the cooldown nor leave the conversation blocked forever.
"""
from __future__ import annotations

import hashlib
import json
import time
from pathlib import Path

from loguru import logger

from echo_agent.channels.base import DeliveryRecoverySummary


class WeixinDeliveryRecovery:
    WINDOW_MS = 24 * 60 * 60 * 1000
    DELAYS_MS = (15 * 60 * 1000, 60 * 60 * 1000, 3 * 60 * 60 * 1000)

    def __init__(self, directory: Path, account: str, token: str, write, *, clock=None):
        self._path = directory / f"{account}.delivery-state.json"
        self._credential = hashlib.sha256(token.encode()).hexdigest()
        self._write = write
        self._clock = clock or time.time
        self.chats: dict[str, dict] = {}
        self.login_required = False
        try:
            if self._path.exists():
                data = json.loads(self._path.read_text())
                if data.get("credential") == self._credential:
                    chats = data.get("chats", {})
                    if not isinstance(chats, dict):
                        raise ValueError("invalid delivery state")
                    for chat, state in chats.items():
                        if (isinstance(chat, str) and isinstance(state, dict)
                                and all(type(state.get(k)) is int and state[k] >= 0
                                        for k in ("attempts", "window_started_ms", "retry_at_ms"))):
                            self.chats[chat] = state
                    self.login_required = data.get("login_required") is True
        except (OSError, ValueError, TypeError, AttributeError):
            logger.warning("weixin: could not restore delivery cooldown state")

    def now_ms(self) -> int:
        return int(self._clock() * 1000)

    def save(self) -> None:
        try:
            self._write(self._path, {"version": 1, "credential": self._credential,
                                   "chats": self.chats, "login_required": self.login_required})
        except OSError:
            logger.exception("weixin: could not persist delivery cooldown state")

    def reject(self, chat: str) -> dict:
        now = self.now_ms()
        previous = self.chats.get(chat, {})
        started = previous.get("window_started_ms", now)
        attempts = previous.get("attempts", 0)
        if now >= started + self.WINDOW_MS:
            started, attempts = now, 0
        attempts += 1
        retry_at = (now + self.DELAYS_MS[attempts - 1] if attempts <= len(self.DELAYS_MS)
                    else max(now + self.DELAYS_MS[-1], started + self.WINDOW_MS))
        state = {"attempts": attempts, "window_started_ms": started, "retry_at_ms": retry_at}
        self.chats[chat] = state
        self.save()
        return state

    def accepted(self, chat: str) -> None:
        if self.chats.pop(chat, None) is not None:
            self.save()

    def fresh_context(self, chat: str) -> None:
        self.chats.pop(chat, None)
        self.login_required = False
        self.save()

    def summary(self) -> DeliveryRecoverySummary:
        return {"pending_conversations": len(self.chats),
                "retry_at_ms": min((s["retry_at_ms"] for s in self.chats.values()), default=0),
                "retry_attempts": max((s["attempts"] for s in self.chats.values()), default=0)}
