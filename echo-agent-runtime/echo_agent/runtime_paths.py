"""Runtime path helpers for global installs and editable checkouts."""

from __future__ import annotations

from pathlib import Path


def echo_home() -> Path:
    return Path.home() / ".echo-agent"


def default_config_path() -> Path:
    return echo_home() / "echo-agent.yaml"


def bundled_skills_dir() -> Path | None:
    package_root = Path(__file__).resolve().parent
    candidates = [
        package_root / "_bundled" / "skills",
        package_root.parent / "skills",
    ]
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return None


def normalize_cache_dir(cache_dir: str) -> str:
    """Pin a configured cache to one absolute path; empty keeps library defaults.

    Relative paths are anchored at initialization, even if cwd changes later.
    Home expansion follows the current process/service user's home directory.
    """
    return str(Path(cache_dir).expanduser().resolve()) if cache_dir else ""
