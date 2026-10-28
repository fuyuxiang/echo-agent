#!/usr/bin/env python3
"""Notes manager - local Markdown knowledge base."""

import argparse
import re
from datetime import date
from pathlib import Path

NOTES_DIR = Path.home() / ".echo-agent" / "notes"


def init_dir():
    NOTES_DIR.mkdir(parents=True, exist_ok=True)
    (NOTES_DIR / "daily").mkdir(exist_ok=True)


def create_note(title: str, content: str = "", tags: list = None):
    init_dir()
    slug = re.sub(r"[^\w一-鿿]+", "-", title).strip("-").lower()
    path = NOTES_DIR / f"{slug}.md"
    tags_str = ", ".join(tags) if tags else ""
    frontmatter = f"---\ntitle: {title}\ntags: [{tags_str}]\ncreated: {date.today()}\n---\n\n"
    path.write_text(frontmatter + f"# {title}\n\n{content}\n")
    print(f"Created: {path}")


def daily_note():
    init_dir()
    today = date.today().isoformat()
    path = NOTES_DIR / "daily" / f"{today}.md"
    if not path.exists():
        path.write_text(f"---\ntitle: {today}\ntags: [daily]\ncreated: {today}\n---\n\n# {today}\n\n")
        print(f"Created daily note: {path}")
    else:
        print(f"Daily note exists: {path}")
    return path


def search_notes(query: str):
    init_dir()
    results = []
    for p in NOTES_DIR.rglob("*.md"):
        content = p.read_text()
        if query.lower() in content.lower():
            title_match = re.search(r"^# (.+)$", content, re.M)
            title = title_match.group(1) if title_match else p.stem
            results.append((p, title))
    for path, title in results:
        print(f"  {title} — {path.relative_to(NOTES_DIR)}")
    if not results:
        print("No results found.")
    return results


def list_notes():
    init_dir()
    notes = list(NOTES_DIR.rglob("*.md"))
    for p in sorted(notes):
        rel = p.relative_to(NOTES_DIR)
        print(f"  {rel}")
    print(f"\nTotal: {len(notes)} notes")


def find_backlinks(note_name: str):
    init_dir()
    pattern = f"[[{note_name}]]"
    results = []
    for p in NOTES_DIR.rglob("*.md"):
        if pattern in p.read_text():
            results.append(p)
    print(f"Backlinks to '{note_name}':")
    for p in results:
        print(f"  {p.relative_to(NOTES_DIR)}")
    if not results:
        print("  (none)")


def list_tags():
    init_dir()
    tags = {}
    for p in NOTES_DIR.rglob("*.md"):
        content = p.read_text()
        match = re.search(r"^tags:\s*\[(.+)\]", content, re.M)
        if match:
            for tag in match.group(1).split(","):
                tag = tag.strip()
                if tag:
                    tags[tag] = tags.get(tag, 0) + 1
    for tag, count in sorted(tags.items(), key=lambda x: -x[1]):
        print(f"  {tag} ({count})")


def main():
    parser = argparse.ArgumentParser(description="Notes Manager")
    sub = parser.add_subparsers(dest="cmd")
    p = sub.add_parser("create")
    p.add_argument("title")
    p.add_argument("--content", "-c", default="")
    p.add_argument("--tags", "-t", nargs="*", default=[])
    sub.add_parser("daily")
    p = sub.add_parser("search")
    p.add_argument("query")
    sub.add_parser("list")
    sub.add_parser("tags")
    p = sub.add_parser("backlinks")
    p.add_argument("name")
    args = parser.parse_args()

    if args.cmd == "create":
        create_note(args.title, args.content, args.tags)
    elif args.cmd == "daily":
        daily_note()
    elif args.cmd == "search":
        search_notes(args.query)
    elif args.cmd == "list":
        list_notes()
    elif args.cmd == "tags":
        list_tags()
    elif args.cmd == "backlinks":
        find_backlinks(args.name)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
