"""MCP OAuth 2.1 PKCE client — browser-based authorization for HTTP MCP servers.

Follows the MCP authorization flow: discover the Protected Resource Metadata for
the MCP server, follow it to the Authorization Server's metadata, then run
authorization-code + PKCE with the RFC 8707 ``resource`` parameter bound to the
MCP server. Dynamic client registration is used when the AS offers it, and the
resulting credentials are persisted so a restart does not re-register.

Three rules hold throughout, each written against a way the previous version
could be induced to leak a credential:

* **Endpoints are validated before use.** A metadata document is fetched from
  the network; treating ``token_endpoint`` as trustworthy meant a hostile or
  compromised metadata response could redirect the authorization code and
  PKCE verifier to an attacker's origin. Endpoints must be HTTPS (loopback
  excepted for local development) and must belong to the issuer they came from.
* **Tokens are written 0600, atomically, inside their directory.** They were
  written 0644 by ``write_text``, and ``server_name`` reached the path unchecked
  so ``../../x`` escaped the token directory entirely.
* **Every wait is bounded and every failure resolves the future.** A state
  mismatch used to leave the callback future unresolved, so the flow sat for the
  full 300s before failing with a timeout that named the wrong cause.
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import os
import re
import secrets
import tempfile
import time
import webbrowser
from pathlib import Path
from typing import Any
from urllib.parse import urlencode, urljoin, urlparse

import aiohttp
from loguru import logger

#: Server names that may become a filename. Anything else is rejected rather
#: than sanitised: a name that needs rewriting to be safe is a configuration
#: mistake worth reporting, and silent rewriting can still collide.
_SAFE_SERVER_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")

#: Clock skew margin when deciding whether an access token is still usable.
_EXPIRY_MARGIN_SECONDS = 60

_METADATA_TIMEOUT = aiohttp.ClientTimeout(total=10)
_TOKEN_TIMEOUT = aiohttp.ClientTimeout(total=15)


class MCPOAuthError(RuntimeError):
    """Raised when the OAuth flow cannot be completed safely."""


def _is_loopback(host: str) -> bool:
    return host in ("localhost", "127.0.0.1", "::1", "[::1]")


def require_secure_endpoint(url: str, label: str) -> str:
    """Validate that *url* is a usable OAuth endpoint, or raise.

    HTTPS is mandatory because these requests carry the authorization code, the
    PKCE verifier and the refresh token. Loopback is exempt so a developer can
    run an AS locally, and that exemption is safe for the same reason it is in
    the OAuth specs: traffic never leaves the host.
    """
    parsed = urlparse(url)
    if parsed.scheme not in ("http", "https"):
        raise MCPOAuthError(f"{label} must be an http(s) URL, got {url!r}")
    if not parsed.hostname:
        raise MCPOAuthError(f"{label} has no host: {url!r}")
    if parsed.scheme == "http" and not _is_loopback(parsed.hostname):
        raise MCPOAuthError(
            f"{label} must use HTTPS (got {url!r}) — it carries OAuth credentials"
        )
    return url


def same_origin(a: str, b: str) -> bool:
    """Whether two URLs share scheme, host and effective port."""
    pa, pb = urlparse(a), urlparse(b)
    default = {"http": 80, "https": 443}
    return (
        pa.scheme == pb.scheme
        and (pa.hostname or "").lower() == (pb.hostname or "").lower()
        and (pa.port or default.get(pa.scheme, 0)) == (pb.port or default.get(pb.scheme, 0))
    )


class MCPOAuthClient:

    def __init__(self, server_name: str, server_url: str, token_dir: Path):
        if not _SAFE_SERVER_NAME_RE.match(server_name):
            # `server_name` is a config key that becomes a filename. Unchecked,
            # "../../escaped" wrote the token outside the token directory.
            raise MCPOAuthError(
                f"MCP server name {server_name!r} cannot be used for credential storage: "
                "use only letters, digits, dot, dash and underscore (max 64 chars)"
            )
        self._server_name = server_name
        self._server_url = server_url.rstrip("/")
        require_secure_endpoint(self._server_url, "MCP server URL")

        self._token_dir = token_dir
        # 0700 from creation: the directory holds bearer tokens, so it must not
        # be listable by other local users.
        self._token_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        self._harden_dir_permissions()
        self._token_file = self._token_dir / f"{server_name}.json"
        self._client_file = self._token_dir / f"{server_name}.client.json"

        resolved = (self._token_dir / f"{server_name}.json").resolve()
        if self._token_dir.resolve() not in resolved.parents:
            raise MCPOAuthError(
                f"Refusing to store credentials outside {self._token_dir} for {server_name!r}"
            )

    def _harden_dir_permissions(self) -> None:
        # mkdir(mode=...) is a no-op when the directory already exists, which is
        # the common case after the first run.
        try:
            os.chmod(self._token_dir, 0o700)
        except OSError as e:
            logger.debug("Could not tighten permissions on {}: {}", self._token_dir, e)

    # ── public API ──────────────────────────────────────────────────────────

    def get_access_token(self) -> str | None:
        token_data = self._load_token()
        if not token_data or self._is_expired(token_data):
            return None
        return token_data.get("access_token")

    async def ensure_token(self) -> str:
        token_data = self._load_token()
        if token_data:
            if not self._is_expired(token_data):
                return token_data["access_token"]
            if token_data.get("refresh_token"):
                refreshed = await self._refresh_token(token_data)
                if refreshed:
                    return refreshed["access_token"]

        token_data = await self._authorize()
        return token_data["access_token"]

    async def refresh_after_401(self) -> str | None:
        """Re-acquire an access token after the resource server rejected one.

        A token can be revoked or invalidated long before its nominal expiry, and
        nothing here reacted to that: the 401 propagated as a generic connection
        error and the stored token stayed in place, so every reconnect replayed
        the same dead credential. Returns None when only a fresh interactive
        authorization would help, which the caller should not trigger silently.
        """
        token_data = self._load_token()
        if not token_data or not token_data.get("refresh_token"):
            return None
        refreshed = await self._refresh_token(token_data)
        return refreshed["access_token"] if refreshed else None

    # ── discovery ───────────────────────────────────────────────────────────

    @staticmethod
    def _well_known(base: str, suffix: str) -> str:
        """Build a ``/.well-known/...`` URL at the *origin* of *base*.

        The old code concatenated the suffix onto the full MCP URL, producing
        ``https://host/mcp/.well-known/oauth-authorization-server``. Metadata
        lives at the origin, so discovery simply never found anything and every
        endpoint silently fell back to a guess.
        """
        parsed = urlparse(base)
        origin = f"{parsed.scheme}://{parsed.netloc}"
        path = parsed.path.rstrip("/")
        # RFC 8414 places the issuer path *after* the well-known segment.
        return urljoin(origin, f"/.well-known/{suffix}{path}")

    async def _fetch_json(self, url: str) -> dict[str, Any] | None:
        try:
            async with aiohttp.ClientSession() as session:
                async with session.get(url, timeout=_METADATA_TIMEOUT) as resp:
                    if resp.status != 200:
                        return None
                    data = await resp.json(content_type=None)
                    return data if isinstance(data, dict) else None
        except Exception as e:
            logger.debug("Metadata fetch failed for {}: {}", url, e)
            return None

    async def discover_protected_resource(self) -> dict[str, Any]:
        """Fetch the MCP server's Protected Resource Metadata (RFC 9728).

        This is the step that tells us *which* authorization server governs this
        resource. Skipping it — as the previous implementation did — meant
        assuming the MCP server was also its own AS.
        """
        for candidate in (
            self._well_known(self._server_url, "oauth-protected-resource"),
            f"{self._server_url}/.well-known/oauth-protected-resource",
        ):
            if data := await self._fetch_json(candidate):
                return data
        return {}

    @staticmethod
    def parse_www_authenticate(header: str) -> dict[str, str]:
        """Extract parameters from a ``WWW-Authenticate: Bearer ...`` header.

        A 401 from an MCP server carries the ``resource_metadata`` URL here, which
        is the spec's intended discovery entry point.
        """
        params: dict[str, str] = {}
        for match in re.finditer(r'([a-zA-Z_]+)\s*=\s*"([^"]*)"', header or ""):
            params[match.group(1).lower()] = match.group(2)
        return params

    async def _resolve_auth_server(self) -> tuple[str, dict[str, Any]]:
        """Return ``(issuer, as_metadata)`` for this MCP server.

        Order: Protected Resource Metadata names the authorization server; we
        then read that AS's own metadata. Falling back to the MCP server's own
        origin keeps servers that are their own AS working.
        """
        resource_meta = await self.discover_protected_resource()
        issuers = resource_meta.get("authorization_servers") or []
        issuer = issuers[0] if isinstance(issuers, list) and issuers else self._server_url
        if not isinstance(issuer, str):
            issuer = self._server_url
        require_secure_endpoint(issuer, "authorization server issuer")

        for candidate in (
            self._well_known(issuer, "oauth-authorization-server"),
            self._well_known(issuer, "openid-configuration"),
            f"{issuer.rstrip('/')}/.well-known/openid-configuration",
        ):
            if metadata := await self._fetch_json(candidate):
                return issuer, metadata

        logger.debug(
            "No authorization server metadata for '{}' — falling back to conventional paths",
            self._server_name,
        )
        return issuer, {}

    def _endpoint_from(
        self, metadata: dict[str, Any], key: str, issuer: str, default_path: str,
    ) -> str:
        """Pick an endpoint out of AS metadata, validating it before returning.

        The same-origin check against the issuer is the fix for the most serious
        problem in this file: ``token_endpoint`` was taken from the metadata
        document and used unconditionally, so a metadata response pointing it at
        another origin caused the authorization code *and* the PKCE verifier to be
        POSTed to that origin. An AS may legitimately host endpoints elsewhere,
        but honouring that requires trust we cannot establish here, so a
        cross-origin endpoint is refused rather than followed.
        """
        raw = metadata.get(key)
        if not isinstance(raw, str) or not raw:
            return f"{issuer.rstrip('/')}{default_path}"

        require_secure_endpoint(raw, key)
        if not same_origin(raw, issuer):
            raise MCPOAuthError(
                f"Authorization server metadata for '{self._server_name}' declares a {key} at "
                f"{raw!r}, which is not the issuer's origin ({issuer!r}). Refusing to send "
                "credentials to a different origin than the one that published the metadata."
            )
        return raw

    # ── authorization code + PKCE ───────────────────────────────────────────

    async def _authorize(self) -> dict[str, Any]:
        issuer, metadata = await self._resolve_auth_server()
        auth_endpoint = self._endpoint_from(metadata, "authorization_endpoint", issuer, "/authorize")
        token_endpoint = self._endpoint_from(metadata, "token_endpoint", issuer, "/token")
        registration_endpoint = metadata.get("registration_endpoint")

        # The redirect port is chosen *before* registration so the URI we
        # register is the URI we actually use. Registering
        # "http://localhost/callback" and then redirecting to
        # "http://localhost:54321/callback" is an exact-match failure at any
        # strict AS, and the resulting error is opaque.
        #
        # The socket is bound here and held until the handler is ready, so no
        # other process can claim the port in between — the previous code probed
        # for a free port, closed it, and bound again later, leaving exactly that
        # window open.
        sock = self._bind_callback_socket()
        redirect_port = sock.getsockname()[1]
        redirect_uri = f"http://127.0.0.1:{redirect_port}/callback"
        server: asyncio.AbstractServer | None = None

        try:
            # Bound load: a persisted secret is only reused when it was issued by
            # this same issuer/token endpoint for this same resource. Otherwise it
            # is discarded here and DCR runs again below.
            client_id, client_secret = self._load_client_credentials(
                issuer=issuer, token_endpoint=token_endpoint,
            )
            if not client_id and isinstance(registration_endpoint, str) and registration_endpoint:
                require_secure_endpoint(registration_endpoint, "registration_endpoint")
                if not same_origin(registration_endpoint, issuer):
                    raise MCPOAuthError(
                        f"registration_endpoint {registration_endpoint!r} is not on the issuer's "
                        f"origin ({issuer!r})"
                    )
                client_id, client_secret = await self._register_client(
                    registration_endpoint, redirect_uri, issuer, token_endpoint,
                )
            if not client_id:
                # No DCR available: fall back to the server name as a
                # pre-registered public client id.
                client_id = self._server_name

            code_verifier = secrets.token_urlsafe(64)
            code_challenge = (
                base64.urlsafe_b64encode(hashlib.sha256(code_verifier.encode()).digest())
                .rstrip(b"=")
                .decode()
            )
            state = secrets.token_urlsafe(32)

            # urlencode, not string concatenation: an endpoint carrying its own
            # query string, or any value needing escaping, silently produced a
            # malformed authorization URL before.
            query = urlencode({
                "response_type": "code",
                "client_id": client_id,
                "redirect_uri": redirect_uri,
                "state": state,
                "code_challenge": code_challenge,
                "code_challenge_method": "S256",
                # RFC 8707. MCP requires it so the AS can issue a token audienced
                # to this specific MCP server rather than a bearer token that
                # works anywhere.
                "resource": self._server_url,
            })
            separator = "&" if urlparse(auth_endpoint).query else "?"
            auth_url = f"{auth_endpoint}{separator}{query}"

            code_future: asyncio.Future[str] = asyncio.get_running_loop().create_future()
            server = await asyncio.start_server(
                self._make_callback_handler(state, code_future), sock=sock,
            )
            sock = None  # ownership transferred to the server

            logger.info(
                "Opening browser to authorize MCP server '{}' — complete the sign-in to continue",
                self._server_name,
            )
            if not webbrowser.open(auth_url):
                logger.warning(
                    "Could not open a browser automatically. Visit this URL to authorize "
                    "'{}':\n{}", self._server_name, auth_url,
                )

            try:
                code = await asyncio.wait_for(code_future, timeout=300)
            except asyncio.TimeoutError:
                raise MCPOAuthError(
                    f"OAuth authorization for '{self._server_name}' timed out after 5 minutes"
                ) from None
        finally:
            if server is not None:
                server.close()
                await server.wait_closed()
            elif sock is not None:
                # Failed before the server took ownership.
                sock.close()

        token_data = await self._exchange_code(
            token_endpoint, code, client_id, client_secret, code_verifier, redirect_uri,
        )
        token_data["issuer"] = issuer
        token_data["token_endpoint"] = token_endpoint
        self._save_token(token_data)
        return token_data

    @staticmethod
    def _bind_callback_socket() -> Any:
        """Bind a loopback socket on an ephemeral port and return it listening."""
        import socket

        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            sock.bind(("127.0.0.1", 0))
            sock.listen(8)
            sock.setblocking(False)
        except Exception:
            sock.close()
            raise
        return sock

    def _make_callback_handler(
        self, expected_state: str, future: asyncio.Future[str],
    ) -> Any:
        """Build the loopback HTTP handler that receives the authorization code."""

        async def handle_client(
            reader: asyncio.StreamReader, writer: asyncio.StreamWriter,
        ) -> None:
            try:
                # Read until the end of the request headers rather than a single
                # 4096-byte read: a long header block (or a client that writes
                # slowly) previously produced a truncated request line and an
                # unparseable callback.
                try:
                    header_bytes = await asyncio.wait_for(
                        reader.readuntil(b"\r\n\r\n"), timeout=10,
                    )
                except asyncio.IncompleteReadError as e:
                    header_bytes = e.partial
                except (asyncio.LimitOverrunError, asyncio.TimeoutError):
                    header_bytes = b""

                request_line = header_bytes.decode(errors="replace").split("\r\n")[0]
                parts = request_line.split(" ")
                path = parts[1] if len(parts) > 1 else ""

                from urllib.parse import parse_qs, urlparse as _urlparse
                params = parse_qs(_urlparse(path).query)
                state = params.get("state", [""])[0]
                code = params.get("code", [""])[0]
                error = params.get("error", [""])[0]

                if error:
                    detail = params.get("error_description", [""])[0] or error
                    body = f"Authorization failed: {detail}"
                    self._fail_future(future, MCPOAuthError(
                        f"Authorization server returned an error: {detail}"
                    ))
                elif not secrets.compare_digest(state, expected_state):
                    # Resolve the future instead of only changing the response
                    # text. Leaving it pending meant the flow waited the full
                    # 300s and then reported a timeout, hiding the real cause.
                    body = "State mismatch — authorization failed."
                    self._fail_future(future, MCPOAuthError(
                        "OAuth state parameter did not match; the callback may have been forged"
                    ))
                elif not code:
                    body = "No authorization code in callback."
                    self._fail_future(future, MCPOAuthError(
                        "Authorization callback carried no code"
                    ))
                else:
                    body = "Authorization complete. You can close this tab."
                    if not future.done():
                        future.set_result(code)

                payload = body.encode()
                writer.write(
                    b"HTTP/1.1 200 OK\r\n"
                    b"Content-Type: text/html; charset=utf-8\r\n"
                    b"Cache-Control: no-store\r\n"
                    b"Content-Length: " + str(len(payload)).encode() + b"\r\n"
                    b"Connection: close\r\n\r\n" + payload
                )
                await writer.drain()
            except Exception as e:
                logger.debug("OAuth callback handling failed: {}", e)
            finally:
                writer.close()

        return handle_client

    @staticmethod
    def _fail_future(future: asyncio.Future[str], error: Exception) -> None:
        if not future.done():
            future.set_exception(error)

    # ── dynamic client registration ─────────────────────────────────────────

    async def _register_client(
        self, endpoint: str, redirect_uri: str, issuer: str, token_endpoint: str,
    ) -> tuple[str, str]:
        """Register this client with the AS and persist the result.

        Persistence matters: without it every process start re-registered, which
        accumulates dead client records at the AS and loses any client secret it
        issued — so the next refresh had no way to authenticate.
        """
        body = {
            "client_name": "Echo Agent",
            # The exact URI that will be used, including the port.
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }
        try:
            async with aiohttp.ClientSession() as session:
                # allow_redirects=False: this POST carries the redirect_uri we
                # will accept a code on, and its response carries the client
                # secret. aiohttp follows redirects by default and re-sends the
                # body on a 307/308, so a hostile or compromised registration
                # endpoint could bounce the whole exchange to another origin. The
                # same-origin checks elsewhere validate the *initial* endpoint
                # only — they say nothing about a redirect target.
                async with session.post(
                    endpoint, json=body, timeout=_METADATA_TIMEOUT, allow_redirects=False,
                ) as resp:
                    if resp.status not in (200, 201):
                        text = (await resp.text())[:300]
                        logger.warning(
                            "Dynamic client registration for '{}' failed ({}): {}",
                            self._server_name, resp.status, text,
                        )
                        return "", ""
                    data = await resp.json(content_type=None)
        except Exception as e:
            logger.warning("Dynamic client registration for '{}' failed: {}", self._server_name, e)
            return "", ""

        if not isinstance(data, dict):
            return "", ""
        client_id = str(data.get("client_id", "") or "")
        client_secret = str(data.get("client_secret", "") or "")
        if client_id:
            self._save_client_credentials({
                "client_id": client_id,
                "client_secret": client_secret,
                "issuer": issuer,
                "token_endpoint": token_endpoint,
                # The resource this registration was scoped to. Recorded so a
                # later config edit that repoints the server name cannot silently
                # reuse this secret against a different address.
                "server_url": self._server_url,
                "redirect_uri": redirect_uri,
                "registered_at": time.time(),
            })
        return client_id, client_secret

    def _load_client_credentials(
        self, *, issuer: str = "", token_endpoint: str = "",
    ) -> tuple[str, str]:
        """Load the persisted DCR result, bound to where it was issued.

        The record is keyed only by server *name*, which is a config key the
        operator can repoint at a different address. Loading it unconditionally
        meant a secret issued by one authorization server could be sent to
        whoever the config named next — a credential leak triggered by an
        ordinary config edit.

        When *issuer* / *token_endpoint* are supplied they must match what was
        recorded at registration; a mismatch discards the record so the caller
        re-registers. Callers that only need the identifier for an already
        origin-validated request (the refresh path) pass neither and get the
        record as-is.
        """
        data = self._read_json(self._client_file)
        if not data:
            return "", ""

        stored_issuer = str(data.get("issuer", "") or "")
        stored_endpoint = str(data.get("token_endpoint", "") or "")
        stored_server = str(data.get("server_url", "") or "")

        # Records written before this binding existed carry no server_url. They
        # are migrated rather than trusted: without knowing which resource the
        # secret was scoped to, reuse cannot be shown to be safe.
        if issuer or token_endpoint:
            if not stored_issuer or not stored_endpoint or not stored_server:
                logger.info(
                    "Discarding pre-binding client registration for '{}' — it does not "
                    "record which authorization server issued it; re-registering",
                    self._server_name,
                )
                self._discard_client_credentials()
                return "", ""

            mismatches = []
            if issuer and not same_origin(stored_issuer, issuer):
                mismatches.append(f"issuer {stored_issuer!r} != {issuer!r}")
            if token_endpoint and not same_origin(stored_endpoint, token_endpoint):
                mismatches.append(f"token_endpoint {stored_endpoint!r} != {token_endpoint!r}")
            if stored_server != self._server_url:
                mismatches.append(f"server_url {stored_server!r} != {self._server_url!r}")
            if mismatches:
                logger.warning(
                    "MCP server '{}' now points somewhere else than when its client "
                    "credentials were issued ({}). Discarding the old secret and "
                    "re-registering rather than sending it to a different origin.",
                    self._server_name, "; ".join(mismatches),
                )
                self._discard_client_credentials()
                return "", ""

        return str(data.get("client_id", "") or ""), str(data.get("client_secret", "") or "")

    def _discard_client_credentials(self) -> None:
        try:
            self._client_file.unlink(missing_ok=True)
        except OSError as e:
            logger.warning(
                "Could not remove stale client credentials for '{}': {}",
                self._server_name, e,
            )

    def _save_client_credentials(self, data: dict[str, Any]) -> None:
        self._write_secret_json(self._client_file, data)

    # ── token endpoints ─────────────────────────────────────────────────────

    async def _exchange_code(
        self, token_endpoint: str, code: str, client_id: str, client_secret: str,
        code_verifier: str, redirect_uri: str,
    ) -> dict[str, Any]:
        body = {
            "grant_type": "authorization_code",
            "code": code,
            "client_id": client_id,
            "code_verifier": code_verifier,
            "redirect_uri": redirect_uri,
            # Required by MCP so the issued token is audienced to this server.
            "resource": self._server_url,
        }
        if client_secret:
            body["client_secret"] = client_secret

        async with aiohttp.ClientSession() as session:
            # allow_redirects=False: the body holds the authorization code, the
            # PKCE verifier and possibly the client secret. A cross-origin 307/308
            # would have aiohttp re-send all of it to whatever origin the token
            # endpoint names — handing the credentials to a second server that
            # the same-origin check on `token_endpoint` never saw.
            async with session.post(
                token_endpoint, data=body, timeout=_TOKEN_TIMEOUT, allow_redirects=False,
            ) as resp:
                if resp.status != 200:
                    text = (await resp.text())[:300]
                    raise MCPOAuthError(f"Token exchange failed ({resp.status}): {text}")
                data = await resp.json(content_type=None)

        if not isinstance(data, dict) or not data.get("access_token"):
            raise MCPOAuthError("Token endpoint returned no access_token")
        data["obtained_at"] = time.time()
        return data

    async def _refresh_token(self, token_data: dict[str, Any]) -> dict[str, Any] | None:
        """Exchange a refresh token for a new access token.

        Reuses the issuer and token endpoint recorded when the token was first
        obtained, so a refresh cannot be steered elsewhere by a metadata document
        served later; both are re-validated regardless.
        """
        issuer = token_data.get("issuer")
        token_endpoint = token_data.get("token_endpoint")
        if not isinstance(token_endpoint, str) or not token_endpoint:
            issuer, metadata = await self._resolve_auth_server()
            token_endpoint = self._endpoint_from(metadata, "token_endpoint", issuer, "/token")
        else:
            require_secure_endpoint(token_endpoint, "token_endpoint")
            if isinstance(issuer, str) and issuer and not same_origin(token_endpoint, issuer):
                logger.warning(
                    "Stored token_endpoint for '{}' is not on the issuer origin — refusing refresh",
                    self._server_name,
                )
                return None

        # Bound here too: this POST carries the client secret, so it must not be
        # sent to a token endpoint other than the one that issued it. On a
        # mismatch the secret is discarded and the refresh proceeds as a public
        # client — which the AS will reject, forcing clean re-authorization
        # instead of leaking the old secret to a new origin.
        client_id, client_secret = self._load_client_credentials(
            issuer=issuer if isinstance(issuer, str) else "",
            token_endpoint=token_endpoint,
        )
        body = {
            "grant_type": "refresh_token",
            "refresh_token": token_data["refresh_token"],
            # Both were missing before. Most authorization servers reject a
            # public-client refresh that does not identify the client, and MCP
            # requires the resource on every token request.
            "client_id": client_id or self._server_name,
            "resource": self._server_url,
        }
        if client_secret:
            body["client_secret"] = client_secret

        try:
            async with aiohttp.ClientSession() as session:
                # allow_redirects=False — same reason as the code exchange. A
                # refresh token is longer-lived than an access token, so leaking
                # it via a cross-origin redirect is the worse of the two.
                async with session.post(
                    token_endpoint, data=body, timeout=_TOKEN_TIMEOUT, allow_redirects=False,
                ) as resp:
                    if resp.status != 200:
                        text = (await resp.text())[:200]
                        logger.warning(
                            "Token refresh for '{}' failed ({}): {}",
                            self._server_name, resp.status, text,
                        )
                        return None
                    new_data = await resp.json(content_type=None)
        except Exception as e:
            logger.warning("Token refresh failed for '{}': {}", self._server_name, e)
            return None

        if not isinstance(new_data, dict) or not new_data.get("access_token"):
            return None
        new_data["obtained_at"] = time.time()
        # A rotating AS returns a new refresh token; one that does not expects us
        # to keep using the old one.
        new_data.setdefault("refresh_token", token_data.get("refresh_token"))
        new_data.setdefault("issuer", issuer)
        new_data.setdefault("token_endpoint", token_endpoint)
        self._save_token(new_data)
        return new_data

    # ── persistence ─────────────────────────────────────────────────────────

    def _is_expired(self, token_data: dict[str, Any]) -> bool:
        obtained = token_data.get("obtained_at", 0)
        expires_in = token_data.get("expires_in", 3600)
        try:
            obtained = float(obtained)
            expires_in = float(expires_in)
        except (TypeError, ValueError):
            return True
        return time.time() > obtained + expires_in - _EXPIRY_MARGIN_SECONDS

    def _read_json(self, path: Path) -> dict[str, Any] | None:
        if not path.exists():
            return None
        try:
            data = json.loads(path.read_text())
            return data if isinstance(data, dict) else None
        except (json.JSONDecodeError, OSError):
            return None

    def _load_token(self) -> dict[str, Any] | None:
        return self._read_json(self._token_file)

    def _save_token(self, data: dict[str, Any]) -> None:
        self._write_secret_json(self._token_file, data)

    def _write_secret_json(self, path: Path, data: dict[str, Any]) -> None:
        """Write JSON to *path* atomically, readable only by this user.

        ``write_text`` left the file at the process umask — 0644 in practice, so
        every local user could read the access and refresh tokens. Creating the
        temporary file with 0600 via ``os.open`` means there is never a moment
        where the credential exists with wider permissions, and ``os.replace``
        keeps a crash from leaving a half-written token behind.
        """
        path.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp_name = tempfile.mkstemp(dir=str(path.parent), prefix=f".{path.name}.", suffix=".tmp")
        try:
            os.fchmod(fd, 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as fh:
                json.dump(data, fh, indent=2)
                fh.flush()
                os.fsync(fh.fileno())
            os.replace(tmp_name, path)
        except Exception:
            try:
                os.unlink(tmp_name)
            except OSError:
                # The credential write still raises its original failure; missing
                # or undeletable temporary cleanup must not mask that cause.
                pass
            raise
