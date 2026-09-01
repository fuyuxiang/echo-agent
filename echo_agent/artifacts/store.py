"""Durable, session-isolated artifact store with idempotent chunk appends."""

from __future__ import annotations

import asyncio
import hashlib
import json
import os
import re
import uuid
import weakref
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from echo_agent.artifacts.validation import validate_text

_ID_RE = re.compile(r"^[a-f0-9]{32}$")
_SAFE_NAME_RE = re.compile(r"[^A-Za-z0-9._\-\u3400-\u4dbf\u4e00-\u9fff]+")


class ArtifactError(ValueError):
    """A validation/business failure that must not trip infrastructure breakers."""


def _utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


class ArtifactStore:
    """Owns artifact paths so model calls never receive filesystem authority."""

    def __init__(
        self,
        workspace: Path,
        root_dir: str,
        *,
        max_chunk_chars: int,
        max_artifact_mb: int,
        allowed_extensions: list[str],
    ) -> None:
        workspace_root = workspace.resolve()
        root = (workspace_root / root_dir).resolve()
        try:
            root.relative_to(workspace_root)
        except ValueError as exc:
            raise ValueError("artifact root must stay inside the workspace") from exc
        # Discovery must be side-effect free: merely exposing the tools should
        # not create data directories (and read-only deployments must still be
        # able to start). The first artifact_create performs the mkdir.
        self.workspace_root = workspace_root
        self.root = root
        self.max_chunk_chars = max_chunk_chars
        self.max_bytes = max_artifact_mb * 1024 * 1024
        self.allowed_extensions = frozenset(ext.lower() for ext in allowed_extensions)
        self._locks: weakref.WeakValueDictionary[tuple[str, str], asyncio.Lock] = (
            weakref.WeakValueDictionary()
        )

    @staticmethod
    def session_hash(session_key: str) -> str:
        if not session_key:
            raise ArtifactError("artifact operations require a session context")
        return hashlib.sha256(session_key.encode("utf-8")).hexdigest()[:32]

    def _session_dir(self, session_key: str, *, create: bool = False) -> Path:
        path = self.root / self.session_hash(session_key)
        if create:
            self.root.mkdir(parents=True, exist_ok=True, mode=0o700)
            resolved_root = self.root.resolve()
            try:
                resolved_root.relative_to(self.workspace_root)
            except ValueError as exc:
                raise ArtifactError("artifact root escaped the workspace") from exc
            if resolved_root != self.root:
                raise ArtifactError("artifact root changed through a symbolic link")
            # The session component is deterministic, so a local process can
            # pre-create it before the first artifact.  Never follow that entry:
            # otherwise ``mkdir(<session>/<uuid>)`` would create files at the
            # symlink target, outside the configured artifact store.
            if path.is_symlink():
                raise ArtifactError("artifact session directory cannot be a symbolic link")
            path.mkdir(parents=True, exist_ok=True, mode=0o700)
            try:
                resolved_session = path.resolve(strict=True)
                resolved_session.relative_to(resolved_root)
            except (OSError, ValueError) as exc:
                raise ArtifactError("artifact session directory escaped its store") from exc
            if resolved_session != path or not path.is_dir():
                raise ArtifactError("artifact session directory changed through a symbolic link")
        return path

    def _artifact_dir(self, session_key: str, artifact_id: str) -> Path:
        if not _ID_RE.fullmatch(artifact_id or ""):
            raise ArtifactError("invalid artifact_id")
        path = self._session_dir(session_key) / artifact_id
        resolved = path.resolve(strict=False)
        try:
            resolved.relative_to(self.root)
        except ValueError as exc:
            raise ArtifactError("artifact path escaped its store") from exc
        return path

    def _lock(self, session_key: str, artifact_id: str) -> asyncio.Lock:
        key = (self.session_hash(session_key), artifact_id)
        return self._locks.setdefault(key, asyncio.Lock())

    @staticmethod
    def _sanitize_filename(filename: str, allowed_extensions: frozenset[str]) -> tuple[str, str]:
        leaf = Path(filename or "report.md").name.strip()
        leaf = _SAFE_NAME_RE.sub("_", leaf).strip(" ._")
        if not leaf:
            leaf = "report.md"
        ext = Path(leaf).suffix.lower()
        if ext not in allowed_extensions:
            allowed = ", ".join(sorted(allowed_extensions))
            raise ArtifactError(f"unsupported artifact extension '{ext or '<none>'}'; allowed: {allowed}")
        stem = Path(leaf).stem[:96].strip(" ._") or "report"
        return f"{stem}{ext}", ext

    @staticmethod
    def _atomic_json(path: Path, data: dict[str, Any]) -> None:
        tmp = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
        payload = json.dumps(data, ensure_ascii=False, indent=2).encode("utf-8")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        try:
            with os.fdopen(fd, "wb") as handle:
                handle.write(payload)
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(tmp, path)
        finally:
            if tmp.exists():
                tmp.unlink()

    def _load(self, session_key: str, artifact_id: str) -> tuple[Path, dict[str, Any]]:
        directory = self._artifact_dir(session_key, artifact_id)
        manifest_path = directory / "manifest.json"
        if not directory.is_dir() or not manifest_path.is_file() or manifest_path.is_symlink():
            raise ArtifactError("artifact not found in this session")
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            raise ArtifactError("artifact manifest is unreadable") from exc
        if (
            manifest.get("artifact_id") != artifact_id
            or manifest.get("session_hash") != self.session_hash(session_key)
        ):
            raise ArtifactError("artifact ownership check failed")
        return directory, manifest

    async def create(self, session_key: str, *, filename: str, title: str = "") -> dict[str, Any]:
        safe_name, ext = self._sanitize_filename(filename, self.allowed_extensions)
        session_dir = self._session_dir(session_key, create=True)
        for _ in range(8):
            artifact_id = uuid.uuid4().hex
            directory = session_dir / artifact_id
            try:
                directory.mkdir(mode=0o700)
                break
            except FileExistsError:
                continue
        else:  # pragma: no cover - cryptographically implausible
            raise RuntimeError("could not allocate artifact id")
        (directory / "chunks").mkdir(mode=0o700)
        now = _utc_now()
        manifest = {
            "version": 1,
            "artifact_id": artifact_id,
            "session_hash": self.session_hash(session_key),
            "filename": safe_name,
            "extension": ext,
            "title": str(title)[:200],
            "state": "draft",
            "next_sequence": 0,
            "total_bytes": 0,
            "total_characters": 0,
            "chunks": [],
            "created_at": now,
            "updated_at": now,
            "validation": None,
            "deliveries": [],
        }
        try:
            self._atomic_json(directory / "manifest.json", manifest)
        except Exception:
            # No opaque id has been returned yet, so a failed create would leave
            # an unreachable directory that no manifest-aware sweeper may touch.
            (directory / "chunks").rmdir()
            directory.rmdir()
            raise
        return self.public_manifest(manifest)

    async def append(
        self,
        session_key: str,
        artifact_id: str,
        *,
        sequence: int,
        content: str,
        expected_bytes: int | None = None,
    ) -> dict[str, Any]:
        if sequence < 0:
            raise ArtifactError("sequence must be non-negative")
        if len(content) > self.max_chunk_chars:
            raise ArtifactError(
                f"chunk has {len(content)} characters; maximum is {self.max_chunk_chars}; split it"
            )
        payload = content.encode("utf-8")
        digest = hashlib.sha256(payload).hexdigest()
        async with self._lock(session_key, artifact_id):
            directory, manifest = self._load(session_key, artifact_id)
            if manifest.get("state") != "draft":
                raise ArtifactError("artifact is finalized and cannot be changed")
            next_sequence = int(manifest.get("next_sequence", 0))
            chunks = manifest.get("chunks") or []
            if sequence < next_sequence:
                prior = next((item for item in chunks if int(item.get("sequence", -1)) == sequence), None)
                if prior and prior.get("sha256") == digest:
                    result = self.public_manifest(manifest)
                    result["idempotent_replay"] = True
                    return result
                raise ArtifactError("sequence was already written with different content")
            if sequence > next_sequence:
                raise ArtifactError(f"out-of-order chunk: expected sequence {next_sequence}")
            total_bytes = int(manifest.get("total_bytes", 0))
            if expected_bytes is not None and expected_bytes != total_bytes:
                raise ArtifactError(f"offset mismatch: expected_bytes={expected_bytes}, actual={total_bytes}")
            if total_bytes + len(payload) > self.max_bytes:
                raise ArtifactError("artifact size quota exceeded")

            chunk_path = directory / "chunks" / f"{sequence:08d}.part"
            if chunk_path.exists():
                existing = chunk_path.read_bytes()
                if hashlib.sha256(existing).hexdigest() != digest:
                    raise ArtifactError("chunk journal conflict")
            else:
                fd = os.open(chunk_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                with os.fdopen(fd, "wb") as handle:
                    handle.write(payload)
                    handle.flush()
                    os.fsync(handle.fileno())

            chunks.append({
                "sequence": sequence,
                "bytes": len(payload),
                "characters": len(content),
                "sha256": digest,
            })
            manifest.update({
                "next_sequence": sequence + 1,
                "total_bytes": total_bytes + len(payload),
                "total_characters": int(manifest.get("total_characters", 0)) + len(content),
                "chunks": chunks,
                "updated_at": _utc_now(),
            })
            self._atomic_json(directory / "manifest.json", manifest)
            return self.public_manifest(manifest)

    def _draft_text(self, directory: Path, manifest: dict[str, Any]) -> str:
        pieces: list[str] = []
        chunks = manifest.get("chunks") or []
        if len(chunks) != int(manifest.get("next_sequence", 0)):
            raise ArtifactError("artifact chunk journal is incomplete")
        actual_bytes = 0
        actual_characters = 0
        for expected, item in enumerate(chunks):
            if int(item.get("sequence", -1)) != expected:
                raise ArtifactError("artifact chunk sequence is incomplete")
            chunk_path = directory / "chunks" / f"{expected:08d}.part"
            if not chunk_path.is_file() or chunk_path.is_symlink():
                raise ArtifactError(f"artifact chunk {expected} is missing")
            payload = chunk_path.read_bytes()
            if hashlib.sha256(payload).hexdigest() != item.get("sha256"):
                raise ArtifactError(f"artifact chunk {expected} failed checksum verification")
            try:
                text = payload.decode("utf-8")
            except UnicodeDecodeError as exc:
                raise ArtifactError(f"artifact chunk {expected} is not UTF-8") from exc
            if len(payload) != int(item.get("bytes", -1)) or len(text) != int(
                item.get("characters", -1)
            ):
                raise ArtifactError(f"artifact chunk {expected} metadata is inconsistent")
            actual_bytes += len(payload)
            actual_characters += len(text)
            pieces.append(text)
        if (
            actual_bytes != int(manifest.get("total_bytes", -1))
            or actual_characters != int(manifest.get("total_characters", -1))
        ):
            raise ArtifactError("artifact totals are inconsistent")
        return "".join(pieces)

    async def validate(self, session_key: str, artifact_id: str) -> dict[str, Any]:
        async with self._lock(session_key, artifact_id):
            directory, manifest = self._load(session_key, artifact_id)
            if manifest.get("state") == "finalized":
                path = directory / manifest["filename"]
                if not path.is_file() or path.is_symlink():
                    raise ArtifactError("finalized artifact file is missing")
                content = path.read_text(encoding="utf-8")
            else:
                content = self._draft_text(directory, manifest)
            return validate_text(content, str(manifest.get("extension", "")))

    async def finalize(self, session_key: str, artifact_id: str) -> dict[str, Any]:
        async with self._lock(session_key, artifact_id):
            directory, manifest = self._load(session_key, artifact_id)
            if manifest.get("state") == "finalized":
                result = self.public_manifest(manifest)
                result["idempotent_replay"] = True
                return result
            content = self._draft_text(directory, manifest)
            validation = validate_text(content, str(manifest.get("extension", "")))
            if not validation["valid"]:
                raise ArtifactError("artifact validation failed: " + json.dumps(validation["errors"], ensure_ascii=False))
            destination = directory / manifest["filename"]
            tmp = directory / f".{manifest['filename']}.{uuid.uuid4().hex}.tmp"
            payload = content.encode("utf-8")
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            try:
                with os.fdopen(fd, "wb") as handle:
                    handle.write(payload)
                    handle.flush()
                    os.fsync(handle.fileno())
                os.replace(tmp, destination)
            finally:
                if tmp.exists():
                    tmp.unlink()
            manifest.update({
                "state": "finalized",
                "sha256": hashlib.sha256(payload).hexdigest(),
                "validation": validation,
                "finalized_at": _utc_now(),
                "updated_at": _utc_now(),
            })
            self._atomic_json(directory / "manifest.json", manifest)
            return self.public_manifest(manifest)

    async def finalized_path(self, session_key: str, artifact_id: str) -> tuple[Path, dict[str, Any]]:
        async with self._lock(session_key, artifact_id):
            directory, manifest = self._load(session_key, artifact_id)
            if manifest.get("state") != "finalized":
                raise ArtifactError("artifact must be finalized before delivery")
            path = directory / manifest["filename"]
            if not path.is_file() or path.is_symlink():
                raise ArtifactError("finalized artifact file is missing")
            return path, manifest

    async def record_delivery(
        self,
        session_key: str,
        artifact_id: str,
        *,
        channel: str,
        chat_id: str,
        delivery_id: str = "",
        mode: str = "attachment",
        completed_parts: int = 1,
        total_parts: int = 1,
    ) -> None:
        """Durably checkpoint delivery progress for safe retries.

        Sending and recording cannot be one atomic transaction with an external
        channel.  Persisting every acknowledged text part is nevertheless enough
        to prevent a normal retry after part N fails from replaying parts 1..N-1.
        ``delivery_id`` is an opaque digest produced by the delivery tool; raw
        chat identifiers and local paths are never written to the manifest.
        """
        if completed_parts < 0 or total_parts < 1 or completed_parts > total_parts:
            raise ArtifactError("invalid artifact delivery progress")
        async with self._lock(session_key, artifact_id):
            directory, manifest = self._load(session_key, artifact_id)
            if manifest.get("state") != "finalized":
                raise ArtifactError("artifact must be finalized before delivery")
            deliveries = list(manifest.get("deliveries") or [])
            now = _utc_now()
            entry = {
                "delivery_id": delivery_id,
                "channel": channel,
                "chat_id_hash": hashlib.sha256(chat_id.encode()).hexdigest()[:16],
                "mode": mode,
                "completed_parts": completed_parts,
                "total_parts": total_parts,
                "updated_at": now,
            }
            if completed_parts == total_parts:
                entry["delivered_at"] = now
            prior_index = next(
                (
                    index for index, item in enumerate(deliveries)
                    if delivery_id and item.get("delivery_id") == delivery_id
                ),
                None,
            )
            if prior_index is None:
                deliveries.append(entry)
            else:
                prior = deliveries[prior_index]
                if (
                    prior.get("mode") != mode
                    or int(prior.get("total_parts", -1)) != total_parts
                    or int(prior.get("completed_parts", 0)) > completed_parts
                ):
                    raise ArtifactError("artifact delivery checkpoint conflict")
                deliveries[prior_index] = entry
            manifest["deliveries"] = deliveries[-20:]
            manifest["updated_at"] = now
            self._atomic_json(directory / "manifest.json", manifest)

    async def delivery_progress(
        self,
        session_key: str,
        artifact_id: str,
        *,
        delivery_id: str,
    ) -> dict[str, Any] | None:
        """Return a copy of an opaque delivery checkpoint, if one exists."""
        if not delivery_id:
            return None
        async with self._lock(session_key, artifact_id):
            _directory, manifest = self._load(session_key, artifact_id)
            if manifest.get("state") != "finalized":
                raise ArtifactError("artifact must be finalized before delivery")
            for item in manifest.get("deliveries") or []:
                if item.get("delivery_id") == delivery_id:
                    return dict(item)
        return None

    @staticmethod
    def public_manifest(manifest: dict[str, Any]) -> dict[str, Any]:
        return {
            key: manifest.get(key)
            for key in (
                "artifact_id", "filename", "title", "state", "next_sequence",
                "total_bytes", "total_characters", "sha256", "created_at",
                "updated_at", "finalized_at", "validation",
            )
            if manifest.get(key) is not None
        }
