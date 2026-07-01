"""Gateway authentication — allowlist and pairing code authorization."""

from __future__ import annotations

import json
import hmac
import secrets
import time
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.config.schema import GatewayAuthConfig


class GatewayAuth:

    def __init__(self, config: GatewayAuthConfig, data_dir: Path):
        self._mode = config.mode
        self._allowed = set(config.allowed_users)
        self._admins = set(config.admin_users)
        self._api_tokens = list(config.api_tokens)
        self._admin_tokens = list(config.admin_tokens)
        self._allowed_origins = set(config.allowed_origins)
        self.token_header = config.token_header
        self._pairing_ttl = config.pairing_ttl_seconds
        self._data_dir = data_dir / "gateway_auth"
        self._data_dir.mkdir(parents=True, exist_ok=True)
        self._audit_path = self._data_dir / "audit.jsonl"

        self._approved: dict[str, set[str]] = {}
        self._pending_codes: dict[str, dict[str, Any]] = {}
        self._verify_failures: dict[str, list[float]] = {}
        self._lockout_seconds = 300
        self._max_failures = 5
        self._load_approved()
        self._load_pending()

    def is_authorized(self, platform: str, user_id: str) -> bool:
        """Normal authorization only (open / allowlist / pairing).

        The loopback exemption is applied by the transport layer (see
        gateway/server.py), NOT here: a loopback peer merely means "this local
        user need not be pre-listed", it never means "this caller may claim any
        identity". Collapsing those two into an unconditional ``return True``
        was the P0 in 820563d — removed."""
        if self._mode == "open":
            return True
        if self._mode == "allowlist":
            return user_id in self._allowed or f"{platform}:{user_id}" in self._allowed
        if self._mode == "pairing":
            if user_id in self._allowed or f"{platform}:{user_id}" in self._allowed:
                return True
            approved = self._approved.get(platform, set())
            return user_id in approved
        return False

    def authenticate_token(self, token: str) -> bool:
        if not self._api_tokens:
            return True
        if not token:
            return False
        return any(hmac.compare_digest(token, configured) for configured in self._api_tokens)

    def authenticate_admin_token(self, token: str) -> bool:
        """Authorize a high-risk admin endpoint.

        If ``admin_tokens`` is configured, only those tokens pass — this gives
        real scope separation between chat-level and admin-level callers. When
        ``admin_tokens`` is empty we fall back to ``api_tokens`` so existing
        single-token deployments keep working (no silent privilege change)."""
        admin = self._admin_tokens or self._api_tokens
        if not admin:
            return True  # unauthenticated deployment (loopback, no tokens)
        if not token:
            return False
        return any(hmac.compare_digest(token, configured) for configured in admin)

    def is_origin_allowed(self, origin: str, sec_fetch_site: str) -> bool:
        """CSRF defense for browser clients — opt-in via ``allowed_origins``.

        Disabled by default (empty ``allowed_origins``) so it never breaks
        existing clients: native HTTP callers, the same-origin playground, or a
        webview desktop client (which sends a cross-site Origin like
        ``tauri://localhost``). When the operator opts in by configuring
        ``allowed_origins``, genuine cross-site browser requests are rejected
        unless their Origin is on the allowlist — this is what blocks
        CSRF-to-localhost / DNS-rebinding from a malicious public web page.
        """
        # Opt-in: no allowlist configured → CSRF enforcement off (no behavior change).
        if not self._allowed_origins:
            return True
        # No browser headers at all → not a browser-driven request → allow.
        if not origin and not sec_fetch_site:
            return True
        # Same-origin / same-site / direct navigation are safe.
        if sec_fetch_site in ("same-origin", "same-site", "none"):
            return True
        # Cross-site (or unknown): only an explicitly allowlisted Origin may proceed.
        return bool(origin) and origin in self._allowed_origins

    def is_cross_site_browser(self, origin: str, sec_fetch_site: str) -> bool:
        """Whether the request is an *explicit cross-site browser* request.

        Default-on CSRF primitive for the main channels (WS handshake and
        POST /message). Unlike ``is_origin_allowed`` (opt-in, off when
        ``allowed_origins`` is empty), this stays on even with an empty
        allowlist — that is what closes the loopback WebSocket hole where a
        malicious page drives the local agent. Native clients (cli/curl/SDK)
        send neither header, so they are never flagged."""
        origin = (origin or "").strip()
        sec_fetch_site = (sec_fetch_site or "").strip()
        # No browser metadata at all → native client → not a browser request.
        if not origin and sec_fetch_site in ("", "none"):
            return False
        # Same-origin / same-site are safe.
        if sec_fetch_site in ("same-origin", "same-site"):
            return False
        # Explicitly allowlisted Origin is trusted (webview / desktop escape hatch).
        if origin and origin in self._allowed_origins:
            return False
        # Everything else that carries a cross-site Origin or Sec-Fetch-Site.
        return True

    def token_from_headers(self, headers: Any) -> str:
        token = headers.get(self.token_header, "")
        if token:
            return token.strip()
        auth = headers.get("Authorization", "")
        if auth.lower().startswith("bearer "):
            return auth[7:].strip()
        return ""

    def is_admin(self, platform: str, user_id: str, token: str = "") -> bool:
        if token and self._api_tokens and self.authenticate_token(token):
            return True
        return user_id in self._admins or f"{platform}:{user_id}" in self._admins

    def audit(self, action: str, *, platform: str = "", user_id: str = "", ok: bool = True, reason: str = "") -> None:
        record = {
            "ts": time.time(),
            "action": action,
            "platform": platform,
            "user_id": user_id,
            "ok": ok,
            "reason": reason,
        }
        # Auditing is a side-channel: its failure must never break the caller's
        # core flow (e.g. the WS message loop). Self-heal a missing data dir —
        # the process may have been started from a cwd that was later unlinked —
        # and downgrade any write failure to a warning.
        try:
            self._data_dir.mkdir(parents=True, exist_ok=True)
            with self._audit_path.open("a", encoding="utf-8") as f:
                f.write(json.dumps(record, ensure_ascii=False) + "\n")
        except OSError as e:
            logger.warning("audit log write failed ({}): {}", action, e)

    def generate_pairing_code(self, platform: str) -> str:
        code = secrets.token_hex(5).upper()
        self._pending_codes[code] = {
            "platform": platform,
            "created_at": time.time(),
        }
        self._save_pending()
        logger.info("Pairing code generated for {}", platform)
        self.audit("pair_generate", platform=platform)
        return code

    def verify_pairing(self, platform: str, user_id: str, code: str) -> bool:
        code = code.upper().strip()
        lockout_key = f"{platform}:{user_id}"

        if self._is_locked_out(lockout_key):
            self.audit("pair_verify", platform=platform, user_id=user_id, ok=False, reason="locked_out")
            return False

        entry = self._pending_codes.get(code)
        if entry is None:
            self._record_verify_failure(lockout_key)
            return False

        if time.time() - entry["created_at"] > self._pairing_ttl:
            del self._pending_codes[code]
            self._save_pending()
            self._record_verify_failure(lockout_key)
            return False

        if entry["platform"] != platform:
            self._record_verify_failure(lockout_key)
            return False

        del self._pending_codes[code]
        self._save_pending()

        if platform not in self._approved:
            self._approved[platform] = set()
        self._approved[platform].add(user_id)
        self._save_approved(platform)

        self._verify_failures.pop(lockout_key, None)
        logger.info("User {}:{} paired successfully", platform, user_id)
        self.audit("pair_verify", platform=platform, user_id=user_id)
        return True

    def _is_locked_out(self, lockout_key: str) -> bool:
        # Keyed by platform:user — keying by platform alone would let one
        # remote attacker lock out pairing for every user on the platform.
        failures = self._verify_failures.get(lockout_key, [])
        if len(failures) < self._max_failures:
            return False
        recent = [t for t in failures if time.time() - t < self._lockout_seconds]
        self._verify_failures[lockout_key] = recent
        return len(recent) >= self._max_failures

    def _record_verify_failure(self, lockout_key: str) -> None:
        if lockout_key not in self._verify_failures:
            self._verify_failures[lockout_key] = []
        self._verify_failures[lockout_key].append(time.time())

    def _load_approved(self) -> None:
        for path in self._data_dir.glob("*_approved.json"):
            platform = path.stem.replace("_approved", "")
            try:
                data = json.loads(path.read_text(encoding="utf-8"))
                self._approved[platform] = set(data)
            except Exception as e:
                logger.debug("Failed to load approved list for {}: {}", platform, e)

    def _save_approved(self, platform: str) -> None:
        path = self._data_dir / f"{platform}_approved.json"
        users = sorted(self._approved.get(platform, set()))
        path.write_text(json.dumps(users, indent=2), encoding="utf-8")

    def _load_pending(self) -> None:
        path = self._data_dir / "pending_codes.json"
        if path.exists():
            try:
                self._pending_codes = json.loads(path.read_text(encoding="utf-8"))
            except Exception as e:
                logger.debug("Failed to load pending codes: {}", e)
                self._pending_codes = {}
        now = time.time()
        expired = [k for k, v in self._pending_codes.items()
                   if now - v.get("created_at", 0) > self._pairing_ttl]
        for k in expired:
            del self._pending_codes[k]

    def _save_pending(self) -> None:
        path = self._data_dir / "pending_codes.json"
        path.write_text(json.dumps(self._pending_codes, indent=2), encoding="utf-8")
