"""Echo Agent — a modular AI agent framework."""

from __future__ import annotations

from importlib.metadata import PackageNotFoundError, version
from pathlib import Path
import tomllib


def _source_tree_version(pyproject: Path | None = None) -> str:
    """Read the authoritative project version when executing a checkout.

    Editable environments can retain stale ``dist-info`` after pyproject is
    bumped—the exact 0.3.6-vs-0.3.8 audit failure.  A wheel has no adjacent
    pyproject and therefore continues to use its installed metadata below.
    """
    path = pyproject or Path(__file__).resolve().parent.parent / "pyproject.toml"
    if not path.is_file():
        return ""
    try:
        project = tomllib.loads(path.read_text(encoding="utf-8")).get("project", {})
    except (OSError, tomllib.TOMLDecodeError):
        return ""
    if project.get("name") != "echo-agent":
        return ""
    value = project.get("version")
    return value.strip() if isinstance(value, str) else ""


def _resolve_version() -> str:
    source_version = _source_tree_version()
    if source_version:
        return source_version
    try:
        return version("echo-agent")
    except PackageNotFoundError:  # unpacked/frozen source without metadata
        return "0.0.0+unknown"


__version__ = _resolve_version()
