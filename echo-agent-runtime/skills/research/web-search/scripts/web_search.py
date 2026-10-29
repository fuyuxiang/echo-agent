#!/usr/bin/env python3
"""Web search via 360 Search, Bing RSS and DDGS. No API key needed."""

import argparse
import json
from echo_agent.skills.web_search import search_text, search_news, search_images

def main():
    parser = argparse.ArgumentParser(description="Web search via DDGS")
    parser.add_argument("query", help="Search query")
    parser.add_argument("--max", type=int, default=5, help="Max results")
    parser.add_argument("--type", choices=["text", "news", "images"], default="text")
    parser.add_argument("--region", default="wt-wt", help="Region code (e.g. cn-zh)")
    parser.add_argument("--json", action="store_true", help="JSON output")
    args = parser.parse_args()

    if args.type == "text":
        results = search_text(args.query, args.max, args.region)
    elif args.type == "news":
        results = search_news(args.query, args.max)
    else:
        results = search_images(args.query, args.max)

    if args.json:
        print(json.dumps(results, ensure_ascii=False, indent=2))
    else:
        for i, r in enumerate(results, 1):
            title = r.get("title", "")
            url = r.get("href") or r.get("url") or r.get("image", "")
            body = r.get("body", r.get("description", ""))[:150]
            print(f"{i}. {title}\n   {url}\n   {body}\n")


if __name__ == "__main__":
    main()
