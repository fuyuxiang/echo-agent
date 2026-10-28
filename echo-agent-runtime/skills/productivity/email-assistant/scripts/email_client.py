#!/usr/bin/env python3
"""IMAP/SMTP email client for reading, searching, and sending."""

import argparse
import email
import imaplib
import smtplib
from email.header import decode_header
from email.mime.multipart import MIMEMultipart
from email.mime.text import MIMEText


def decode_str(s):
    if not s:
        return ""
    parts = decode_header(s)
    return "".join(
        part.decode(enc or "utf-8") if isinstance(part, bytes) else part
        for part, enc in parts
    )


def list_recent(host, user, password, folder="INBOX", count=10, port=993):
    imap = imaplib.IMAP4_SSL(host, port)
    imap.login(user, password)
    imap.select(folder)
    _, data = imap.search(None, "ALL")
    ids = data[0].split()[-count:]
    for mid in reversed(ids):
        _, msg_data = imap.fetch(mid, "(BODY.PEEK[HEADER.FIELDS (FROM SUBJECT DATE)])")
        raw = msg_data[0][1]
        msg = email.message_from_bytes(raw)
        print(f"  [{decode_str(msg['Date'])}] {decode_str(msg['From'])}")
        print(f"    Subject: {decode_str(msg['Subject'])}")
    imap.close()
    imap.logout()


def search_mail(host, user, password, query, folder="INBOX", port=993):
    imap = imaplib.IMAP4_SSL(host, port)
    imap.login(user, password)
    imap.select(folder)
    _, data = imap.search(None, "SUBJECT", f'"{query}"')
    ids = data[0].split()[-20:]
    for mid in reversed(ids):
        _, msg_data = imap.fetch(mid, "(BODY.PEEK[HEADER.FIELDS (FROM SUBJECT DATE)])")
        raw = msg_data[0][1]
        msg = email.message_from_bytes(raw)
        print(f"  {decode_str(msg['Subject'])} — {decode_str(msg['From'])}")
    imap.close()
    imap.logout()


def send_mail(smtp_host, user, password, to, subject, body, port=465):
    msg = MIMEMultipart()
    msg["From"] = user
    msg["To"] = to
    msg["Subject"] = subject
    msg.attach(MIMEText(body, "plain", "utf-8"))
    with smtplib.SMTP_SSL(smtp_host, port) as server:
        server.login(user, password)
        server.send_message(msg)
    print(f"Sent to {to}: {subject}")


def main():
    parser = argparse.ArgumentParser(description="Email client")
    sub = parser.add_subparsers(dest="cmd")
    p = sub.add_parser("list")
    p.add_argument("--host", required=True)
    p.add_argument("--user", required=True)
    p.add_argument("--password", required=True)
    p.add_argument("--count", type=int, default=10)
    p = sub.add_parser("search")
    p.add_argument("--host", required=True)
    p.add_argument("--user", required=True)
    p.add_argument("--password", required=True)
    p.add_argument("query")
    p = sub.add_parser("send")
    p.add_argument("--smtp-host", required=True)
    p.add_argument("--user", required=True)
    p.add_argument("--password", required=True)
    p.add_argument("--to", required=True)
    p.add_argument("--subject", required=True)
    p.add_argument("--body", required=True)
    args = parser.parse_args()

    if args.cmd == "list":
        list_recent(args.host, args.user, args.password, count=args.count)
    elif args.cmd == "search":
        search_mail(args.host, args.user, args.password, args.query)
    elif args.cmd == "send":
        send_mail(args.smtp_host, args.user, args.password, args.to, args.subject, args.body)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
