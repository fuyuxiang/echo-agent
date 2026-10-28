"""Resolve or generate a valid Fernet key for credential encryption.

Single responsibility: hand back a usable Fernet key. Resolution order:
  1. ``env_name`` env var (validated as a real Fernet key)
  2. ``key_path`` file (validated)
  3. otherwise generate one and persist it with 0600 perms

This replaces the previous weak ``sha256(secret)`` KDF: we store a proper
Fernet key directly, so there is no passphrase and no derivation step.

IO failures (unwritable dir, read-only filesystem, etc.) are not wrapped: the
underlying ``OSError`` propagates directly, leaving fallback to the caller.
"""

from __future__ import annotations

import os
from pathlib import Path

from cryptography.fernet import Fernet
from loguru import logger

KEY_FILENAME = ".credential_key"


def _validate(raw: bytes, *, source: str) -> bytes:
    try:
        Fernet(raw)
    except (ValueError, TypeError) as exc:
        raise ValueError(
            f"Invalid Fernet key from {source}; expected a 44-byte urlsafe-base64 "
            f"key (generate one with Fernet.generate_key())"
        ) from exc
    return raw


def resolve_or_create_key(
    key_path: Path,
    env_name: str = "ECHO_AGENT_CREDENTIAL_KEY",
) -> bytes:
    env_value = os.environ.get(env_name, "")
    if env_value:
        return _validate(env_value.encode(), source=env_name)

    key_file = Path(key_path)
    if key_file.exists():
        return _validate(key_file.read_bytes(), source=str(key_file))

    key_file.parent.mkdir(parents=True, exist_ok=True)
    # 从创建起即 0600，避免 write→chmod 之间的明文可读窗口；
    # O_EXCL 消除两个进程同时生成时互相覆盖的竞态。
    try:
        fd = os.open(key_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        # 竞态下另一进程刚生成，读它的（已校验）
        return _validate(key_file.read_bytes(), source=str(key_file))
    try:
        key = Fernet.generate_key()
        os.write(fd, key)
    finally:
        os.close(fd)
    logger.info("Generated credential encryption key at {}", key_file)
    return key
