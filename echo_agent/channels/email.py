"""Email channel — IMAP polling + SMTP sending."""

from __future__ import annotations

import asyncio
import email
import email.mime.text
import imaplib
import re
import smtplib
from email.header import decode_header

from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.config.schema import EmailChannelConfig
from echo_agent.utils.text import html_to_text


class EmailChannel(BaseChannel):
    name = "email"

    def __init__(self, config: EmailChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._poll_task: asyncio.Task | None = None
        self._processed_uids: set[str] = set()
        self._subject_map: dict[str, str] = {}

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
        text = event.text or ""
        if not text:
            return SendResult(success=False, error="no text")
        to_addr = event.chat_id
        subject = self._subject_map.get(to_addr, "Re: Echo Agent")
        loop = asyncio.get_event_loop()
        try:
            await loop.run_in_executor(None, self._send_smtp, to_addr, subject, text)
        except Exception as e:
            return SendResult(success=False, error=str(e))
        return SendResult(success=True)

    def _send_smtp(self, to_addr: str, subject: str, body: str) -> None:
        try:
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
        except Exception as e:
            logger.error("Email send failed to {}: {}", to_addr, e)

    async def _poll_loop(self) -> None:
        loop = asyncio.get_event_loop()
        while self._running:
            try:
                messages = await loop.run_in_executor(None, self._fetch_imap)
                for from_addr, subject, body in messages:
                    self._subject_map[from_addr] = subject
                    await self._handle_message(
                        sender_id=from_addr, chat_id=from_addr, text=body,
                        metadata={"subject": subject},
                    )
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error("Email poll error: {}", e)
            await asyncio.sleep(self.config.poll_interval_seconds)

    def _fetch_imap(self) -> list[tuple[str, str, str]]:
        results: list[tuple[str, str, str]] = []
        try:
            if self.config.use_ssl:
                conn = imaplib.IMAP4_SSL(self.config.imap_host, self.config.imap_port)
            else:
                conn = imaplib.IMAP4(self.config.imap_host, self.config.imap_port)
            conn.login(self.config.username, self.config.password)
            conn.select("INBOX")
            _, data = conn.search(None, "UNSEEN")
            uids = data[0].split() if data[0] else []
            for uid in uids:
                uid_str = uid.decode()
                if uid_str in self._processed_uids:
                    continue
                self._processed_uids.add(uid_str)
                _, msg_data = conn.fetch(uid, "(RFC822)")
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
                    results.append((from_addr, subject, body))
            conn.logout()
        except Exception as e:
            logger.error("IMAP fetch error: {}", e)
        if len(self._processed_uids) > 100000:
            self._processed_uids = set(list(self._processed_uids)[-50000:])
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
