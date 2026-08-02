#!/usr/bin/env python3
"""Extract clean text from URLs using trafilatura."""

import argparse
from echo_agent.dependencies.skill_require import require  # noqa: E402

require("skill.web-extract")

import trafilatura  # noqa: E402


def extract(url: str, output_format: str = "txt", include_links: bool = False) -> str:
    downloaded = trafilatura.fetch_url(url)
    if not downloaded:
        return f"Error: Could not fetch {url}"
    result = trafilatura.extract(
        downloaded,
        output_format=output_format if output_format != "txt" else "txt",
        include_links=include_links,
        include_tables=True,
    )
    return result or "No content extracted."


def main():
    parser = argparse.ArgumentParser(description="Extract content from URL")
    parser.add_argument("url", help="URL to extract")
    parser.add_argument("--format", choices=["txt", "markdown", "xml"], default="txt")
    parser.add_argument("--links", action="store_true", help="Include links")
    args = parser.parse_args()

    text = extract(args.url, args.format, args.links)
    print(text)


if __name__ == "__main__":
    main()
