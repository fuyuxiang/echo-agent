"""Email channel — IMAP polling + SMTP sending."""

from __future__ import annotations

import asyncio
import email
import email.mime.text
import imaplib
import json
import re
import smtplib
from dataclasses import dataclass
from pathlib import Path
from email.header import decode_header

from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.config.schema import EmailChannelConfig
from echo_agent.utils.text import html_to_text


@dataclass(frozen=True)
class _FetchedEmail:
    """One UID in the contiguous IMAP batch returned by ``_fetch_imap``.

    Filtered senders and empty/malformed messages are represented too, with
    ``publish=False``.  The async poller can then advance one contiguous UID
    watermark without either retrying permanent skips forever or jumping over
    a publishable message that the inbound bus did not accept.
    """

    uid: int
    from_addr: str = ""
    subject: str = ""
    body: str = ""
    publish: bool = False


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
        # No try/except: callers (send) depend on the exception to surface the
        # failure. Logging-only previously turned every SMTP failure into a
        # silent "success".
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
                await self._process_fetched(messages)
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error("Email poll error: {}", e)
            await asyncio.sleep(self.config.poll_interval_seconds)

    async def _process_fetched(self, messages: list[_FetchedEmail]) -> None:
        """Publish and acknowledge a contiguous batch in ascending UID order.

        A UID becomes durable only after its inbound event was accepted by the
        bus.  Filtered/empty messages need no publish and are acknowledged
        immediately.  On the first failed publish we stop, leaving that UID and
        every later UID above the persisted watermark for the next poll.
        """
        for message in messages:
            if message.publish:
                self._subject_map[message.from_addr] = message.subject
                accepted = await self._handle_message(
                    sender_id=message.from_addr,
                    chat_id=message.from_addr,
                    text=message.body,
                    metadata={"subject": message.subject, "uid": str(message.uid)},
                )
                if accepted is None:
                    logger.warning(
                        "Email UID {} was not accepted by the inbound bus; "
                        "keeping watermark at {} for retry",
                        message.uid,
                        self._last_seen_uid,
                    )
                    break

            self._last_seen_uid = max(self._last_seen_uid, message.uid)
            self._save_state()

    def _fetch_imap(self) -> list[_FetchedEmail]:
        """Fetch the contiguous UID batch above the durable watermark.

        Uses UID SEARCH (not the sequence-number SEARCH), and only asks the
        server for UIDs above the persisted watermark.  This synchronous method
        deliberately does *not* mutate or save the watermark: publish happens
        later on the event loop, and only a successfully accepted inbound event
        may acknowledge a publishable email.

        * The old code did ``conn.search(None, "UNSEEN")`` and stored the
          returned sequence numbers in ``_processed_uids``. Sequence numbers
          are unstable across EXPUNGE/append — a message expunged mid-session
          shifts every subsequent seqno, so the dedup logic either let
          already-seen messages through or skipped genuinely new ones.
        * The dedup state was in-memory only, so every restart re-fetched
          every UNSEEN message and republished them.

        Filtered senders and empty bodies are returned as non-publishable
        records.  They still occupy their place in the ordered batch, allowing
        the poller to acknowledge them without skipping over an earlier email
        whose publish failed.
        """
        results: list[_FetchedEmail] = []
        conn = None
        watermark = self._last_seen_uid
        try:
            if self.config.use_ssl:
                conn = imaplib.IMAP4_SSL(self.config.imap_host, self.config.imap_port)
            else:
                conn = imaplib.IMAP4(self.config.imap_host, self.config.imap_port)
            conn.login(self.config.username, self.config.password)
            conn.select("INBOX")
            _, data = conn.uid("SEARCH", None, f"UID {watermark + 1}:*")
            uids = data[0].split() if data[0] else []
            uid_values: list[int] = []
            for raw_uid in uids:
                try:
                    uid = int(raw_uid.decode())
                except ValueError:
                    logger.warning("Ignoring invalid IMAP UID: {!r}", raw_uid)
                    continue
                # RFC sequence ranges can resolve ``N:*`` backwards when N is
                # above the mailbox's current maximum.  Filter the server's
                # result explicitly so the last acknowledged UID is not fetched
                # and published again on an otherwise empty poll.
                if uid > watermark:
                    uid_values.append(uid)

            for uid in sorted(set(uid_values)):
                _, msg_data = conn.uid("FETCH", str(uid), "(RFC822)")
                if not msg_data or not msg_data[0]:
                    # Do not jump over a UID we failed to fetch.  Retrying the
                    # same contiguous suffix is safer than permanently losing
                    # one message because a server returned a partial response.
                    logger.warning("IMAP FETCH returned no body for UID {}; retrying later", uid)
                    break
                raw = msg_data[0][1]
                try:
                    msg = email.message_from_bytes(raw)
                    from_addr = self._parse_address(msg.get("From", ""))
                    if not self.is_allowed(from_addr):
                        results.append(_FetchedEmail(uid=uid))
                        continue
                    subject = self._decode_header(msg.get("Subject", ""))
                    body = self._extract_body(msg)
                    results.append(_FetchedEmail(
                        uid=uid,
                        from_addr=from_addr,
                        subject=subject,
                        body=body,
                        publish=bool(body),
                    ))
                except Exception as e:
                    # MIME parsing is deterministic for a given raw message. A
                    # poison email must not pin every later UID forever.
                    logger.warning("Skipping malformed email UID {}: {}", uid, e)
                    results.append(_FetchedEmail(uid=uid))
        except Exception as e:
            logger.error("IMAP fetch error: {}", e)
        finally:
            if conn is not None:
                try:
                    conn.logout()
                except Exception:
                    pass
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
