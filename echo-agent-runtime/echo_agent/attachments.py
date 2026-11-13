"""Durable, session-scoped originals; independent of document understanding/cache eviction."""
from __future__ import annotations

import hashlib
import json
import mimetypes
import os
import re
import shutil
import sqlite3
from contextlib import contextmanager
import tempfile
import time
import zipfile
from loguru import logger
from pathlib import Path
from typing import Any


def is_pdf_header(head: bytes) -> bool:
    """Recognize a header, never a quoted signature in prose or source code."""
    return re.match(rb"(?:\xef\xbb\xbf)?[\x00\t\r\n ]*%PDF-\d\.\d(?:[\r\n\t ]|$)", head) is not None


def document_extension(path: Path, name: str = "", mime_type: str = "") -> str:
    """Prefer verified container signatures, then the transport's original name/type."""
    with path.open("rb") as handle:
        head = handle.read(1024)
    if is_pdf_header(head):
        return ".pdf"
    if head.startswith(b"PK\x03\x04"):
        try:
            with zipfile.ZipFile(path) as archive:
                names = set(archive.namelist())
            for marker, extension in (("word/document.xml", ".docx"), ("xl/workbook.xml", ".xlsx"),
                                      ("ppt/presentation.xml", ".pptx")):
                if marker in names:
                    return extension
        except (OSError, zipfile.BadZipFile):
            # A malformed ZIP is still retainable; fall back to its declared format.
            pass
    if head.startswith(b"\x89PNG\r\n\x1a\n"):
        return ".png"
    if head.startswith(b"\xff\xd8\xff"):
        return ".jpg"
    if head.startswith((b"GIF87a", b"GIF89a")):
        return ".gif"
    suffix = Path(name.replace("\\", "/")).suffix.lower() or path.suffix.lower()
    if re.fullmatch(r"\.[a-z0-9]{1,8}", suffix):
        return suffix
    return mimetypes.guess_extension(mime_type.split(";")[0].strip()) or ""


def _atomic_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".attachment-", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(value, handle, ensure_ascii=False)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


class AttachmentStore:
    """Atomic originals with an indexed catalog, bounded storage and reminder pins."""

    def __init__(self, workspace: Path):
        self.workspace = Path(workspace).resolve()
        self.root = self.workspace / "data" / "attachments"
        self.trusted_roots = {self.workspace}

    @contextmanager
    def _catalog(self):
        self.root.mkdir(parents=True, exist_ok=True)
        db = sqlite3.connect(self.root / "catalog.sqlite3", timeout=10)
        try:
            db.execute("CREATE TABLE IF NOT EXISTS originals (id TEXT PRIMARY KEY, session TEXT, received REAL, size INTEGER, deleted INTEGER DEFAULT 0)")
            db.execute("CREATE INDEX IF NOT EXISTS originals_recent ON originals(session, deleted, received DESC)")
            db.execute("CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT)")
            if not db.execute("SELECT 1 FROM metadata WHERE key='imported'").fetchone():
                # A one-time upgrade of existing manifests, never a per-turn scan.
                for path in self.root.glob("*/att_*/manifest.json"):
                    try:
                        record = json.loads(path.read_text())
                        db.execute("INSERT OR IGNORE INTO originals VALUES (?, ?, ?, ?, 0)",
                                   (record["id"], record["session_key"], record["received_at"], record["size_bytes"]))
                    except (OSError, ValueError, KeyError):
                        continue
                db.execute("INSERT OR IGNORE INTO metadata VALUES ('imported', '1')")
            yield db
            db.commit()
        finally:
            db.close()

    def configure(self, *, retention_days: int, max_mb: int) -> None:
        _atomic_json(self.root / "policy.json", {"retention_days": retention_days, "max_bytes": max_mb * 1024 * 1024})

    def policy(self) -> dict:
        try:
            return json.loads((self.root / "policy.json").read_text())
        except FileNotFoundError:
            return {"retention_days": 30, "max_bytes": 1024 * 1024 * 1024}

    def _pins(self) -> set[str]:
        """Read durable jobs so tools and cleanup agree across restarts."""
        path = self.workspace / "data" / "scheduler.json"
        try:
            data = json.loads(path.read_text())
        except FileNotFoundError:
            return set()
        except (ValueError, OSError) as exc:
            raise ValueError("提醒状态暂时无法读取，已暂停附件清理以保护原件。") from exc
        pins = set()
        jobs = data.get("jobs", []) if isinstance(data, dict) else data
        if isinstance(jobs, dict):
            jobs = jobs.values()
        for job in jobs:
            payload = job.get("payload", {})
            if job.get("status", "active") not in {"completed", "cancelled"}:
                # Paused schedules retain files for a later resume.
                pins.update(payload.get("attachment_ids", []))
            for run in job.get("run_history", []):
                if (not run.get("delivery_cancelled") and run.get("delivery_status") != "delivered"
                        and run.get("status") in {"dispatching", "queued", "running", "interrupted", "delivery_pending"}):
                    pins.update(run.get("reminder_payload", payload).get("attachment_ids", []))
        return pins

    def _remove(self, db, session_key: str, attachment_id: str) -> None:
        folder = self._scope(session_key) / attachment_id
        if folder.is_symlink() or not folder.resolve().is_relative_to(self.root.resolve()):
            raise ValueError("Invalid attachment directory")
        shutil.rmtree(folder, ignore_errors=False)
        # A tombstone prevents a delayed retry from restoring an explicitly deleted original.
        db.execute("UPDATE originals SET deleted=1, size=0, session='' WHERE id=?", (attachment_id,))

    def cleanup(self, *, force: bool = False) -> int:
        removed = 0
        with self._catalog() as db:
            row = db.execute("SELECT value FROM metadata WHERE key='cleanup_at'").fetchone()
            if not force and row and time.time() - float(row[0]) < 86400:
                return 0
            cutoff = time.time() - self.policy()["retention_days"] * 86400
            candidates = db.execute("SELECT id, session FROM originals WHERE deleted=0 AND received<?", (cutoff,)).fetchall()
            try:
                pins = self._pins() if candidates else set()
            except ValueError:
                logger.warning("Attachment cleanup deferred: scheduled job references unavailable")
                return 0
            for attachment_id, session in candidates:
                if attachment_id not in pins:
                    try:
                        self._remove(db, session, attachment_id)
                        removed += 1
                    except FileNotFoundError:
                        db.execute("UPDATE originals SET deleted=1, size=0, session='' WHERE id=?", (attachment_id,))
            db.execute("INSERT OR REPLACE INTO metadata VALUES ('cleanup_at', ?)", (str(time.time()),))
        return removed

    def usage(self, session_key: str) -> dict:
        with self._catalog() as db:
            count, size = db.execute("SELECT count(*), coalesce(sum(size),0) FROM originals WHERE session=? AND deleted=0", (session_key,)).fetchone()
        return {"count": count, "size_bytes": size, **self.policy()}

    def delete(self, session_key: str, attachment_id: str) -> None:
        self.get(session_key, attachment_id)
        if attachment_id in self._pins():
            raise ValueError("此附件仍被提醒或待投递任务引用。请先移除提醒中的附件或取消任务，再删除原件。")
        with self._catalog() as db:
            self._remove(db, session_key, attachment_id)

    def _scope(self, session_key: str) -> Path:
        if not session_key:
            raise ValueError("Attachment access requires a session")
        return self.root / hashlib.sha256(session_key.encode()).hexdigest()[:24]

    def get(self, session_key: str, attachment_id: str) -> dict:
        if not isinstance(attachment_id, str) or not re.fullmatch(r"att_[a-f0-9]{24}", attachment_id):
            raise ValueError("Invalid attachment ID")
        path = self._scope(session_key) / attachment_id / "manifest.json"
        try:
            record = json.loads(path.read_text())
        except FileNotFoundError as exc:
            raise ValueError("Attachment not found in this conversation") from exc
        if record.get("session_key") != session_key or record.get("id") != attachment_id:
            raise ValueError("Attachment does not belong to this conversation")
        target = (self.workspace / record["path"]).resolve()
        if (not target.is_relative_to(self.workspace) or not target.is_relative_to(path.parent.resolve())
                or not target.is_file()):
            raise ValueError("Saved attachment is unavailable")
        return record

    def path(self, record: dict) -> Path:
        return self.workspace / record["path"]

    def recent(self, session_key: str, *, limit: int = 10, since: float = 0) -> list[dict]:
        self.cleanup()
        records = []
        with self._catalog() as db:
            ids = db.execute("SELECT id FROM originals WHERE session=? AND deleted=0 AND received>=? ORDER BY received DESC LIMIT ?",
                             (session_key, since, max(1, min(limit, 100)))).fetchall()
        for (attachment_id,) in ids:
            try:
                records.append(self.get(session_key, attachment_id))
            except (OSError, ValueError, KeyError):
                continue
        return records

    def save(self, session_key: str, event_id: str, index: int, item: dict[str, Any],
             *, received_at: float | None = None) -> dict:
        identity = f"{session_key}\0{event_id}\0{index}"
        attachment_id = "att_" + hashlib.sha256(identity.encode()).hexdigest()[:24]
        with self._catalog() as db:
            row = db.execute("SELECT deleted FROM originals WHERE id=?", (attachment_id,)).fetchone()
            if row and row[0]:
                raise ValueError("该附件原件已删除或到期，请重新发送文件。")
        try:
            existing = self.get(session_key, attachment_id)
            with self._catalog() as db:
                db.execute("INSERT OR IGNORE INTO originals VALUES (?, ?, ?, ?, 0)",
                           (attachment_id, session_key, existing["received_at"], existing["size_bytes"]))
            if item.get("extraction_status"):
                self.update_extraction(session_key, attachment_id, item["extraction_status"], item.get("extraction_error", ""))
                return self.get(session_key, attachment_id)
            return existing
        except ValueError:
            # A first delivery has no manifest yet; continue with atomic creation.
            pass
        source = Path(item["url"]).resolve()
        if not source.is_file() or not any(source.is_relative_to(root) for root in self.trusted_roots):
            raise ValueError("Attachment must be a local file inside the workspace")
        self.cleanup(force=True)
        with self._catalog() as db:
            used = db.execute("SELECT coalesce(sum(size),0) FROM originals WHERE deleted=0").fetchone()[0]
        available = self.policy()["max_bytes"] - used
        quota_error = "附件保存空间已满，原件尚未持久保存。请删除不再需要的附件或请管理员增加 attachments.max_mb 后重试。"
        if source.stat().st_size > available:
            raise ValueError(quota_error)
        folder = self._scope(session_key) / attachment_id
        folder.mkdir(parents=True, exist_ok=True)
        original = str(item.get("name") or source.name).replace("\\", "/").rsplit("/", 1)[-1]
        original = re.sub(r'[\x00-\x1f<>:"/\\|?*]', "_", original).strip(" .") or "attachment"
        extension = document_extension(source, original, item.get("mime_type", ""))
        stem = Path(original).stem if Path(original).suffix else original
        # Cap UTF-8 bytes, not code points: Chinese names otherwise exceed NAME_MAX.
        stem = stem.encode("utf-8")[:180].decode("utf-8", errors="ignore") or "attachment"
        filename = stem + extension
        original_dir = folder / "original"
        original_dir.mkdir(exist_ok=True)
        target = original_dir / filename
        fd, temporary = tempfile.mkstemp(prefix=".original-", dir=original_dir)
        digest, copied = hashlib.sha256(), 0
        try:
            with os.fdopen(fd, "wb") as output, source.open("rb") as incoming:
                while chunk := incoming.read(1024 * 1024):
                    copied += len(chunk)
                    if copied > available:
                        raise ValueError(quota_error)
                    output.write(chunk)
                    digest.update(chunk)
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, target)
        finally:
            Path(temporary).unlink(missing_ok=True)
        record = {
            "id": attachment_id, "session_key": session_key, "event_id": event_id,
            "name": filename, "original_name": original, "type": item.get("type", "file"),
            "path": str(target.relative_to(self.workspace)),
            "mime_type": mimetypes.guess_type(filename)[0] or item.get("mime_type", ""),
            "size_bytes": target.stat().st_size,
            "sha256": digest.hexdigest(),
            "received_at": received_at if received_at is not None else time.time(),
            "extraction_status": item.get("extraction_status", "not_requested"),
            "extraction_error": item.get("extraction_error", ""),
        }
        _atomic_json(folder / "manifest.json", record)
        with self._catalog() as db:
            db.execute("INSERT OR REPLACE INTO originals VALUES (?, ?, ?, ?, 0)",
                       (attachment_id, session_key, record["received_at"], record["size_bytes"]))
        return record

    def update_extraction(self, session_key: str, attachment_id: str, status: str, error: str = "") -> None:
        record = self.get(session_key, attachment_id)
        record.update(extraction_status=status, extraction_error=error)
        _atomic_json(self._scope(session_key) / attachment_id / "manifest.json", record)

    def persist_resolved(self, session_key: str, event_id: str, items: list[dict],
                         *, received_at: float | None = None) -> list[dict]:
        records = []
        for index, item in enumerate(items):
            url = item.get("url", "")
            if not url or url.startswith(("http://", "https://", "data:")):
                continue
            try:
                record = self.save(session_key, event_id, index, item, received_at=received_at)
            except (OSError, ValueError) as exc:
                item["storage_error"] = str(exc)
                continue
            item.update(url=str(self.path(record)), name=record["name"], attachment_id=record["id"])
            records.append(record)
        return records

    @staticmethod
    def describe(record: dict) -> str:
        # JSON quoting keeps channel-supplied names from forging extra context lines.
        return (f"附件 ID={record['id']} 名称={json.dumps(record['name'], ensure_ascii=False)} "
                f"类型={record['type']} 大小={record['size_bytes']}字节 原件已保存 "
                f"正文状态={record.get('extraction_status', 'not_requested')}")
