"""Plugin manifest — Pydantic model and YAML parsing."""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml
from pydantic import BaseModel, Field


class PluginProvides(BaseModel):
    """Declares what a plugin provides."""

    tools: list[str] = Field(default_factory=list)
    hooks: list[str] = Field(default_factory=list)
    commands: list[str] = Field(default_factory=list)


class PluginManifest(BaseModel):
    """Parsed representation of a plugin.yaml manifest."""

    name: str
    version: str = "0.0.1"
    description: str = ""
    author: str = ""
    license: str = ""
    requires_echo_agent: str = ""
    requires_env: list[str] = Field(default_factory=list)
    provides: PluginProvides = Field(default_factory=PluginProvides)
    kind: str = "integration"
    config_key: str = ""
    depends_on: list[str] = Field(default_factory=list)
    permissions: list[str] = Field(default_factory=list)


@dataclass
class PluginRecord:
    """Runtime state for a discovered plugin."""

    manifest: PluginManifest
    source: str  # "entrypoint", "user", "project"
    path: Path | None = None
    module: Any = None
    status: str = "discovered"  # discovered, loaded, activated, failed, disabled
    error: str = ""
    tools_registered: list[str] = field(default_factory=list)
    hooks_registered: list[str] = field(default_factory=list)


def parse_manifest(path: Path) -> PluginManifest:
    """Parse a plugin.yaml file into a PluginManifest."""
    content = path.read_text(encoding="utf-8")
    data = yaml.safe_load(content) or {}
    return PluginManifest(**data)


def parse_manifest_from_dict(data: dict[str, Any]) -> PluginManifest:
    """Parse a manifest from a dictionary (for entry_point plugins)."""
    return PluginManifest(**data)


def check_required_env(manifest: PluginManifest) -> list[str]:
    """Check which required environment variables are missing."""
    missing = []
    for var in manifest.requires_env:
        if not os.environ.get(var):
            missing.append(var)
    return missing
