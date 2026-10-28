#!/usr/bin/env python3
"""Text processing: clean, count, regex, encode/decode."""

import argparse
import base64
import html
import re
from urllib.parse import quote, unquote

PATTERNS = {
    "email": r"[\w.-]+@[\w.-]+\.\w+",
    "url": r"https?://\S+",
    "phone_cn": r"1[3-9]\d{9}",
    "ip": r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}",
    "date": r"\d{4}-\d{2}-\d{2}",
    "chinese": r"[一-鿿]+",
}


def clean(text):
    text = re.sub(r"<[^>]+>", "", text)
    text = " ".join(text.split())
    return text


def count(text):
    chars = len(text)
    chars_no_space = len(text.replace(" ", ""))
    words = len(text.split())
    chinese = len(re.findall(r"[一-鿿]", text))
    print(f"Characters: {chars} (no space: {chars_no_space})")
    print(f"Words: {words}")
    print(f"Chinese chars: {chinese}")


def regex_extract(pattern_name, text):
    pattern = PATTERNS.get(pattern_name, pattern_name)
    matches = re.findall(pattern, text)
    for m in matches:
        print(m)
    if not matches:
        print("No matches.")


def encode(method, text):
    if method == "url":
        print(quote(text))
    elif method == "base64":
        print(base64.b64encode(text.encode()).decode())
    elif method == "html":
        print(html.escape(text))


def decode(method, text):
    if method == "url":
        print(unquote(text))
    elif method == "base64":
        print(base64.b64decode(text).decode())
    elif method == "html":
        print(html.unescape(text))


def main():
    parser = argparse.ArgumentParser(description="Text processor")
    sub = parser.add_subparsers(dest="cmd")
    sub.add_parser("clean").add_argument("text")
    sub.add_parser("count").add_argument("text")
    p = sub.add_parser("regex-extract")
    p.add_argument("pattern", help="Pattern name or regex")
    p.add_argument("text")
    p = sub.add_parser("encode")
    p.add_argument("method", choices=["url", "base64", "html"])
    p.add_argument("text")
    p = sub.add_parser("decode")
    p.add_argument("method", choices=["url", "base64", "html"])
    p.add_argument("text")
    args = parser.parse_args()

    if args.cmd == "clean":
        print(clean(args.text))
    elif args.cmd == "count":
        count(args.text)
    elif args.cmd == "regex-extract":
        regex_extract(args.pattern, args.text)
    elif args.cmd == "encode":
        encode(args.method, args.text)
    elif args.cmd == "decode":
        decode(args.method, args.text)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
