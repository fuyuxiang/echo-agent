"""Gateway authentication — allowlist and pairing code authorization."""

from __future__ import annotations

import hashlib
import json
import hmac
import secrets
import time
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.config.schema import GatewayAuthConfig


class GatewayAuth:

    # Host names that mean "this machine" when the gateway is bound to a
    # loopback address. Listed explicitly (rather than resolving "anything
    # whose IP is loopback") because the relevant attack — DNS rebinding — is
    # precisely a case where the browser sends a Host string that LOOKS local
    # but is not: it is the attacker's domain that the rebinding has made
    # resolve to 127.0.0.1. The string check is the whole point.
    _LOOPBACK_HOSTS = frozenset({"localhost", "127.0.0.1", "[::1]"})

    def __init__(
        self, config: GatewayAuthConfig, data_dir: Path,
        *, bound_host: str | None = None,
    ):
        self._mode = config.mode
        self._allowed = set(config.allowed_users)
        self._admins = set(config.admin_users)
        self._api_tokens = list(config.api_tokens)
        self._admin_tokens = list(config.admin_tokens)
        self._allowed_origins = set(config.allowed_origins)
        self.token_header = config.token_header
        self._pairing_ttl = config.pairing_ttl_seconds
        # allowed_hosts is a configured escape hatch; empty defers to a default
        # derived from the bind address. See is_host_allowed.
        self._allowed_hosts = set(config.allowed_hosts)
        self._bound_host = (bound_host or "").strip()
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
        """Authorize a read/chat-level endpoint.

        Admin scope *implies* read scope: a token in ``admin_tokens`` passes here
        too. Without that implication the two lists were disjoint sets rather
        than a hierarchy, and a deployment that configured a separate
        ``admin_tokens`` had no token that could use the dashboard at all — the
        api token logged in but every admin control 403'd, while the admin token
        was rejected by the read guard the login probe itself goes through
        (401). Nothing is widened for chat-level callers: this only lets the
        strictly higher scope reach the lower one."""
        if not self._api_tokens and not self._admin_tokens:
            return True
        if not token:
            return False
        allowed = [*self._api_tokens, *self._admin_tokens]
        return any(hmac.compare_digest(token, configured) for configured in allowed)

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

    def is_cross_site_browser(
        self, origin: str, sec_fetch_site: str, host: str = "",
    ) -> bool:
        """Whether the request is an *explicit cross-site browser* request.

        Default-on CSRF primitive for the main channels (WS handshake and
        POST /message). Unlike ``is_origin_allowed`` (opt-in, off when
        ``allowed_origins`` is empty), this stays on even with an empty
        allowlist — that is what closes the loopback WebSocket hole where a
        malicious page drives the local agent. Native clients (cli/curl/SDK)
        send neither header, so they are never flagged.

        ``host`` is the request's Host header. Pass it whenever it is available:
        it is what lets ``same-site`` be checked rather than trusted, and it is
        what ``is_host_allowed`` uses to close the DNS-rebinding hole (see that
        method). The two checks compose: same-origin/same-site routing is
        decided here, and on top of that the Host must name a host this gateway
        was reached on — a rebinding page sends ``Sec-Fetch-Site: same-origin``
        plus a Host it controls, and Origin-vs-Host alone (both attacker's)
        cannot stop that.
        """
        origin = (origin or "").strip()
        sec_fetch_site = (sec_fetch_site or "").strip()
        # No browser metadata at all → native client → not a browser request.
        # Native clients send a Host but the CSRF primitive does not care —
        # the rebinding check below still applies if they were to forge a
        # browser-shaped request, but real curl/sdk never carry an Origin.
        if not origin and sec_fetch_site in ("", "none"):
            return False
        # Same-origin is safe by definition. ALMOST — see is_host_allowed for
        # the rebinding case this does not catch.
        if sec_fetch_site == "same-origin":
            return False
        # same-site is NOT same-origin: the browser is telling us the initiator
        # shares a registrable domain, which still leaves a different subdomain,
        # a different port, and (for localhost) any other local service — none of
        # which this gate wants to trust. It was previously accepted outright,
        # keeping the door open for exactly the CSRF-to-localhost / DNS-rebinding
        # cases the gate exists to stop. Verify it really is the same origin by
        # comparing the Origin against the Host we were reached on; with no Host
        # to compare, fall through to the allowlist rather than assume.
        if sec_fetch_site == "same-site" and host and self._origin_matches_host(origin, host):
            return False
        # Explicitly allowlisted Origin is trusted (webview / desktop escape hatch).
        if origin and origin in self._allowed_origins:
            return False
        # Everything else that carries a cross-site Origin or Sec-Fetch-Site.
        return True

    def is_host_allowed(self, host: str) -> bool:
        """Whether the request's Host names a host this gateway was reached on.

        DNS rebinding closes the CSRF gate's other checks. The page is loaded
        from ``evil.example``, the attacker rebinds its DNS to 127.0.0.1, and
        the browser's subsequent request hits this gateway with:

          - ``Origin: http://evil.example:58123`` (attacker's page origin)
          - ``Sec-Fetch-Site: same-origin`` (correct: same scheme/host/port as
            the page that made the request)
          - ``Host: evil.example:58123`` (the rebinding target)

        All three are consistent with each other AND with the gateway's own
        bind (loopback peer, so ``_is_loopback_peer`` grants the trust
        exemption). ``is_cross_site_browser`` therefore returns False. The only
        signal that breaks the picture is ``Host``: this gateway was not
        reached on a host name that resolves to ``evil.example`` from the
        user's perspective.

        Resolution order:

          1. ``allowed_hosts`` (configured escape hatch for reverse proxies).
          2. Empty config + bind is loopback → the loopback host set
             (localhost / 127.0.0.1 / ::1). This is the secure default.
          3. Empty config + bind is non-loopback → no default. An operator who
             bound to 0.0.0.0 must list their proxy domain explicitly; the
             loopback exemption does NOT extend to attacker-supplied names.

        An empty Host is treated as untrusted — a bare origin alone is not
        enough to be sure the peer reached a host we control. Native clients
        typically send Host to a loopback address, so the second branch is
        where they pass.
        """
        host = (host or "").strip()
        if not host:
            return False
        # Strip IPv6 brackets so "[::1]" matches the entry in _LOOPBACK_HOSTS.
        normalized = self._normalize_host(host)
        if not normalized:
            return False
        if normalized in self._allowed_hosts:
            return True
        if not self._allowed_hosts and self._bound_is_loopback():
            return normalized in self._LOOPBACK_HOSTS
        return False

    @staticmethod
    def _normalize_host(host: str) -> str:
        """Lowercase, strip port, preserve IPv6 brackets.

        ``localhost:58123`` and ``[::1]:58123`` should compare equal to their
        bare-host forms. The bracket shape is preserved on IPv6 because that is
        how it appears in the Host header.
        """
        if host.startswith("["):
            hostname, _, _ = host.partition("]")
            return "[" + hostname.lstrip("[").lower() + "]"
        lowered = host.lower()
        # Strip a single trailing :port (we never see multiple colons outside of
        # IPv6 literals, which the branch above handled).
        if ":" in lowered:
            return lowered.rsplit(":", 1)[0]
        return lowered

    def _bound_is_loopback(self) -> bool:
        from echo_agent.cli.runtime_probe import _WILDCARD_HOSTS
        import ipaddress

        if not self._bound_host:
            return False
        if self._bound_host in _WILDCARD_HOSTS:
            return False
        try:
            return ipaddress.ip_address(self._bound_host).is_loopback
        except ValueError:
            return self._bound_host == "localhost"

    @staticmethod
    def _origin_matches_host(origin: str, host: str) -> bool:
        """Whether ``origin``'s authority is the same as ``host``.

        Compared as host:port, so http://localhost:5173 does not pass for a
        gateway serving on localhost:58123 — a different port is a different
        origin, and on a dev box it is a different program.
        """
        from urllib.parse import urlsplit

        if not origin or not host:
            return False
        try:
            parsed = urlsplit(origin)
        except ValueError:
            return False
        if not parsed.hostname:
            return False
        origin_port = parsed.port or (443 if parsed.scheme == "https" else 80)

        host = host.strip().lower()
        # Host may or may not carry a port; IPv6 literals keep their brackets.
        if host.startswith("["):
            hostname, _, port_text = host.partition("]")
            hostname = hostname[1:]
            port_text = port_text.lstrip(":")
        else:
            hostname, _, port_text = host.partition(":")
        if not hostname:
            return False
        if port_text:
            try:
                host_port = int(port_text)
            except ValueError:
                return False
        else:
            # No port in Host: it is the scheme default for how we were reached,
            # which for an Origin-bearing browser request is the Origin's scheme.
            host_port = 443 if parsed.scheme == "https" else 80

        return parsed.hostname.lower() == hostname and origin_port == host_port

    def token_from_headers(self, headers: Any) -> str:
        token = headers.get(self.token_header, "")
        if token:
            return token.strip()
        auth = headers.get("Authorization", "")
        if auth.lower().startswith("bearer "):
            return auth[7:].strip()
        return ""

    def token_identifier(self, token: str) -> str:
        """A stable, non-reversible label for "which token did this".

        For audit records that are readable by a wider audience than the token
        holder. Callers used to store ``token[:8]``, which put a slice of an
        *admin* credential into cron authorization records that any read-scope
        caller can fetch via GET /cron — and a token of 8 characters or fewer was
        recorded in full. A truncated digest identifies the token across records
        (so "who authorized this" still works) without carrying material an
        attacker can use or extend."""
        token = (token or "").strip()
        if not token:
            return ""
        return "tok_" + hashlib.sha256(token.encode("utf-8")).hexdigest()[:12]

    def is_admin(self, platform: str, user_id: str, token: str = "") -> bool:
        """Whether this caller has admin scope, by token or by admin_users.

        The token branch delegates to ``authenticate_admin_token`` so it follows
        the same hierarchy as the HTTP admin guard: with ``admin_tokens``
        configured only those tokens grant admin, otherwise ``api_tokens`` do.
        It previously gated on ``self._api_tokens`` and used the *read*-level
        check, so a deployment with only ``admin_tokens`` got no admin from its
        admin token, while a read-only api token got admin whenever both lists
        were set.

        The explicit "some list is configured" test matters: with no tokens at
        all ``authenticate_admin_token`` accepts anything (unauthenticated
        deployment), which must not turn an arbitrary string into admin here."""
        configured = self._admin_tokens or self._api_tokens
        if token and configured and self.authenticate_admin_token(token):
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
