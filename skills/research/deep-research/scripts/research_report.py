#!/usr/bin/env python3
"""Deep research: multi-step search → extract → synthesize with citations."""

import argparse
import sys
from datetime import date

try:
    from duckduckgo_search import DDGS
except ImportError:
    sys.exit("Install: pip install duckduckgo_search")

try:
    import trafilatura
except ImportError:
    trafilatura = None


def search_multi(question: str, max_queries: int = 5, results_per: int = 3):
    ddgs = DDGS()
    all_results = []
    queries = [question, f"{question} 2026", f"{question} best practices"]
    for q in queries[:max_queries]:
        results = ddgs.text(q, max_results=results_per)
        all_results.extend(results)
    seen = set()
    unique = []
    for r in all_results:
        if r["href"] not in seen:
            seen.add(r["href"])
            unique.append(r)
    return unique


def extract_page(url: str) -> str:
    if not trafilatura:
        return ""
    downloaded = trafilatura.fetch_url(url)
    if not downloaded:
        return ""
    return trafilatura.extract(downloaded, include_links=False) or ""


def generate_report(question: str, depth: str = "normal"):
    max_q = {"quick": 2, "normal": 5, "deep": 10}.get(depth, 5)
    max_pages = {"quick": 3, "normal": 8, "deep": 15}.get(depth, 8)

    print(f"Researching: {question} (depth={depth})")
    results = search_multi(question, max_queries=max_q)
    print(f"Found {len(results)} unique URLs")

    sources = []
    for i, r in enumerate(results[:max_pages]):
        content = extract_page(r["href"]) if trafilatura else r.get("body", "")
        sources.append({
            "title": r["title"],
            "url": r["href"],
            "snippet": r.get("body", "")[:200],
            "content": content[:2000] if content else r.get("body", ""),
        })
        print(f"  [{i+1}/{min(len(results), max_pages)}] {r['title'][:50]}...")

    report = f"# Research Report: {question}\n\n"
    report += f"**Date:** {date.today()}\n"
    report += f"**Sources consulted:** {len(sources)}\n\n"
    report += "## Sources\n\n"
    for i, s in enumerate(sources, 1):
        report += f"[{i}]: {s['url']} — {s['title']}\n"
    report += "\n## Key Findings\n\n"
    for i, s in enumerate(sources, 1):
        if s["snippet"]:
            report += f"- {s['snippet']} [{i}]\n"

    return report, sources


def main():
    parser = argparse.ArgumentParser(description="Deep research report generator")
    parser.add_argument("question", help="Research question")
    parser.add_argument("--depth", choices=["quick", "normal", "deep"], default="normal")
    parser.add_argument("--output", "-o", help="Save report to file")
    args = parser.parse_args()

    report, _ = generate_report(args.question, args.depth)

    if args.output:
        with open(args.output, "w") as f:
            f.write(report)
        print(f"\nReport saved to {args.output}")
    else:
        print("\n" + report)


if __name__ == "__main__":
    main()
