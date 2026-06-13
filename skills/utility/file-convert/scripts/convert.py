#!/usr/bin/env python3
"""Universal file format converter."""

import argparse
import csv
import json
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    yaml = None


def csv_to_json(src, dst):
    with open(src, newline="", encoding="utf-8-sig") as f:
        data = list(csv.DictReader(f))
    Path(dst).write_text(json.dumps(data, ensure_ascii=False, indent=2))


def json_to_csv(src, dst):
    data = json.loads(Path(src).read_text())
    if not data:
        return
    with open(dst, "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=data[0].keys())
        w.writeheader()
        w.writerows(data)


def yaml_to_json(src, dst):
    if not yaml:
        sys.exit("Install: pip install pyyaml")
    data = yaml.safe_load(Path(src).read_text())
    Path(dst).write_text(json.dumps(data, ensure_ascii=False, indent=2))


def json_to_yaml(src, dst):
    if not yaml:
        sys.exit("Install: pip install pyyaml")
    data = json.loads(Path(src).read_text())
    Path(dst).write_text(yaml.dump(data, allow_unicode=True, default_flow_style=False))


def md_to_html(src, dst):
    try:
        import markdown
    except ImportError:
        sys.exit("Install: pip install markdown")
    text = Path(src).read_text()
    html = markdown.markdown(text, extensions=["tables", "fenced_code"])
    Path(dst).write_text(f"<!DOCTYPE html><html><body>{html}</body></html>")


CONVERTERS = {
    (".csv", ".json"): csv_to_json,
    (".json", ".csv"): json_to_csv,
    (".yaml", ".json"): yaml_to_json,
    (".yml", ".json"): yaml_to_json,
    (".json", ".yaml"): json_to_yaml,
    (".json", ".yml"): json_to_yaml,
    (".md", ".html"): md_to_html,
}


def main():
    parser = argparse.ArgumentParser(description="File format converter")
    parser.add_argument("input", help="Input file")
    parser.add_argument("output", help="Output file")
    args = parser.parse_args()

    src_ext = Path(args.input).suffix.lower()
    dst_ext = Path(args.output).suffix.lower()
    converter = CONVERTERS.get((src_ext, dst_ext))
    if not converter:
        sys.exit(f"Unsupported: {src_ext} -> {dst_ext}\nSupported: {list(CONVERTERS.keys())}")
    converter(args.input, args.output)
    print(f"Converted: {args.input} -> {args.output}")


if __name__ == "__main__":
    main()
