"""Configurable brand strings for the TUI (name, prompt sigil, welcome, goodbye).

Everything user-facing that identifies
the product lives here so a white-label / multi-tenant deployment can rebrand
without touching widget code. Values come from env vars (ECHO_BRAND_*), falling
back to Echo's defaults.
"""

from __future__ import annotations

import os
from collections.abc import Mapping
from dataclasses import dataclass

_MAX_LEN = 80

# Kept alongside the brand strings so the setup wizard can mirror the TUI
# wordmark without importing Textual. Setup maps these roles to ANSI colors.
ECHO_LOGO_ART = (
    "███████╗ ██████╗██╗  ██╗ ██████╗ ",
    "██╔════╝██╔════╝██║  ██║██╔═══██╗",
    "█████╗  ██║     ███████║██║   ██║",
    "██╔══╝  ██║     ██╔══██║██║   ██║",
    "███████╗╚██████╗██║  ██║╚██████╔╝",
)
ECHO_LOGO_GRADIENT = ("primary", "primary", "accent", "secondary", "secondary")


@dataclass(frozen=True)
class Brand:
    name: str = "echo"
    tagline: str = "agent"
    prompt: str = "❯"
    placeholder: str = "输入消息…"
    welcome: str = "输入消息开始对话  ·  /help 查看命令  ·  Ctrl+C 停止任务/退出"
    goodbye: str = "再见 👋"


def _clean(value: str | None, fallback: str) -> str:
    cleaned = " ".join(str(value or "").split()).strip()
    if not cleaned or len(cleaned) > _MAX_LEN:
        return fallback
    return cleaned


def load_brand(env: Mapping[str, str] | None = None) -> Brand:
    """Build a Brand from ECHO_BRAND_* env vars, falling back to defaults."""
    e = env if env is not None else os.environ
    d = Brand()
    return Brand(
        name=_clean(e.get("ECHO_BRAND_NAME"), d.name),
        tagline=_clean(e.get("ECHO_BRAND_TAGLINE"), d.tagline),
        prompt=_clean(e.get("ECHO_BRAND_PROMPT"), d.prompt),
        placeholder=_clean(e.get("ECHO_BRAND_PLACEHOLDER"), d.placeholder),
        welcome=_clean(e.get("ECHO_BRAND_WELCOME"), d.welcome),
        goodbye=_clean(e.get("ECHO_BRAND_GOODBYE"), d.goodbye),
    )
