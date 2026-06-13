#!/usr/bin/env python3
"""Reminder store - SQLite-backed reminder CRUD."""

import argparse
import sqlite3
from datetime import datetime
from pathlib import Path

DB_PATH = Path.home() / ".echo-agent" / "reminders.db"


def get_db():
    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(DB_PATH))
    conn.execute("""CREATE TABLE IF NOT EXISTS reminders (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        content TEXT NOT NULL,
        cron_expr TEXT,
        next_fire TEXT,
        is_recurring INTEGER DEFAULT 0,
        is_done INTEGER DEFAULT 0,
        created_at TEXT DEFAULT CURRENT_TIMESTAMP
    )""")
    return conn


def add_reminder(content: str, cron_expr: str = None, at: str = None):
    conn = get_db()
    is_recurring = 1 if cron_expr and any(c in cron_expr for c in "*,/") else 0
    next_fire = at or ""
    if cron_expr:
        try:
            from croniter import croniter
            next_fire = str(croniter(cron_expr, datetime.now()).get_next(datetime))
        except ImportError:
            pass
    conn.execute(
        "INSERT INTO reminders (content, cron_expr, next_fire, is_recurring) VALUES (?, ?, ?, ?)",
        (content, cron_expr, next_fire, is_recurring),
    )
    conn.commit()
    print(f"Reminder added: {content}")


def list_reminders(show_done=False):
    conn = get_db()
    query = "SELECT id, content, cron_expr, next_fire, is_done FROM reminders"
    if not show_done:
        query += " WHERE is_done = 0"
    for row in conn.execute(query):
        status = "[done]" if row[4] else "[pending]"
        fire = f" @ {row[3]}" if row[3] else ""
        cron = f" ({row[2]})" if row[2] else ""
        print(f"  #{row[0]} {status} {row[1]}{fire}{cron}")


def mark_done(reminder_id: int):
    conn = get_db()
    conn.execute("UPDATE reminders SET is_done = 1 WHERE id = ?", (reminder_id,))
    conn.commit()
    print(f"Reminder #{reminder_id} marked done.")


def delete_reminder(reminder_id: int):
    conn = get_db()
    conn.execute("DELETE FROM reminders WHERE id = ?", (reminder_id,))
    conn.commit()
    print(f"Reminder #{reminder_id} deleted.")


def get_due():
    conn = get_db()
    now = datetime.now().isoformat()
    rows = conn.execute(
        "SELECT id, content FROM reminders WHERE is_done = 0 AND next_fire <= ? AND next_fire != ''",
        (now,),
    ).fetchall()
    for row in rows:
        print(f"  DUE #{row[0]}: {row[1]}")
    return rows


def main():
    parser = argparse.ArgumentParser(description="Reminder store")
    sub = parser.add_subparsers(dest="cmd")
    p_add = sub.add_parser("add")
    p_add.add_argument("content")
    p_add.add_argument("--cron")
    p_add.add_argument("--at")
    sub.add_parser("list")
    p_done = sub.add_parser("done")
    p_done.add_argument("id", type=int)
    p_del = sub.add_parser("delete")
    p_del.add_argument("id", type=int)
    sub.add_parser("due")
    args = parser.parse_args()
    if args.cmd == "add":
        add_reminder(args.content, args.cron, args.at)
    elif args.cmd == "list":
        list_reminders()
    elif args.cmd == "done":
        mark_done(args.id)
    elif args.cmd == "delete":
        delete_reminder(args.id)
    elif args.cmd == "due":
        get_due()
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
