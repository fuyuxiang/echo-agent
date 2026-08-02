"""Email channel — IMAP polling + SMTP sending."""

from __future__ import annotations

import asyncio
import email
import email.mime.text
import imaplib
import json
import re
import smtplib
from pathlib import Path
from email.header import decode_header

from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.config.schema import EmailChannelConfig
from echo_agent.utils.text import html_to_text


class EmailChannel(BaseChannel):
    name = "email"
    is_realtime = False

    def __init__(self, config: EmailChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._poll_task: asyncio.Task | None = None
        # High-water UID; only fetch UIDs above this. Persisted so restarts
        # don't re-fetch (and potentially re-publish) messages we've already
        # processed. See _fetch_imap for the IMAP protocol details.
        self._last_seen_uid: int = 0
        self._state_path = self._resolve_state_path()
        self._subject_map: dict[str, str] = {}
        self._load_state()

    def _resolve_state_path(self) -> Path:
        # Co-locate with the rest of the channel state under the data dir.
        # ``data_dir`` lives on BaseChannel via ChannelManager; the default
        # of ~/.echo-agent/data is fine for the common case.
        from echo_agent.runtime_paths import echo_home
        return echo_home() / "data" / "email_state.json"

    def _load_state(self) -> None:
        try:
            if self._state_path.is_file():
                data = json.loads(self._state_path.read_text(encoding="utf-8"))
                self._last_seen_uid = int(data.get("last_seen_uid", 0) or 0)
        except Exception as e:
            logger.warning("Email state file unreadable ({}); starting fresh", e)
            self._last_seen_uid = 0

    def _save_state(self) -> None:
        try:
            self._state_path.parent.mkdir(parents=True, exist_ok=True)
            tmp = self._state_path.with_suffix(self._state_path.suffix + ".tmp")
            tmp.write_text(
                json.dumps({"last_seen_uid": self._last_seen_uid}),
                encoding="utf-8",
            )
            tmp.replace(self._state_path)
        except OSError as e:
            logger.warning("Failed to persist email state: {}", e)

    async def start(self) -> None:
        self._running = True
        self.bus.subscribe_outbound(self.name, self.send)
        self._poll_task = asyncio.create_task(self._poll_loop())
        logger.info("Email channel started (IMAP: {})", self.config.imap_host)

    async def stop(self) -> None:
        self._running = False
        if self._poll_task:
            self._poll_task.cancel()
            try:
                await self._poll_task
            except asyncio.CancelledError:
                pass

    async def send(self, event: OutboundEvent) -> SendResult | None:
        if not self.should_deliver(event):
            return SendResult(success=True, skipped=True)
        text = event.text or ""
        if not text:
            return SendResult(success=False, error="no text")
        to_addr = event.chat_id
        subject = self._subject_map.get(to_addr, "Re: Echo Agent")
        loop = asyncio.get_running_loop()
        # Let the SMTP failure surface to the bus: ``run_in_executor`` would
        # swallow the exception into a Future, so re-raise inside the executor
        # call and let ``publish_outbound``'s _aggregate see it. The previous
        # code logged and returned, which made every SMTP failure look like
        # success and marked the cron/task complete.
        try:
            await loop.run_in_executor(None, self._send_smtp, to_addr, subject, text)
        except Exception as e:
            return SendResult(success=False, error=str(e))
        return SendResult(success=True)

    def _send_smtp(self, to_addr: str, subject: str, body: str) -> None:
        # No try/except: callers (``send``) depend on the exception to surface
        # the failure. Logging only here used to turn every SMTP failure into
        # a silent "success" — see reviewer P1-6 for the broader receipt story.
        msg = email.mime.text.MIMEText(body, "plain", "utf-8")
        msg["From"] = self.config.username
        msg["To"] = to_addr
        msg["Subject"] = f"Re: {subject}" if not subject.startswith("Re:") else subject
        if self.config.use_ssl:
            with smtplib.SMTP_SSL(self.config.smtp_host, self.config.smtp_port) as smtp:
                smtp.login(self.config.username, self.config.password)
                smtp.send_message(msg)
        else:
            with smtplib.SMTP(self.config.smtp_host, self.config.smtp_port) as smtp:
                smtp.starttls()
                smtp.login(self.config.username, self.config.password)
                smtp.send_message(msg)

    async def _poll_loop(self) -> None:
        loop = asyncio.get_running_loop()
        while self._running:
            try:
                messages = await loop.run_in_executor(None, self._fetch_imap)
                for uid, from_addr, subject, body in messages:
                    self._subject_map[from_addr] = subject
                    # Mark-after-publish: the watermark already advanced
                    # inside ``_fetch_imap`` so a transient bus failure here
                    # will be retried on the next poll (the UID stays in
                    # the watermark's search range). Publish returns a
                    # bool; ``accepted=False`` just means this turn didn't
                    # hand it off — we keep moving forward.
                    await self._handle_message(
                        sender_id=from_addr, chat_id=from_addr, text=body,
                        metadata={"subject": subject, "uid": uid},
                    )
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error("Email poll error: {}", e)
            await asyncio.sleep(self.config.poll_interval_seconds)

    def _fetch_imap(self) -> list[tuple[str, str, str, str]]:
        """Return ``[(uid_str, from_addr, subject, body), ...]`` for unseen mail.

        Uses UID SEARCH (not the sequence-number SEARCH), and only asks the
        server for UIDs above the persisted watermark. Two prior bugs this
        fixes:

        * The old code did ``conn.search(None, "UNSEEN")`` and stored the
          returned sequence numbers in ``_processed_uids``. Sequence numbers
          are unstable across EXPUNGE/append — a message expunged mid-session
          shifts every subsequent seqno, so the dedup logic either let
          already-seen messages through or skipped genuinely new ones.
        * The dedup state was in-memory only, so every restart re-fetched
          every UNSEEN message and republished them.

        ``is_allowed`` and the body parse run inside this fetch (rather than
        the poll loop) so the poll loop can advance the watermark uniformly
        for every seen UID — a sender outside ``allow_from`` must not keep
        the watermark pinned and force the server to rescan the same UIDs
        forever.
        """
        results: list[tuple[str, str, str, str]] = []
        try:
            if self.config.use_ssl:
                conn = imaplib.IMAP4_SSL(self.config.imap_host, self.config.imap_port)
            else:
                conn = imaplib.IMAP4(self.config.imap_host, self.config.imap_port)
            conn.login(self.config.username, self.config.password)
            conn.select("INBOX")
            _, data = conn.uid("SEARCH", None, f"UID {self._last_seen_uid + 1}:*")
            uids = data[0].split() if data[0] else []
            for uid in uids:
                uid_str = uid.decode()
                # Advance the watermark unconditionally for every UID we
                # see: the watermark is the high-water mark of *seen*
                # messages, not the set of UIDs we actually published.
                # Without this an empty-body message (or one filtered out
                # by ``allow_from``) would pin the watermark and force the
                # server to rescan it on every poll.
                try:
                    self._last_seen_uid = max(self._last_seen_uid, int(uid_str))
                except ValueError:
                    continue
                _, msg_data = conn.uid("FETCH", uid_str, "(RFC822)")
                if not msg_data or not msg_data[0]:
                    continue
                raw = msg_data[0][1]
                msg = email.message_from_bytes(raw)
                from_addr = self._parse_address(msg.get("From", ""))
                if not self.is_allowed(from_addr):
                    continue
                subject = self._decode_header(msg.get("Subject", ""))
                body = self._extract_body(msg)
                if body:
                    results.append((uid_str, from_addr, subject, body))
            self._save_state()
            conn.logout()
        except Exception as e:
            logger.error("IMAP fetch error: {}", e)
        return results

    @staticmethod
    def _parse_address(raw: str) -> str:
        match = re.search(r"<([^>]+)>", raw)
        return match.group(1) if match else raw.strip()

    @staticmethod
    def _decode_header(raw: str) -> str:
        parts = decode_header(raw)
        decoded = []
        for part, charset in parts:
            if isinstance(part, bytes):
                decoded.append(part.decode(charset or "utf-8", errors="replace"))
            else:
                decoded.append(part)
        return " ".join(decoded)

    @staticmethod
    def _extract_body(msg: email.message.Message) -> str:
        if msg.is_multipart():
            for part in msg.walk():
                ct = part.get_content_type()
                if ct == "text/plain":
                    payload = part.get_payload(decode=True)
                    if payload:
                        charset = part.get_content_charset() or "utf-8"
                        return payload.decode(charset, errors="replace").strip()
                elif ct == "text/html":
                    payload = part.get_payload(decode=True)
                    if payload:
                        charset = part.get_content_charset() or "utf-8"
                        return html_to_text(payload.decode(charset, errors="replace"))
        else:
            payload = msg.get_payload(decode=True)
            if payload:
                charset = msg.get_content_charset() or "utf-8"
                text = payload.decode(charset, errors="replace").strip()
                if msg.get_content_type() == "text/html":
                    return html_to_text(text)
                return text
        return ""
