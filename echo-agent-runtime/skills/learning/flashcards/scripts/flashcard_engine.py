#!/usr/bin/env python3
"""Flashcard engine with SM-2 spaced repetition."""

import argparse
import csv
import sqlite3
from datetime import datetime, timedelta
from pathlib import Path

DB_PATH = Path.home() / ".echo-agent" / "flashcards.db"


def get_db():
    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(DB_PATH))
    conn.executescript("""
        CREATE TABLE IF NOT EXISTS decks (id INTEGER PRIMARY KEY, name TEXT UNIQUE, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
        CREATE TABLE IF NOT EXISTS cards (
            id INTEGER PRIMARY KEY, deck_id INTEGER, front TEXT, back TEXT,
            card_type TEXT DEFAULT 'basic', repetitions INTEGER DEFAULT 0,
            easiness REAL DEFAULT 2.5, interval INTEGER DEFAULT 0,
            next_review TEXT, created_at TEXT DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (deck_id) REFERENCES decks(id)
        );
    """)
    return conn


def sm2(quality, repetitions, easiness, interval):
    if quality >= 3:
        if repetitions == 0:
            interval = 1
        elif repetitions == 1:
            interval = 6
        else:
            interval = round(interval * easiness)
        repetitions += 1
    else:
        repetitions = 0
        interval = 1
    easiness = max(1.3, easiness + 0.1 - (5 - quality) * (0.08 + (5 - quality) * 0.02))
    return repetitions, easiness, interval


def create_deck(name):
    conn = get_db()
    conn.execute("INSERT OR IGNORE INTO decks (name) VALUES (?)", (name,))
    conn.commit()
    print(f"Deck '{name}' created.")


def add_card(deck_name, front, back, card_type="basic"):
    conn = get_db()
    row = conn.execute("SELECT id FROM decks WHERE name=?", (deck_name,)).fetchone()
    if not row:
        print(f"Deck '{deck_name}' not found. Creating...")
        create_deck(deck_name)
        row = conn.execute("SELECT id FROM decks WHERE name=?", (deck_name,)).fetchone()
    next_review = datetime.now().isoformat()
    conn.execute("INSERT INTO cards (deck_id, front, back, card_type, next_review) VALUES (?, ?, ?, ?, ?)",
                 (row[0], front, back, card_type, next_review))
    conn.commit()
    print(f"Card added to '{deck_name}'.")


def get_due(deck_name=None, limit=20):
    conn = get_db()
    now = datetime.now().isoformat()
    if deck_name:
        rows = conn.execute("""SELECT c.id, c.front, c.back, d.name FROM cards c
            JOIN decks d ON c.deck_id=d.id WHERE d.name=? AND c.next_review<=? LIMIT ?""",
            (deck_name, now, limit)).fetchall()
    else:
        rows = conn.execute("""SELECT c.id, c.front, c.back, d.name FROM cards c
            JOIN decks d ON c.deck_id=d.id WHERE c.next_review<=? LIMIT ?""",
            (now, limit)).fetchall()
    for r in rows:
        print(f"  #{r[0]} [{r[3]}] Q: {r[1]}")
    if not rows:
        print("No cards due for review.")
    return rows


def review_card(card_id, quality):
    conn = get_db()
    row = conn.execute("SELECT repetitions, easiness, interval FROM cards WHERE id=?", (card_id,)).fetchone()
    if not row:
        print(f"Card #{card_id} not found.")
        return
    reps, ease, intv = sm2(quality, row[0], row[1], row[2])
    next_review = (datetime.now() + timedelta(days=intv)).isoformat()
    conn.execute("UPDATE cards SET repetitions=?, easiness=?, interval=?, next_review=? WHERE id=?",
                 (reps, ease, intv, next_review, card_id))
    conn.commit()
    print(f"Card #{card_id} reviewed (quality={quality}). Next review in {intv} days.")


def deck_stats(deck_name):
    conn = get_db()
    now = datetime.now().isoformat()
    row = conn.execute("SELECT id FROM decks WHERE name=?", (deck_name,)).fetchone()
    if not row:
        print(f"Deck '{deck_name}' not found.")
        return
    total = conn.execute("SELECT COUNT(*) FROM cards WHERE deck_id=?", (row[0],)).fetchone()[0]
    due = conn.execute("SELECT COUNT(*) FROM cards WHERE deck_id=? AND next_review<=?", (row[0], now)).fetchone()[0]
    new = conn.execute("SELECT COUNT(*) FROM cards WHERE deck_id=? AND repetitions=0", (row[0],)).fetchone()[0]
    print(f"Deck '{deck_name}': {total} total, {due} due, {new} new")


def import_csv(file_path, deck_name):
    conn = get_db()
    create_deck(deck_name)
    row = conn.execute("SELECT id FROM decks WHERE name=?", (deck_name,)).fetchone()
    count = 0
    with open(file_path, newline="", encoding="utf-8") as f:
        reader = csv.DictReader(f)
        for r in reader:
            front = r.get("front", "")
            back = r.get("back", "")
            if front and back:
                next_review = datetime.now().isoformat()
                conn.execute("INSERT INTO cards (deck_id, front, back, next_review) VALUES (?, ?, ?, ?)",
                             (row[0], front, back, next_review))
                count += 1
    conn.commit()
    print(f"Imported {count} cards into '{deck_name}'.")


def main():
    parser = argparse.ArgumentParser(description="Flashcard Engine (SM-2)")
    sub = parser.add_subparsers(dest="cmd")
    p = sub.add_parser("create-deck")
    p.add_argument("name")
    p = sub.add_parser("add")
    p.add_argument("deck")
    p.add_argument("front")
    p.add_argument("back", nargs="?", default="")
    p.add_argument("--type", default="basic")
    p = sub.add_parser("due")
    p.add_argument("deck", nargs="?")
    p.add_argument("--limit", type=int, default=20)
    p = sub.add_parser("review")
    p.add_argument("card_id", type=int)
    p.add_argument("quality", type=int, choices=range(6))
    p = sub.add_parser("stats")
    p.add_argument("deck")
    p = sub.add_parser("import")
    p.add_argument("file")
    p.add_argument("deck")
    args = parser.parse_args()

    if args.cmd == "create-deck":
        create_deck(args.name)
    elif args.cmd == "add":
        add_card(args.deck, args.front, args.back, args.type)
    elif args.cmd == "due":
        get_due(args.deck, args.limit)
    elif args.cmd == "review":
        review_card(args.card_id, args.quality)
    elif args.cmd == "stats":
        deck_stats(args.deck)
    elif args.cmd == "import":
        import_csv(args.file, args.deck)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
