"""Environment construction for skill script subprocesses.

``skill_run`` used to launch scripts with ``env={}``. The intent was sound —
don't hand a skill the agent's whole environment — but an *empty* env is not a
smaller environment, it is a broken one:

* No ``PATH``, so ``shutil.which("docker")`` returns None. Every skill
  declaring ``requires.bins`` was unrunnable, making that field decoration.
* No ``HOME``, so anything touching ``~/.config`` or a cache dir failed in
  ways whose traceback pointed nowhere near the real cause.
* No credentials, so ``image-gen`` (reads ``OPENAI_API_KEY``) exited on start.
  The documented workaround was to pass secrets through ``args`` — which puts
  them in the audit log and in ``ps`` output. The safe-looking default was
  actively pushing users toward the unsafe pattern.

So: an allowlist, not a blank slate and not ``os.environ``. Infrastructure keys
below are passed through unconditionally; credentials only when the skill names
them in ``metadata.echo.requires.env``, so reading a SKILL.md tells you exactly
which secrets it can see.
"""

from __future__ import annotations

import os

from loguru import logger

from echo_agent.agent.executors.base import prepend_interpreter_bin
from echo_agent.skills.store import parse_frontmatter

# Keys every subprocess needs to behave like a normal program: locate binaries,
# find a home/temp dir, decode text, verify TLS, honor the operator's proxy.
_INFRA_KEYS: tuple[str, ...] = (
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "TEMP",
    "TMP",
    "TZ",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    # TLS trust stores — without these, requests/httpx fail to verify certs on
    # installs that rely on certifi or a corporate bundle.
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    # Proxies: an operator behind an egress proxy has no other way to tell a
    # skill script about it, and silently bypassing it looks like a hang.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    # Keep the child's Python behavior aligned with the parent's.
    "PYTHONHASHSEED",
    "PYTHONIOENCODING",
    "PYTHONUTF8",
    "VIRTUAL_ENV",
    # Propagate the lazy-install kill switch: a script that shells out to the
    # agent's own machinery must see the same policy the parent is under.
    "ECHO_AGENT_DISABLE_LAZY_INSTALLS",
)

# A skill may not request these by name through requires.env, however it asks.
# Passing them would let a skill rewrite what the interpreter imports (and thus
# execute code of its choosing on the next import) or re-point the agent's own
# configuration. PATH/HOME are set by us above, not requestable.
_NEVER_FORWARD: frozenset[str] = frozenset({
    "PYTHONPATH",
    "PYTHONHOME",
    "PYTHONSTARTUP",
    "PYTHONEXECUTABLE",
    "PYTHONWARNINGS",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "DYLD_FRAMEWORK_PATH",
    "BASH_ENV",
    "ENV",
    "IFS",
    "PATH",
    "HOME",
})

_MAX_DECLARED_ENV_KEYS = 32
_ENV_KEY_MAX_LEN = 128


def declared_env_keys(skill_md: str) -> list[str]:
    """Credential/config keys a skill declares via metadata.echo.requires.env.

    Accepts a list of names. Values are never read from the manifest — only the
    *names*, which are then looked up in the agent's own environment. A skill
    therefore cannot inject a value, only ask to see one the operator already
    set.
    """
    try:
        fm, _ = parse_frontmatter(skill_md)
    except Exception:
        return []
    if not isinstance(fm, dict):
        return []
    meta = fm.get("metadata") or {}
    if not isinstance(meta, dict):
        return []
    echo_meta = meta.get("echo") or {}
    if not isinstance(echo_meta, dict):
        return []
    requires = echo_meta.get("requires") or {}
    if not isinstance(requires, dict):
        return []
    raw = requires.get("env") or []
    if isinstance(raw, str):
        raw = [raw]
    if not isinstance(raw, list):
        return []

    keys: list[str] = []
    for item in raw[:_MAX_DECLARED_ENV_KEYS]:
        if not isinstance(item, str):
            continue
        key = item.strip()
        if not key or len(key) > _ENV_KEY_MAX_LEN:
            continue
        # Env names only; anything else is a manifest error, not a request.
        if not all(c.isalnum() or c == "_" for c in key):
            continue
        if key.upper() in _NEVER_FORWARD or key in _NEVER_FORWARD:
            logger.warning("Skill requested non-forwardable env key '{}'; ignoring", key)
            continue
        keys.append(key)
    return keys


def build_skill_env(skill_md: str = "", *, base: dict[str, str] | None = None) -> dict[str, str]:
    """Build the environment for a skill script subprocess.

    Infrastructure keys from ``_INFRA_KEYS`` plus any credential keys the skill
    declared. PATH always leads with ``sys.executable``'s directory so a script
    that shells out to ``python3`` gets the agent's interpreter and therefore
    the venv its dependencies live in.
    """
    source = os.environ if base is None else base
    env: dict[str, str] = {}
    for key in _INFRA_KEYS:
        value = source.get(key)
        if value is not None:
            env[key] = value

    for key in declared_env_keys(skill_md):
        value = source.get(key)
        if value is None:
            # Not an error here: the script reports what it needs far better
            # than we can, and skill_view surfaces missing credentials up front.
            logger.debug("Skill declared env key '{}' but it is unset", key)
            continue
        env[key] = value

    env.setdefault("PATH", os.defpath)
    return prepend_interpreter_bin(env)
