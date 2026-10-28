#!/usr/bin/env python3
"""Notion API client: read/write pages and databases."""

import argparse
import json
import urllib.request

BASE = "https://api.notion.com/v1"
NOTION_VERSION = "2022-06-28"


def _request(method, path, token, data=None):
    url = f"{BASE}{path}"
    body = json.dumps(data).encode() if data else None
    req = urllib.request.Request(url, data=body, method=method)
    req.add_header("Authorization", f"Bearer {token}")
    req.add_header("Notion-Version", NOTION_VERSION)
    req.add_header("Content-Type", "application/json")
    resp = urllib.request.urlopen(req, timeout=15)
    return json.loads(resp.read())


def list_databases(token):
    data = _request("POST", "/search", token, {"filter": {"value": "database", "property": "object"}})
    for db in data.get("results", []):
        title = "".join(t["plain_text"] for t in db.get("title", []))
        print(f"  {title} — {db['id']}")


def query_database(token, database_id, limit=20):
    data = _request("POST", f"/databases/{database_id}/query", token, {"page_size": limit})
    for page in data.get("results", []):
        props = page.get("properties", {})
        title_prop = next((v for v in props.values() if v["type"] == "title"), None)
        if title_prop:
            title = "".join(t["plain_text"] for t in title_prop.get("title", []))
            print(f"  {title} — {page['id']}")


def create_page(token, database_id, title, content=""):
    data = {
        "parent": {"database_id": database_id},
        "properties": {
            "Name": {"title": [{"text": {"content": title}}]}
        },
    }
    if content:
        data["children"] = [
            {"object": "block", "type": "paragraph",
             "paragraph": {"rich_text": [{"type": "text", "text": {"content": content}}]}}
        ]
    result = _request("POST", "/pages", token, data)
    print(f"Created: {result['id']} — {title}")


def main():
    parser = argparse.ArgumentParser(description="Notion client")
    parser.add_argument("--token", required=True, help="Notion integration token")
    sub = parser.add_subparsers(dest="cmd")
    sub.add_parser("databases")
    p = sub.add_parser("query")
    p.add_argument("database_id")
    p.add_argument("--limit", type=int, default=20)
    p = sub.add_parser("create")
    p.add_argument("database_id")
    p.add_argument("title")
    p.add_argument("--content", default="")
    args = parser.parse_args()

    if args.cmd == "databases":
        list_databases(args.token)
    elif args.cmd == "query":
        query_database(args.token, args.database_id, args.limit)
    elif args.cmd == "create":
        create_page(args.token, args.database_id, args.title, args.content)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
