#!/usr/bin/env python3
"""RSS feed monitor with SQLite-backed seen tracking."""

import argparse
import hashlib
import sqlite3
import sys
from pathlib import Path

try:
    import feedparser
except ImportError:
    sys.exit("Install: pip install feedparser")

try:
    import yaml
except ImportError:
    yaml = None

DB_PATH = Path.home() / ".echo-agent" / "feeds.db"
CONFIG_PATH = Path.home() / ".echo-agent" / "feeds.yaml"


def get_db():
    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(DB_PATH))
    conn.execute("CREATE TABLE IF NOT EXISTS seen (id TEXT PRIMARY KEY, url TEXT, title TEXT, seen_at TEXT DEFAULT CURRENT_TIMESTAMP)")
    return conn


def load_feeds():
    if not CONFIG_PATH.exists():
        return []
    if yaml:
        with open(CONFIG_PATH) as f:
            config = yaml.safe_load(f)
        return config.get("feeds", [])
    return []


def entry_id(entry):
    raw = entry.get("id") or entry.get("link") or entry.get("title", "")
    return hashlib.md5(raw.encode()).hexdigest()


def check_feeds(feeds=None, max_items=10):
    if feeds is None:
        feeds = load_feeds()
    conn = get_db()
    new_items = []
    for feed_conf in feeds:
        url = feed_conf if isinstance(feed_conf, str) else feed_conf.get("url")
        label = feed_conf.get("label", url) if isinstance(feed_conf, dict) else url
        category = feed_conf.get("category", "") if isinstance(feed_conf, dict) else ""
        parsed = feedparser.parse(url)
        for entry in parsed.entries[:max_items]:
            eid = entry_id(entry)
            exists = conn.execute("SELECT 1 FROM seen WHERE id=?", (eid,)).fetchone()
            if not exists:
                title = entry.get("title", "No title")
                link = entry.get("link", "")
                conn.execute("INSERT INTO seen (id, url, title) VALUES (?, ?, ?)", (eid, link, title))
                new_items.append({"title": title, "url": link, "category": category, "source": label})
    conn.commit()
    return new_items


def main():
    parser = argparse.ArgumentParser(description="RSS Feed Monitor")
    sub = parser.add_subparsers(dest="cmd")
    sub.add_parser("check")
    sub.add_parser("list")
    p_add = sub.add_parser("add")
    p_add.add_argument("url")
    p_add.add_argument("--category", default="general")
    args = parser.parse_args()

    if args.cmd == "check":
        items = check_feeds()
        if items:
            print(f"📰 {len(items)} new articles:\n")
            for item in items:
                cat = f"[{item['category']}] " if item["category"] else ""
                print(f"  {cat}{item['title']}\n    {item['url']}")
        else:
            print("No new articles.")
    elif args.cmd == "list":
        feeds = load_feeds()
        for f in feeds:
            url = f if isinstance(f, str) else f.get("url")
            label = f.get("label", "") if isinstance(f, dict) else ""
            print(f"  {label or url}")
    elif args.cmd == "add":
        print(f"Add to {CONFIG_PATH}: - url: {args.url}")
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
