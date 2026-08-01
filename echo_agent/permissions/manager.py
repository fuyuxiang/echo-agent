"""Approval and credential management.

Covers:
  - Pre-execution approval workflow (approve/deny/whitelist/blacklist)
  - Credential management (agent identity, token storage, isolation, rotation, audit)
"""

from __future__ import annotations

import hashlib
import asyncio
import json
import os
import tempfile
import uuid
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from pathlib import Path
from typing import Any

from loguru import logger


class ApprovalStatus(str, Enum):
    PENDING = "pending"
    APPROVED = "approved"
    DENIED = "denied"
    EXPIRED = "expired"


@dataclass
class ApprovalRequest:
    id: str = field(default_factory=lambda: uuid.uuid4().hex[:10])
    action: str = ""
    tool_name: str = ""
    params: dict[str, Any] = field(default_factory=dict)
    user_id: str = ""
    status: ApprovalStatus = ApprovalStatus.PENDING
    reason: str = ""
    decided_by: str = ""
    created_at: str = field(default_factory=lambda: datetime.now().isoformat())
    decided_at: str = ""
    # Which conversation this decision belongs to. Needed to answer "whose
    # approval is this?" when a channel disconnects: without it the only way to
    # release an approval nobody can answer anymore was to wait out
    # wait_timeout_seconds (300s by default) with the turn parked.
    session_key: str = ""


class ApprovalManager:
    """Manages pre-execution approval for high-risk actions."""

    def __init__(
        self,
        require_approval: list[str] | None = None,
        auto_approve: list[str] | None = None,
        auto_deny: list[str] | None = None,
        default_policy: str = "approve",
        store_path: Path | None = None,
    ):
        self._require = set(require_approval or [])
        self._auto_approve = set(auto_approve or [])
        self._auto_deny = set(auto_deny or [])
        self._default_policy = default_policy
        self._pending: dict[str, ApprovalRequest] = {}
        self._pending_by_signature: dict[str, str] = {}
        self._history: list[ApprovalRequest] = []
        self._approved_signatures: set[str] = set()
        self._waiters: dict[str, asyncio.Event] = {}
        self._store_path = store_path
        self._load_state()

    def needs_approval(self, action: str) -> bool:
        if action in self._auto_approve:
            return False
        if action in self._auto_deny:
            return True
        if action in self._require:
            return True
        return self._default_policy == "ask"

    def request_approval(
        self, action: str, tool_name: str = "", params: dict[str, Any] | None = None,
        user_id: str = "", session_key: str = "", require: bool = False,
    ) -> ApprovalRequest:
        """Open (or reuse) an approval request for ``action``.

        ``require=True`` means the CALLER has already established that this call
        needs human approval — e.g. ApprovalGate classified it EXEC/DANGEROUS —
        so a genuine PENDING request is created even when ``action`` is absent
        from ``require_approval``. Without it the manager fell through to
        ``default_policy="approve"`` and returned an already-APPROVED request
        that was never registered as pending, which made the ``require_approval``
        name list the effective gate and silently overrode the caller's finding.

        The explicit allow/deny rules above still win: an operator who put the
        action in ``auto_approve``, or a human who previously chose "always",
        has stated a policy, and ``require`` does not revoke it.
        """
        params = params or {}
        signature = self._signature(action, tool_name, params, user_id)
        if signature in self._approved_signatures:
            self._approved_signatures.remove(signature)
            req = ApprovalRequest(action=action, tool_name=tool_name, params=params, user_id=user_id, status=ApprovalStatus.APPROVED, reason="approved by prior request")
            self._history.append(req)
            self._save_state()
            return req
        if action in self._auto_approve:
            req = ApprovalRequest(action=action, tool_name=tool_name, params=params, user_id=user_id, status=ApprovalStatus.APPROVED)
            self._history.append(req)
            return req
        if action in self._auto_deny:
            req = ApprovalRequest(action=action, tool_name=tool_name, params=params, user_id=user_id, status=ApprovalStatus.DENIED, reason="auto-denied by policy")
            self._history.append(req)
            return req
        if require or action in self._require:
            existing = self._existing_pending(signature)
            if existing:
                return existing
            req = ApprovalRequest(
                action=action, tool_name=tool_name, params=params,
                user_id=user_id, session_key=session_key,
            )
            self._pending[req.id] = req
            self._pending_by_signature[signature] = req.id
            self._save_state()
            return req
        if self._default_policy == "approve":
            req = ApprovalRequest(
                action=action, tool_name=tool_name, params=params, user_id=user_id,
                status=ApprovalStatus.APPROVED,
            )
            self._history.append(req)
            return req
        if self._default_policy == "deny":
            req = ApprovalRequest(action=action, tool_name=tool_name, params=params, user_id=user_id, status=ApprovalStatus.DENIED, reason="denied by default policy")
            self._history.append(req)
            return req

        existing = self._existing_pending(signature)
        if existing:
            return existing
        req = ApprovalRequest(
            action=action, tool_name=tool_name, params=params,
            user_id=user_id, session_key=session_key,
        )
        self._pending[req.id] = req
        self._pending_by_signature[signature] = req.id
        self._save_state()
        return req

    def approve(self, request_id: str, level: str = "", decided_by: str = "") -> bool:
        req = self._pending.pop(request_id, None)
        if not req:
            return False
        self._pending_by_signature.pop(self._signature(req.action, req.tool_name, req.params, req.user_id), None)
        req.status = ApprovalStatus.APPROVED
        req.reason = level  # 将 session/always 级别存入 reason,供 approval_gate 解析
        req.decided_by = decided_by
        req.decided_at = datetime.now().isoformat()
        if request_id not in self._waiters:
            self._approved_signatures.add(self._signature(req.action, req.tool_name, req.params, req.user_id))
        self._history.append(req)
        self._save_state()
        self._notify_waiter(request_id)
        return True

    def deny(self, request_id: str, reason: str = "", decided_by: str = "") -> bool:
        req = self._pending.pop(request_id, None)
        if not req:
            return False
        self._pending_by_signature.pop(self._signature(req.action, req.tool_name, req.params, req.user_id), None)
        req.status = ApprovalStatus.DENIED
        req.reason = reason
        req.decided_by = decided_by
        req.decided_at = datetime.now().isoformat()
        self._history.append(req)
        self._save_state()
        self._notify_waiter(request_id)
        return True

    async def wait_for_decision(self, request_id: str, timeout_seconds: int) -> ApprovalRequest | None:
        """Wait until a pending request is approved or denied."""
        req = self._pending.get(request_id)
        if req is None:
            return self._find_history(request_id)

        waiter = self._waiters.setdefault(request_id, asyncio.Event())
        try:
            await asyncio.wait_for(waiter.wait(), timeout=max(0.1, timeout_seconds))
        except asyncio.TimeoutError:
            req = self._pending.pop(request_id, None)
            if req is None:
                return self._find_history(request_id)
            self._pending_by_signature.pop(self._signature(req.action, req.tool_name, req.params, req.user_id), None)
            req.status = ApprovalStatus.EXPIRED
            req.reason = "approval timed out"
            req.decided_at = datetime.now().isoformat()
            self._history.append(req)
            self._save_state()
            return req
        finally:
            self._waiters.pop(request_id, None)
        return self._find_history(request_id)

    def cancel_session(self, session_key: str, *, reason: str = "channel disconnected") -> int:
        """Deny every pending approval for `session_key`. Returns how many.

        The escape valve for a channel that goes away (socket drop, /quit) while
        a turn is parked on wait_for_decision. Nobody can answer that prompt
        anymore, so without this the turn sat blocked for the full
        wait_timeout_seconds — holding the session lock, so the user's next
        message queued behind a decision that could never arrive.

        Denying (rather than releasing as approved) is the safe direction: the
        tool call was risky enough to require a human, and no human is there.
        """
        if not session_key:
            return 0
        victims = [rid for rid, req in self._pending.items() if req.session_key == session_key]
        for rid in victims:
            self.deny(rid, reason=reason, decided_by="system")
        if victims:
            logger.info(
                "Denied {} pending approval(s) for disconnected session {}",
                len(victims), session_key,
            )
        return len(victims)

    def get_pending(self) -> list[ApprovalRequest]:
        return list(self._pending.values())

    def get(self, request_id: str) -> ApprovalRequest | None:
        return self._pending.get(request_id)

    def _signature(self, action: str, tool_name: str, params: dict[str, Any], user_id: str) -> str:
        payload = json.dumps(params, ensure_ascii=False, sort_keys=True, default=str)
        return hashlib.sha256(f"{user_id}:{action}:{tool_name}:{payload}".encode()).hexdigest()

    def _existing_pending(self, signature: str) -> ApprovalRequest | None:
        request_id = self._pending_by_signature.get(signature)
        if not request_id:
            return None
        return self._pending.get(request_id)

    def _find_history(self, request_id: str) -> ApprovalRequest | None:
        for req in reversed(self._history):
            if req.id == request_id:
                return req
        return None

    def _notify_waiter(self, request_id: str) -> None:
        waiter = self._waiters.get(request_id)
        if waiter:
            waiter.set()

    def _load_state(self) -> None:
        if not self._store_path or not self._store_path.exists():
            return
        try:
            data = json.loads(self._store_path.read_text(encoding="utf-8"))
            pending = []
            for item in data.get("pending", []):
                req = self._request_from_dict(item)
                if req.status == ApprovalStatus.PENDING:
                    pending.append(req)
            history = [self._request_from_dict(item) for item in data.get("history", [])]
            approved_signatures = set(data.get("approved_signatures", []))
        except Exception as e:
            logger.warning("Failed to load approval state from {}: {}", self._store_path, e)
            return
        self._pending = {req.id: req for req in pending}
        self._history = history[-500:]
        self._approved_signatures = approved_signatures
        self._pending_by_signature = {
            self._signature(req.action, req.tool_name, req.params, req.user_id): req.id
            for req in pending
        }

    def _save_state(self) -> None:
        if not self._store_path:
            return
        payload = {
            "pending": [self._request_to_dict(req) for req in self._pending.values()],
            "history": [self._request_to_dict(req) for req in self._history[-500:]],
            "approved_signatures": sorted(self._approved_signatures),
        }
        self._store_path.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp_path = tempfile.mkstemp(dir=str(self._store_path.parent), prefix=".approvals_", suffix=".tmp")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                json.dump(payload, handle, ensure_ascii=False, indent=2)
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(tmp_path, self._store_path)
        except BaseException:
            try:
                os.unlink(tmp_path)
            except OSError:
                pass
            raise

    @staticmethod
    def _request_to_dict(req: ApprovalRequest) -> dict[str, Any]:
        return {
            "id": req.id,
            "action": req.action,
            "tool_name": req.tool_name,
            "params": req.params,
            "user_id": req.user_id,
            "status": req.status.value,
            "reason": req.reason,
            "decided_by": req.decided_by,
            "created_at": req.created_at,
            "decided_at": req.decided_at,
            "session_key": req.session_key,
        }

    @staticmethod
    def _request_from_dict(data: dict[str, Any]) -> ApprovalRequest:
        return ApprovalRequest(
            id=data.get("id", uuid.uuid4().hex[:10]),
            action=data.get("action", ""),
            tool_name=data.get("tool_name", ""),
            params=dict(data.get("params", {})),
            user_id=data.get("user_id", ""),
            status=ApprovalStatus(data.get("status", ApprovalStatus.PENDING.value)),
            reason=data.get("reason", ""),
            decided_by=data.get("decided_by", ""),
            created_at=data.get("created_at", datetime.now().isoformat()),
            decided_at=data.get("decided_at", ""),
            # Absent in state files written before session scoping existed.
            session_key=data.get("session_key", ""),
        )


@dataclass
class Credential:
    id: str = field(default_factory=lambda: uuid.uuid4().hex[:8])
    name: str = ""
    tool_scope: str = "*"
    value_hash: str = ""
    created_at: str = field(default_factory=lambda: datetime.now().isoformat())
    rotated_at: str = ""
    _value: str = field(default="", repr=False)


class CredentialManager:
    """Manages agent credentials with isolation per tool and rotation support."""

    def __init__(
        self,
        store_path: Path,
        encryption_key_env: str = "ECHO_AGENT_CREDENTIAL_KEY",
        require_encryption: bool = False,
        key_path: Path | None = None,
    ):
        self._store_path = store_path
        self._encryption_key_env = encryption_key_env
        self._require_encryption = require_encryption
        # Where to persist an auto-generated Fernet key when the env var is
        # unset. Defaults next to the credential store's parent (the workspace).
        self._key_path = key_path or (store_path.parent.parent / ".credential_key")
        self._fernet_cache: Any | None = None
        self._credentials: dict[str, Credential] = {}
        self._audit: list[dict[str, Any]] = []
        self._load()

    def _fernet(self) -> Any | None:
        if self._fernet_cache is not None:
            return self._fernet_cache
        try:
            from cryptography.fernet import Fernet

            from echo_agent.permissions.credential_key import resolve_or_create_key
        except ImportError as exc:
            raise RuntimeError("cryptography is required for encrypted credential storage") from exc
        try:
            key = resolve_or_create_key(
                self._key_path, env_name=self._encryption_key_env
            )
        except OSError as exc:
            if self._require_encryption:
                raise RuntimeError(
                    f"Credential encryption required but no key is available and one "
                    f"could not be created at {self._key_path}: {exc}"
                ) from exc
            return None
        self._fernet_cache = Fernet(key)
        return self._fernet_cache

    def _encode_secret(self, value: str) -> tuple[str, str]:
        fernet = self._fernet()
        if not fernet:
            logger.warning(
                "Credential encryption key not configured; storing credential metadata with plaintext fallback"
            )
            return "plain", value
        return "fernet", fernet.encrypt(value.encode()).decode()

    def _decode_secret(self, item: dict[str, Any]) -> str:
        encoding = item.get("encoding", "plain")
        if encoding == "plain":
            return item.get("_value", "")
        if encoding == "fernet":
            fernet = self._fernet()
            if not fernet:
                raise RuntimeError(
                    f"Credential file is encrypted but {self._encryption_key_env} is not set"
                )
            from cryptography.fernet import InvalidToken
            try:
                return fernet.decrypt(item.get("value_enc", "").encode()).decode()
            except InvalidToken as exc:
                raise RuntimeError(
                    "凭证密钥已变更或损坏，无法解密现有凭证，请重新录入凭证"
                ) from exc
        raise RuntimeError(f"Unsupported credential encoding: {encoding}")

    def _load(self) -> None:
        if not self._store_path.exists():
            return
        try:
            data = json.loads(self._store_path.read_text(encoding="utf-8"))
            for item in data.get("credentials", []):
                value = self._decode_secret(item)
                cred = Credential(
                    id=item["id"], name=item["name"],
                    tool_scope=item.get("tool_scope", "*"),
                    value_hash=item.get("value_hash", ""),
                    created_at=item.get("created_at", ""),
                    rotated_at=item.get("rotated_at", ""),
                    _value=value,
                )
                self._credentials[cred.id] = cred
        except RuntimeError:
            raise  # 解密失败属确定性错误，必须显式暴露
        except Exception as e:
            logger.warning("Failed to load credentials: {}", e)

    def _save(self) -> None:
        self._store_path.parent.mkdir(parents=True, exist_ok=True)
        items: list[dict[str, Any]] = []
        for c in self._credentials.values():
            encoding, encoded = self._encode_secret(c._value)
            item = {
                "id": c.id,
                "name": c.name,
                "tool_scope": c.tool_scope,
                "value_hash": c.value_hash,
                "created_at": c.created_at,
                "rotated_at": c.rotated_at,
                "encoding": encoding,
            }
            if encoding == "fernet":
                item["value_enc"] = encoded
            else:
                item["_value"] = encoded
            items.append(item)
        data = {
            "format": "echo-agent-credentials-v2",
            "encryption_key_env": self._encryption_key_env,
            "credentials": items,
        }
        self._store_path.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")

    def store(self, name: str, value: str, tool_scope: str = "*") -> Credential:
        cred = Credential(
            name=name, tool_scope=tool_scope, _value=value,
            value_hash=hashlib.sha256(value.encode()).hexdigest()[:16],
        )
        self._credentials[cred.id] = cred
        self._audit_log("store", cred)
        self._save()
        return cred

    def get(self, name: str, tool_scope: str = "*") -> str | None:
        for cred in self._credentials.values():
            if cred.name == name and (cred.tool_scope == "*" or cred.tool_scope == tool_scope):
                self._audit_log("access", cred)
                return cred._value
        return None

    def rotate(self, name: str, new_value: str) -> bool:
        for cred in self._credentials.values():
            if cred.name == name:
                cred._value = new_value
                cred.value_hash = hashlib.sha256(new_value.encode()).hexdigest()[:16]
                cred.rotated_at = datetime.now().isoformat()
                self._audit_log("rotate", cred)
                self._save()
                return True
        return False

    def delete(self, name: str) -> bool:
        to_remove = [cid for cid, c in self._credentials.items() if c.name == name]
        for cid in to_remove:
            self._audit_log("delete", self._credentials[cid])
            del self._credentials[cid]
        if to_remove:
            self._save()
        return bool(to_remove)

    def get_for_tool(self, tool_name: str) -> dict[str, str]:
        result = {}
        for cred in self._credentials.values():
            if cred.tool_scope == "*" or cred.tool_scope == tool_name:
                result[cred.name] = cred._value
        return result

    def _audit_log(self, action: str, cred: Credential) -> None:
        self._audit.append({
            "action": action, "credential": cred.name,
            "tool_scope": cred.tool_scope,
            "timestamp": datetime.now().isoformat(),
        })
