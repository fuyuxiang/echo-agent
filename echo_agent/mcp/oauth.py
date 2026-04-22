"""MCP OAuth 2.1 PKCE client — browser-based authorization for HTTP MCP servers."""

from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import os
import secrets
import time
import webbrowser
from pathlib import Path
from typing import Any

import aiohttp
from loguru import logger


class MCPOAuthClient:

    def __init__(self, server_name: str, server_url: str, token_dir: Path):
        self._server_name = server_name
        self._server_url = server_url.rstrip("/")
        self._token_dir = token_dir
        self._token_dir.mkdir(parents=True, exist_ok=True)
        self._token_file = self._token_dir / f"{server_name}.json"

    def get_access_token(self) -> str | None:
        token_data = self._load_token()
        if not token_data:
            return None
        if self._is_expired(token_data):
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

    async def _authorize(self) -> dict[str, Any]:
        metadata = await self._fetch_server_metadata()
        auth_endpoint = metadata.get("authorization_endpoint", f"{self._server_url}/authorize")
        token_endpoint = metadata.get("token_endpoint", f"{self._server_url}/token")
        registration_endpoint = metadata.get("registration_endpoint")

        client_id = self._server_name
        if registration_endpoint:
            client_id = await self._register_client(registration_endpoint)

        code_verifier = secrets.token_urlsafe(64)
        code_challenge = base64.urlsafe_b64encode(
            hashlib.sha256(code_verifier.encode()).digest()
        ).rstrip(b"=").decode()

        state = secrets.token_urlsafe(32)
        redirect_port = self._find_free_port()
        redirect_uri = f"http://localhost:{redirect_port}/callback"

        auth_url = (
            f"{auth_endpoint}?response_type=code&client_id={client_id}"
            f"&redirect_uri={redirect_uri}&state={state}"
            f"&code_challenge={code_challenge}&code_challenge_method=S256"
        )

        code_future: asyncio.Future[str] = asyncio.get_event_loop().create_future()
        server = await self._start_callback_server(redirect_port, state, code_future)

        logger.info("Opening browser for MCP OAuth: {}", self._server_name)
        webbrowser.open(auth_url)

        try:
            code = await asyncio.wait_for(code_future, timeout=300)
        except asyncio.TimeoutError:
            raise RuntimeError("OAuth authorization timed out (5 minutes)")
        finally:
            server.close()
            await server.wait_closed()

        token_data = await self._exchange_code(token_endpoint, code, client_id, code_verifier, redirect_uri)
        self._save_token(token_data)
        return token_data

    async def _fetch_server_metadata(self) -> dict[str, Any]:
        url = f"{self._server_url}/.well-known/oauth-authorization-server"
        try:
            async with aiohttp.ClientSession() as session:
                async with session.get(url, timeout=aiohttp.ClientTimeout(total=10)) as resp:
                    if resp.status == 200:
                        return await resp.json()
        except Exception as e:
            logger.debug("Failed to discover OAuth metadata: {}", e)
        return {}

    async def _register_client(self, endpoint: str) -> str:
        body = {
            "client_name": "Echo Agent",
            "redirect_uris": ["http://localhost/callback"],
            "grant_types": ["authorization_code"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }
        async with aiohttp.ClientSession() as session:
            async with session.post(endpoint, json=body, timeout=aiohttp.ClientTimeout(total=10)) as resp:
                if resp.status in (200, 201):
                    data = await resp.json()
                    return data.get("client_id", self._server_name)
        return self._server_name

    async def _exchange_code(
        self, token_endpoint: str, code: str, client_id: str,
        code_verifier: str, redirect_uri: str,
    ) -> dict[str, Any]:
        body = {
            "grant_type": "authorization_code",
            "code": code,
            "client_id": client_id,
            "code_verifier": code_verifier,
            "redirect_uri": redirect_uri,
        }
        async with aiohttp.ClientSession() as session:
            async with session.post(token_endpoint, data=body, timeout=aiohttp.ClientTimeout(total=15)) as resp:
                if resp.status != 200:
                    text = await resp.text()
                    raise RuntimeError(f"Token exchange failed ({resp.status}): {text[:300]}")
                data = await resp.json()
                data["obtained_at"] = time.time()
                return data

    async def _refresh_token(self, token_data: dict[str, Any]) -> dict[str, Any] | None:
        metadata = await self._fetch_server_metadata()
        token_endpoint = metadata.get("token_endpoint", f"{self._server_url}/token")
        body = {
            "grant_type": "refresh_token",
            "refresh_token": token_data["refresh_token"],
        }
        try:
            async with aiohttp.ClientSession() as session:
                async with session.post(token_endpoint, data=body, timeout=aiohttp.ClientTimeout(total=10)) as resp:
                    if resp.status == 200:
                        new_data = await resp.json()
                        new_data["obtained_at"] = time.time()
                        new_data.setdefault("refresh_token", token_data.get("refresh_token"))
                        self._save_token(new_data)
                        return new_data
        except Exception as e:
            logger.warning("Token refresh failed for '{}': {}", self._server_name, e)
        return None

    async def _start_callback_server(
        self, port: int, expected_state: str, future: asyncio.Future[str],
    ) -> asyncio.AbstractServer:
        async def handle_client(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
            data = await reader.read(4096)
            request_line = data.decode(errors="replace").split("\r\n")[0]
            path = request_line.split(" ")[1] if " " in request_line else ""

            from urllib.parse import parse_qs, urlparse
            parsed = urlparse(path)
            params = parse_qs(parsed.query)

            response_body = "Authorization complete. You can close this tab."
            state = params.get("state", [""])[0]
            code = params.get("code", [""])[0]

            if state != expected_state:
                response_body = "State mismatch — authorization failed."
            elif code and not future.done():
                future.set_result(code)

            http_resp = f"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {len(response_body)}\r\n\r\n{response_body}"
            writer.write(http_resp.encode())
            await writer.drain()
            writer.close()

        server = await asyncio.start_server(handle_client, "127.0.0.1", port)
        return server

    def _find_free_port(self) -> int:
        import socket
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.bind(("127.0.0.1", 0))
            return s.getsockname()[1]

    def _is_expired(self, token_data: dict[str, Any]) -> bool:
        obtained = token_data.get("obtained_at", 0)
        expires_in = token_data.get("expires_in", 3600)
        return time.time() > obtained + expires_in - 60

    def _load_token(self) -> dict[str, Any] | None:
        if self._token_file.exists():
            try:
                return json.loads(self._token_file.read_text())
            except (json.JSONDecodeError, OSError):
                pass
        return None

    def _save_token(self, data: dict[str, Any]) -> None:
        self._token_file.write_text(json.dumps(data, indent=2))
