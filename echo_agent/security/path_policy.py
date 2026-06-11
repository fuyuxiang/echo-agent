"""Path security policy — denylist-based access control for filesystem operations.

Default-allow with explicit denylists for sensitive paths. Replaces the old
workspace-only whitelist approach to let the agent operate on user directories
(Desktop, Documents, etc.) while still protecting credentials and system files.
"""

from __future__ import annotations

import os
import tempfile
from pathlib import Path
from typing import Optional


def _home() -> str:
    return os.path.realpath(os.path.expanduser("~"))


def _echo_home() -> Path:
    return Path.home() / ".echo-agent"


# ---------------------------------------------------------------------------
# Device files — reading these hangs the process
# ---------------------------------------------------------------------------
BLOCKED_DEVICE_PATHS: frozenset[str] = frozenset({
    "/dev/zero", "/dev/random", "/dev/urandom", "/dev/full",
    "/dev/stdin", "/dev/tty", "/dev/console",
    "/dev/stdout", "/dev/stderr",
    "/dev/fd/0", "/dev/fd/1", "/dev/fd/2",
})

# ---------------------------------------------------------------------------
# Write denylist — exact paths
# ---------------------------------------------------------------------------

def _build_write_denied_paths() -> set[str]:
    home = _home()
    echo_home = str(_echo_home().resolve())
    return {
        os.path.realpath(p)
        for p in [
            os.path.join(home, ".ssh", "authorized_keys"),
            os.path.join(home, ".ssh", "id_rsa"),
            os.path.join(home, ".ssh", "id_ed25519"),
            os.path.join(home, ".ssh", "id_ecdsa"),
            os.path.join(home, ".ssh", "config"),
            os.path.join(echo_home, ".env"),
            os.path.join(home, ".bashrc"),
            os.path.join(home, ".zshrc"),
            os.path.join(home, ".profile"),
            os.path.join(home, ".bash_profile"),
            os.path.join(home, ".zprofile"),
            os.path.join(home, ".netrc"),
            os.path.join(home, ".pgpass"),
            os.path.join(home, ".npmrc"),
            os.path.join(home, ".pypirc"),
            "/etc/sudoers",
            "/etc/passwd",
            "/etc/shadow",
            "/etc/gshadow",
            "/private/etc/sudoers",
            "/private/etc/passwd",
            "/private/etc/shadow",
        ]
    }


# ---------------------------------------------------------------------------
# Write denylist — directory prefixes (anything under these is denied)
# ---------------------------------------------------------------------------

def _build_write_denied_prefixes() -> list[str]:
    home = _home()
    return [
        os.path.realpath(p) + os.sep
        for p in [
            os.path.join(home, ".ssh"),
            os.path.join(home, ".aws"),
            os.path.join(home, ".gnupg"),
            os.path.join(home, ".kube"),
            os.path.join(home, ".docker"),
            os.path.join(home, ".azure"),
            os.path.join(home, ".config", "gh"),
            "/etc/sudoers.d",
            "/etc/systemd",
        ]
    ]


# ---------------------------------------------------------------------------
# System path prefixes — writes here are blocked (use shell with sudo instead)
# ---------------------------------------------------------------------------
SYSTEM_PATH_PREFIXES: tuple[str, ...] = (
    "/etc/", "/boot/", "/usr/lib/systemd/",
    "/private/etc/", "/private/var/",
)

# ---------------------------------------------------------------------------
# Read denylist — credential files (prevent secret exfiltration)
# ---------------------------------------------------------------------------

def _build_read_denied_credential_paths() -> set[str]:
    home = _home()
    return {
        os.path.realpath(p)
        for p in [
            os.path.join(home, ".ssh", "id_rsa"),
            os.path.join(home, ".ssh", "id_ed25519"),
            os.path.join(home, ".ssh", "id_ecdsa"),
            os.path.join(home, ".netrc"),
            os.path.join(home, ".pgpass"),
            os.path.join(home, ".aws", "credentials"),
            os.path.join(home, ".docker", "config.json"),
            os.path.join(home, ".kube", "config"),
            os.path.join(home, ".azure", "credentials"),
            "/etc/shadow",
            "/etc/gshadow",
            "/private/etc/shadow",
        ]
    }


def _build_read_denied_patterns() -> list[str]:
    """Filename patterns for credential-like files."""
    return [".env", "credentials.json", "service-account.json", "secrets.yaml", "secrets.yml"]


def _build_read_denied_prefixes() -> list[str]:
    """Directories whose entire contents are read-denied — they hold private
    keys under arbitrary names, so a file-level denylist cannot cover them."""
    home = _home()
    return [
        os.path.realpath(p) + os.sep
        for p in [
            os.path.join(home, ".ssh"),
            os.path.join(home, ".gnupg"),
            os.path.join(home, ".aws"),
            os.path.join(home, ".kube"),
            os.path.join(home, ".docker"),
            os.path.join(home, ".azure"),
            os.path.join(home, ".config", "gh"),
        ]
    ]


# ---------------------------------------------------------------------------
# Read denylist — internal cache dirs (prevent prompt injection)
# ---------------------------------------------------------------------------

def _build_read_blocked_dirs() -> list[Path]:
    echo_home = _echo_home().resolve()
    return [
        echo_home / "skills" / ".cache",
        echo_home / "data" / ".internal",
    ]


# ---------------------------------------------------------------------------
# Temp path detection — macOS resolves /tmp to /private/var/folders/...
# ---------------------------------------------------------------------------
_TEMP_PREFIXES: tuple[str, ...] | None = None


def _is_temp_path(resolved: str) -> bool:
    global _TEMP_PREFIXES
    if _TEMP_PREFIXES is None:
        candidates = {"/tmp", "/private/tmp", "/var/tmp", "/private/var/tmp"}
        try:
            real_tmp = os.path.realpath(tempfile.gettempdir())
            candidates.add(real_tmp)
            if real_tmp.startswith("/private/var/folders"):
                candidates.add("/private/var/folders")
        except Exception:
            pass
        _TEMP_PREFIXES = tuple(p + os.sep for p in candidates) + tuple(candidates)
    return any(resolved.startswith(p) for p in _TEMP_PREFIXES)


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def resolve_path(path: str, workspace: str) -> Path:
    """Resolve a path: absolute paths stay as-is, relative paths resolve against workspace."""
    raw = Path(path).expanduser()
    return raw.resolve() if raw.is_absolute() else (Path(workspace) / raw).resolve()


def check_read(path: str, workspace: str) -> Optional[str]:
    """Return an error message if reading this path should be denied, else None."""
    normalized = os.path.expanduser(path)
    resolved = resolve_path(path, workspace)
    resolved_str = str(resolved)
    # realpath of the *resolved* path so a symlink pointing at a device file
    # or credential cannot dodge the checks below.
    real = os.path.realpath(resolved_str)

    # Block device files (check both the literal path and the symlink target)
    if normalized in BLOCKED_DEVICE_PATHS or real in BLOCKED_DEVICE_PATHS:
        return f"Cannot read '{path}': device file that would block or produce infinite output"

    # Block /proc stdio aliases
    for candidate in (normalized, real):
        if candidate.startswith("/proc/") and candidate.endswith(("/fd/0", "/fd/1", "/fd/2")):
            return f"Cannot read '{path}': stdio device alias"

    # Block credential files
    denied_credentials = _build_read_denied_credential_paths()
    if resolved_str in denied_credentials or real in denied_credentials:
        return f"Read denied: {path} is a credential file"

    # Block credential directories wholesale (private keys live under
    # arbitrary names — a file denylist can't enumerate them)
    for prefix in _build_read_denied_prefixes():
        if resolved_str.startswith(prefix) or real.startswith(prefix):
            return f"Read denied: {path} is inside a protected credential directory"

    # Block credential-like filenames
    filename = resolved.name
    for pattern in _build_read_denied_patterns():
        if filename == pattern or filename.endswith(pattern):
            return f"Read denied: {path} appears to be a credential file ({pattern})"

    # Block internal cache directories
    for blocked_dir in _build_read_blocked_dirs():
        try:
            resolved.relative_to(blocked_dir)
            return (
                f"Access denied: {path} is an internal cache file. "
                "Use the appropriate tool instead of reading directly."
            )
        except ValueError:
            continue

    return None


def check_write(path: str, workspace: str, safe_write_root: str = "") -> Optional[str]:
    """Return an error message if writing this path should be denied, else None."""
    resolved = str(resolve_path(path, workspace))
    normalized = os.path.normpath(os.path.expanduser(path))

    # Exact path denylist
    denied_paths = _build_write_denied_paths()
    if resolved in denied_paths or normalized in denied_paths:
        return f"Write denied: {path} is a protected credential/config file"

    # Directory prefix denylist
    for prefix in _build_write_denied_prefixes():
        if resolved.startswith(prefix) or normalized.startswith(prefix):
            return f"Write denied: {path} is inside a protected directory"

    # System path prefixes
    for prefix in SYSTEM_PATH_PREFIXES:
        if resolved.startswith(prefix) or normalized.startswith(prefix):
            # Allow temp directories (macOS resolves /tmp → /private/var/folders/...)
            if _is_temp_path(resolved):
                break
            return (
                f"Write denied: {path} is a system path. "
                "Use the shell tool with appropriate privileges if needed."
            )

    # Optional safe_write_root enforcement
    if safe_write_root:
        root = os.path.realpath(os.path.expanduser(safe_write_root))
        if not (resolved == root or resolved.startswith(root + os.sep)):
            return f"Write denied: {path} is outside the configured safe_write_root ({safe_write_root})"

    return None


def check_cwd(path: str) -> Optional[str]:
    """Return an error message if using this path as cwd should be denied, else None."""
    resolved = os.path.realpath(os.path.expanduser(path))
    blocked_prefixes = ("/proc/", "/sys/")
    for prefix in blocked_prefixes:
        if resolved.startswith(prefix):
            return f"Cannot use {path} as working directory: system pseudo-filesystem"
    return None
