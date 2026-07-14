"""Lazy dependency installer for Echo Agent skill backends.

Skills often require Python packages that not every user needs (e.g., openpyxl
for Excel, edge-tts for speech synthesis, faster-whisper for transcription).
Rather than bundling everything as a mandatory install, this module provides
on-demand installation when a skill is first invoked.

Security model:

* **Venv-scoped only.** Installs target ``sys.executable`` in the active venv.
  System Python is never touched.
* **PyPI packages only.** Specs may include version ranges but NOT URLs, file
  paths, or index overrides.
* **Allowlist.** Only specs that appear in :data:`SKILL_DEPS` can be installed.
  Arbitrary package names cannot sneak in.
* **Opt-out.** Set ``ECHO_AGENT_DISABLE_LAZY_INSTALLS=1`` or configure
  ``skills.allow_lazy_installs: false`` to prevent all runtime installs.
* **Safe spec validation.** A regex rejects any spec containing shell
  metacharacters, URLs, or path traversals.

Usage in skill scripts:

    from echo_agent.dependencies import ensure, FeatureUnavailable
    try:
        ensure("skill.excel-author")
    except FeatureUnavailable as e:
        sys.exit(str(e))
    import openpyxl  # now safe
"""

from __future__ import annotations

import asyncio
import logging
import os
import re
import shutil
import subprocess
import sys
import threading
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

logger = logging.getLogger(__name__)

# Single source of truth for how long one install may run. The blocking
# subprocess is capped at this, and install-triggering tools set their
# ``timeout_seconds`` at or above it (plus their own overhead) so the front-end
# waits for the real outcome instead of abandoning a still-running install —
# which used to leave pip mutating site-packages after the tool already
# reported a timeout.
INSTALL_TIMEOUT_SECONDS = 300

# Serialize all installs into the shared venv. Concurrent installs (retries,
# multiple sessions, several skills at once) would otherwise race on the same
# site-packages and corrupt it. The async wrappers acquire this before touching
# pip; the sync entry points stay lock-free for callers that manage their own
# concurrency (CLI, tests).
_install_lock: Optional[asyncio.Lock] = None

# A dedicated single-worker executor keeps a slow/offline install off the
# default asyncio thread pool, so it can never starve other ``to_thread`` work
# (message intake, embeddings) even while it blocks for the full install window.
_install_executor: Optional[ThreadPoolExecutor] = None
_executor_guard = threading.Lock()


def _get_install_lock() -> asyncio.Lock:
    """Lazily create the install lock bound to the running loop.

    Created on first use so the module imports cleanly with no running loop.
    """
    global _install_lock
    if _install_lock is None:
        _install_lock = asyncio.Lock()
    return _install_lock


def _get_install_executor() -> ThreadPoolExecutor:
    global _install_executor
    if _install_executor is None:
        with _executor_guard:
            if _install_executor is None:
                _install_executor = ThreadPoolExecutor(
                    max_workers=1, thread_name_prefix="lazy-install"
                )
    return _install_executor

# =============================================================================
# Allowlist of lazy-installable skill dependencies.
#
# Keys are dot-separated feature names ("skill.<skill-name>" or
# "provider.<name>"). Values are tuples of pip-installable specs.
# Only specs listed here can be installed via this module.
# =============================================================================

SKILL_DEPS: dict[str, tuple[str, ...]] = {
    # ─── Research skills ───────────────────────────────────────────────────
    "skill.web-search": ("duckduckgo_search>=7.0",),
    "skill.web-extract": ("trafilatura>=2.0",),
    "skill.deep-research": ("duckduckgo_search>=7.0", "trafilatura>=2.0"),
    "skill.rss-watcher": ("feedparser>=6.0",),

    # ─── Productivity skills ──────────────────────────────────────────────
    "skill.reminder": ("croniter>=1.4",),
    "skill.calendar": ("caldav>=1.3", "icalendar>=5.0"),
    "skill.notion-sync": (),  # uses stdlib urllib only
    "skill.ocr-document": ("pymupdf>=1.24", "python-docx>=1.1", "Pillow>=10.0", "pytesseract>=0.3"),
    "skill.ocr-document.pdf": ("pymupdf>=1.24",),
    "skill.ocr-document.docx": ("python-docx>=1.1",),
    "skill.ocr-document.image": ("Pillow>=10.0", "pytesseract>=0.3"),

    # ─── Inbound document parsing (read side) ─────────────────────────────
    "media.document": ("pymupdf>=1.24", "python-docx>=1.1", "openpyxl>=3.1", "python-pptx>=1.0"),

    # ─── Utility skills ───────────────────────────────────────────────────
    "skill.file-convert": ("pyyaml>=6.0",),
    "skill.file-convert.md": ("markdown>=3.6",),
    "skill.calculator": (),  # stdlib only
    "skill.text-tools": (),  # stdlib only
    "skill.maps-poi": (),  # stdlib urllib only

    # ─── Creative skills ──────────────────────────────────────────────────
    "skill.image-gen": (),  # uses API via urllib, no extra deps
    "tool.image-gen-fal": ("fal-client>=0.5",),
    "skill.meme-gen": ("Pillow>=10.0",),
    "skill.ppt-author": ("python-pptx>=1.0",),
    "skill.excel-author": ("openpyxl>=3.1",),

    # ─── Development skills ───────────────────────────────────────────────
    "skill.code-runner": (),  # stdlib only
    "skill.workflow-chain": (),  # stdlib only

    # ─── Finance skills ───────────────────────────────────────────────────
    "skill.finance-tracker": (),  # stdlib sqlite3
    "skill.stocks": (),  # stdlib urllib

    # ─── Media skills ─────────────────────────────────────────────────────
    "skill.tts-voice": ("edge-tts>=7.0",),
    "skill.voice-note": ("faster-whisper>=1.0",),

    # ─── Health skills ────────────────────────────────────────────────────
    "skill.fitness-nutrition": (),  # stdlib urllib

    # ─── Learning skills ──────────────────────────────────────────────────
    "skill.flashcards": (),  # stdlib sqlite3

    # ─── DevOps skills ────────────────────────────────────────────────────
    "skill.system-monitor": ("psutil>=5.9",),
    "skill.docker-manage": (),  # uses docker CLI

    # ─── Provider dependencies (not skill-specific) ───────────────────────
    "provider.openai": ("openai>=1.30",),
    "provider.anthropic": ("anthropic>=0.40",),
    "provider.bedrock": ("anthropic>=0.40", "boto3>=1.34"),
    "provider.gemini": ("google-generativeai>=0.8",),
}

# =============================================================================
# Safety validation
# =============================================================================

_SAFE_SPEC = re.compile(
    r"^[A-Za-z0-9_][A-Za-z0-9_.\-]*"
    r"(?:\[[A-Za-z0-9_,\-]+\])?"
    r"(?:[<>=!~]=?[A-Za-z0-9_.\-+,*<>=!~]+)?$"
)


class FeatureUnavailable(RuntimeError):
    """A lazily-installable feature is missing and cannot be made available."""

    def __init__(self, feature: str, missing: tuple[str, ...], reason: str):
        self.feature = feature
        self.missing = missing
        self.reason = reason
        super().__init__(self._format())

    def _format(self) -> str:
        if self.missing:
            spec_list = " ".join(repr(s) for s in self.missing)
            return (
                f"Feature {self.feature!r} unavailable: {self.reason}. "
                f"To install manually: uv pip install {spec_list}  "
                f"(or: pip install {spec_list})."
            )
        return f"Feature {self.feature!r} unavailable: {self.reason}."


@dataclass(frozen=True)
class _InstallResult:
    success: bool
    stdout: str
    stderr: str


# =============================================================================
# Config and policy
# =============================================================================


def _allow_lazy_installs() -> bool:
    """Check whether lazy installs are allowed by configuration.

    Checks (in order):
    1. ECHO_AGENT_DISABLE_LAZY_INSTALLS=1 env var → deny
    2. Config file skills.allow_lazy_installs field → use value
    3. Default → allow
    """
    if os.environ.get("ECHO_AGENT_DISABLE_LAZY_INSTALLS") == "1":
        return False
    try:
        from echo_agent.config.loader import load_config
        cfg = load_config()
        skills_cfg = getattr(cfg, "skills", None)
        if skills_cfg and hasattr(skills_cfg, "allow_lazy_installs"):
            return bool(skills_cfg.allow_lazy_installs)
    except Exception as e:
        logger.debug("Could not read skills.allow_lazy_installs from config, defaulting to allowed: %s", e)
    return True


def _spec_is_safe(spec: str) -> bool:
    """Reject specs containing URLs, paths, or shell metacharacters."""
    if not spec or len(spec) > 200:
        return False
    if any(ch in spec for ch in (";", "|", "&", "`", "$", "\n", "\r", "\t", "\\")):
        return False
    if spec.startswith(("-", "/", ".")) or "://" in spec or "@" in spec:
        return False
    return bool(_SAFE_SPEC.match(spec))


def _pkg_name_from_spec(spec: str) -> str:
    """Extract bare package name: 'openpyxl>=3.1' → 'openpyxl'."""
    m = re.match(r"^([A-Za-z0-9_][A-Za-z0-9_.\-]*)", spec)
    return m.group(1) if m else spec


def _specifier_from_spec(spec: str) -> str:
    """Extract version specifier: 'openpyxl>=3.1' → '>=3.1'."""
    m = re.match(r"^[A-Za-z0-9_][A-Za-z0-9_.\-]*(?:\[[A-Za-z0-9_,\-]+\])?", spec)
    if not m:
        return ""
    return spec[m.end():]


# =============================================================================
# Version checking
# =============================================================================


def _is_satisfied(spec: str) -> bool:
    """Check if spec is installed AND satisfies version constraints."""
    pkg = _pkg_name_from_spec(spec)
    try:
        from importlib.metadata import PackageNotFoundError, version
    except ImportError:
        return False
    try:
        installed = version(pkg)
    except PackageNotFoundError:
        return False
    except Exception:
        return False

    spec_tail = _specifier_from_spec(spec)
    if not spec_tail:
        return True

    try:
        from packaging.specifiers import SpecifierSet
        from packaging.version import Version
    except ImportError:
        return True

    try:
        return Version(installed) in SpecifierSet(spec_tail)
    except Exception:
        return True


def _is_present(spec: str) -> bool:
    """Cheap presence-only check (any version counts)."""
    pkg = _pkg_name_from_spec(spec)
    try:
        from importlib.metadata import PackageNotFoundError, version
    except ImportError:
        return False
    try:
        version(pkg)
        return True
    except PackageNotFoundError:
        return False
    except Exception:
        return False


# =============================================================================
# Installation engine
# =============================================================================


def _venv_pip_install(
    specs: tuple[str, ...], *, timeout: int = INSTALL_TIMEOUT_SECONDS
) -> _InstallResult:
    """Install specs into the active venv using uv → pip → ensurepip ladder."""
    if not specs:
        return _InstallResult(True, "", "")

    venv_root = Path(sys.executable).parent.parent
    uv_env = {**os.environ, "VIRTUAL_ENV": str(venv_root)}

    # Tier 1: uv (preferred — fast, no pip needed in venv)
    uv_bin = shutil.which("uv")
    if uv_bin:
        try:
            r = subprocess.run(
                [uv_bin, "pip", "install", *specs],
                capture_output=True, text=True, timeout=timeout, env=uv_env,
            )
            if r.returncode == 0:
                return _InstallResult(True, r.stdout or "", r.stderr or "")
            logger.debug("uv pip install failed: %s", r.stderr)
        except (subprocess.TimeoutExpired, FileNotFoundError) as e:
            logger.debug("uv invocation failed: %s", e)

    # Tier 2: python -m pip (with ensurepip bootstrap if needed)
    pip_cmd = [sys.executable, "-m", "pip"]
    try:
        probe = subprocess.run(
            pip_cmd + ["--version"],
            capture_output=True, text=True, timeout=15,
        )
        if probe.returncode != 0:
            raise FileNotFoundError("pip not in venv")
    except (subprocess.TimeoutExpired, FileNotFoundError):
        try:
            subprocess.run(
                [sys.executable, "-m", "ensurepip", "--upgrade", "--default-pip"],
                capture_output=True, text=True, timeout=120, check=True,
            )
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as e:
            return _InstallResult(False, "",
                                  f"pip not available and ensurepip failed: {e}")

    try:
        r = subprocess.run(
            pip_cmd + ["install", *specs],
            capture_output=True, text=True, timeout=timeout,
        )
        return _InstallResult(r.returncode == 0, r.stdout or "", r.stderr or "")
    except subprocess.TimeoutExpired as e:
        return _InstallResult(False, "", f"pip install timed out: {e}")
    except Exception as e:
        return _InstallResult(False, "", f"pip install failed: {e}")


# =============================================================================
# Public API
# =============================================================================


def install_authorized(specs: tuple[str, ...], *, source: str) -> dict[str, object]:
    """Install pip specs explicitly authorized by a user/operator out-of-band
    (HTTP endpoint or consent reply).

    Unlike ensure(): does NOT consult the SKILL_DEPS allowlist and does NOT
    honor allow_lazy_installs / ECHO_AGENT_DISABLE_LAZY_INSTALLS — the
    authorization IS the consent. Still enforces _spec_is_safe and venv scope.

    Args:
        specs:  pip specs to install (e.g. ("python-pptx",)).
        source: provenance tag for logging/audit (e.g. "http:skill:ppt-author").

    Returns a JSON-friendly dict with success/installed/skipped/rejected/detail.
    """
    rejected = [s for s in specs if not _spec_is_safe(s)]
    if rejected:
        return {"success": False, "installed": [], "skipped": [],
                "rejected": rejected,
                "detail": f"refusing unsafe spec(s): {', '.join(rejected)}"}

    already: list[str] = []
    to_install_list: list[str] = []
    for s in specs:
        (already if _is_satisfied(s) else to_install_list).append(s)
    to_install = tuple(to_install_list)
    if not to_install:
        return {"success": True, "installed": [], "skipped": list(already),
                "rejected": [], "detail": "all specs already satisfied"}

    logger.info("install_authorized(source=%s): installing %s", source, " ".join(to_install))
    result = _venv_pip_install(to_install)
    if not result.success:
        snippet = (result.stderr or result.stdout or "").strip()[-2000:]
        return {"success": False, "installed": [], "skipped": list(already),
                "rejected": [], "detail": snippet or "install failed (no output)"}

    try:
        import importlib.metadata as _md
        if hasattr(_md, "_cache_clear"):
            _md._cache_clear()  # type: ignore[attr-defined]
    except Exception as e:
        logger.debug("Failed to clear importlib.metadata cache after install: %s", e)

    still_missing = [s for s in to_install if not _is_satisfied(s)]
    if still_missing:
        return {"success": False, "installed": [], "skipped": list(already),
                "rejected": [],
                "detail": f"installed but still not importable (restart may be needed): {still_missing}"}

    logger.info("install_authorized(source=%s): complete", source)
    return {"success": True, "installed": list(to_install), "skipped": list(already),
            "rejected": [], "detail": "ok"}


def feature_specs(feature: str) -> tuple[str, ...]:
    """Return the registered specs for a feature, or raise KeyError."""
    if feature not in SKILL_DEPS:
        raise KeyError(f"Unknown feature: {feature!r}. Available: {list(SKILL_DEPS.keys())}")
    return SKILL_DEPS[feature]


def feature_missing(feature: str) -> tuple[str, ...]:
    """Return the subset of specs for feature that are not currently installed."""
    return tuple(s for s in feature_specs(feature) if not _is_satisfied(s))


def is_available(feature: str) -> bool:
    """Return True if all of the feature's deps are satisfied."""
    if feature not in SKILL_DEPS:
        return False
    specs = SKILL_DEPS[feature]
    if not specs:
        return True
    return not feature_missing(feature)


def feature_install_command(feature: str) -> Optional[str]:
    """Return the manual install command for a feature, or None."""
    if feature not in SKILL_DEPS:
        return None
    specs = SKILL_DEPS[feature]
    if not specs:
        return None
    return "uv pip install " + " ".join(repr(s) for s in specs)


def ensure(feature: str, *, prompt: bool = True) -> None:
    """Ensure all packages for feature are importable.

    If missing, attempts venv-scoped install. Raises FeatureUnavailable if:
    - Feature not in allowlist
    - Lazy installs disabled
    - User declines at prompt
    - Install fails

    Args:
        feature: Key in SKILL_DEPS (e.g., "skill.excel-author")
        prompt: If True and stdin is TTY, ask user before installing.
                Set False for non-interactive contexts (gateway, cron).
    """
    if feature not in SKILL_DEPS:
        raise FeatureUnavailable(
            feature, (), f"feature {feature!r} not in SKILL_DEPS allowlist"
        )

    specs = SKILL_DEPS[feature]
    if not specs:
        return

    missing = feature_missing(feature)
    if not missing:
        return

    for spec in missing:
        if not _spec_is_safe(spec):
            raise FeatureUnavailable(
                feature, missing,
                f"refusing to install unsafe spec {spec!r}"
            )

    if not _allow_lazy_installs():
        raise FeatureUnavailable(
            feature, missing,
            "lazy installs disabled (set ECHO_AGENT_DISABLE_LAZY_INSTALLS=0 "
            "or skills.allow_lazy_installs: true to enable)"
        )

    # Interactive confirmation at TTY
    if prompt and sys.stdin.isatty() and sys.stdout.isatty():
        spec_list = ", ".join(missing)
        try:
            answer = input(
                f"\n[echo-agent] Skill {feature!r} requires: {spec_list}\n"
                f"Install into the active venv now? [Y/n] "
            ).strip().lower()
        except (EOFError, KeyboardInterrupt):
            answer = "n"
        if answer and answer not in {"y", "yes", ""}:
            raise FeatureUnavailable(
                feature, missing, "user declined install"
            )

    logger.info("Installing %s for feature %r", " ".join(missing), feature)
    result = _venv_pip_install(missing)
    if not result.success:
        snippet = (result.stderr or result.stdout or "").strip()[-2000:]
        raise FeatureUnavailable(
            feature, missing,
            f"install failed: {snippet or 'no error output'}"
        )

    # Clear importlib.metadata cache so newly installed packages are visible
    try:
        import importlib.metadata as _md
        if hasattr(_md, "_cache_clear"):
            _md._cache_clear()  # type: ignore[attr-defined]
    except Exception as e:
        logger.debug("Failed to clear importlib.metadata cache after install: %s", e)

    # Verify installation succeeded
    still_missing = feature_missing(feature)
    if still_missing:
        raise FeatureUnavailable(
            feature, still_missing,
            "install reported success but packages still not importable "
            "(may require Python restart)"
        )

    logger.info("Lazy install complete for feature %r", feature)


async def ensure_async(feature: str, *, prompt: bool = False) -> None:
    """Async-safe :func:`ensure`. Runs the blocking pip install in a worker
    thread so a slow/offline install cannot freeze the event loop.

    ``ensure`` calls ``subprocess.run(..., timeout=300)`` synchronously; invoked
    directly from a coroutine on the loop thread it blocks every other task
    (message intake, heartbeats) for up to 300s — long enough for the loop
    watchdog to declare a freeze and kill the process. Offline environments hit
    this reliably: the install spins the full timeout before failing.

    ``prompt`` defaults to False: a worker thread has no controlling TTY, so an
    interactive ``input()`` prompt would never be answerable. Callers on the
    loop are inherently non-interactive.

    Installs are serialized on :func:`_get_install_lock` and run on a dedicated
    single-worker executor (:func:`_get_install_executor`), so concurrent
    callers cannot race on the shared site-packages and a long install cannot
    starve the default ``to_thread`` pool.
    """
    loop = asyncio.get_running_loop()
    async with _get_install_lock():
        await loop.run_in_executor(
            _get_install_executor(), lambda: ensure(feature, prompt=prompt)
        )


async def install_authorized_async(
    specs: tuple[str, ...], *, source: str
) -> dict[str, object]:
    """Async-safe :func:`install_authorized`. Runs the blocking pip install on
    the dedicated install executor under the shared install lock, for the same
    reasons as :func:`ensure_async`."""
    loop = asyncio.get_running_loop()
    async with _get_install_lock():
        return await loop.run_in_executor(
            _get_install_executor(),
            lambda: install_authorized(specs, source=source),
        )


def active_features() -> list[str]:
    """Return features whose packages are currently installed (any version)."""
    active = []
    for feature, specs in SKILL_DEPS.items():
        if not specs:
            continue
        if any(_is_present(s) for s in specs):
            active.append(feature)
    return active


def refresh_active_features(*, prompt: bool = False) -> dict[str, str]:
    """Re-ensure all previously activated features. Returns status map.

    Useful for updating pinned versions after an echo-agent upgrade.
    Returns {feature: status} where status is one of:
        "current" — already satisfied
        "refreshed" — reinstalled to match current specs
        "failed: <reason>" — install failed
        "skipped: <reason>" — user declined or config disabled
    """
    results: dict[str, str] = {}
    for feature in active_features():
        missing = feature_missing(feature)
        if not missing:
            results[feature] = "current"
            continue
        try:
            ensure(feature, prompt=prompt)
            results[feature] = "refreshed"
        except FeatureUnavailable as e:
            if "disabled" in str(e) or "declined" in str(e):
                results[feature] = f"skipped: {e.reason}"
            else:
                results[feature] = f"failed: {e.reason}"
        except Exception as e:
            results[feature] = f"failed: {e}"
    return results


def check_all_features() -> dict[str, dict[str, object]]:
    """Return a status report of all registered features.

    Returns {feature: {"available": bool, "missing": [...], "command": str|None}}
    Useful for CLI status display.
    """
    report: dict[str, dict[str, object]] = {}
    for feature, specs in SKILL_DEPS.items():
        if not specs:
            report[feature] = {"available": True, "missing": [], "command": None}
            continue
        missing = tuple(s for s in specs if not _is_satisfied(s))
        report[feature] = {
            "available": not missing,
            "missing": list(missing),
            "command": feature_install_command(feature) if missing else None,
        }
    return report
