"""Concurrency-safe session context variables.

Uses Python contextvars so each asyncio task gets its own isolated copy
of session state — no cross-contamination when the gateway processes
multiple messages concurrently.
"""

from __future__ import annotations

from contextvars import ContextVar, Token
from typing import Any

_SESSION_PLATFORM: ContextVar[str] = ContextVar("session_platform", default="")
_SESSION_CHAT_ID: ContextVar[str] = ContextVar("session_chat_id", default="")
_SESSION_CHAT_NAME: ContextVar[str] = ContextVar("session_chat_name", default="")
_SESSION_THREAD_ID: ContextVar[str] = ContextVar("session_thread_id", default="")
_SESSION_USER_ID: ContextVar[str] = ContextVar("session_user_id", default="")
_SESSION_USER_NAME: ContextVar[str] = ContextVar("session_user_name", default="")
_SESSION_KEY: ContextVar[str] = ContextVar("session_key", default="")

_ALL_VARS = {
    "platform": _SESSION_PLATFORM,
    "chat_id": _SESSION_CHAT_ID,
    "chat_name": _SESSION_CHAT_NAME,
    "thread_id": _SESSION_THREAD_ID,
    "user_id": _SESSION_USER_ID,
    "user_name": _SESSION_USER_NAME,
    "session_key": _SESSION_KEY,
}


def set_session_vars(**kwargs: str) -> list[Token]:
    tokens: list[Token] = []
    for name, value in kwargs.items():
        var = _ALL_VARS.get(name)
        if var is not None:
            tokens.append(var.set(value))
    return tokens


def clear_session_vars(tokens: list[Token]) -> None:
    for token in tokens:
        try:
            token.var.reset(token)
        except ValueError:
            pass


def get_session_var(name: str, default: str = "") -> str:
    var = _ALL_VARS.get(name)
    if var is None:
        return default
    return var.get(default)


def get_all_session_vars() -> dict[str, str]:
    return {name: var.get("") for name, var in _ALL_VARS.items()}
