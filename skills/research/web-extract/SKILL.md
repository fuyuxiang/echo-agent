---
name: web-extract
description: "Extract clean text content from any URL. Uses trafilatura for high-quality extraction, no API key needed."
version: 1.0.0
metadata:
  echo:
    tags: [Web, Extract, Scraping, Content, URL]
---

# Web Extract

Extract readable text/markdown from any URL. Uses trafilatura — the best Python content extraction library (handles news, blogs, docs reliably).

## Quick Usage

```bash
pip install trafilatura httpx
```

```python
import trafilatura

# Fetch and extract in one step
text = trafilatura.fetch_and_extract("https://example.com/article")
print(text)

# With more options
downloaded = trafilatura.fetch_url("https://example.com/article")
result = trafilatura.extract(downloaded, output_format="markdown", include_links=True)
```

### Helper script

```bash
python3 scripts/extract_url.py "https://example.com/article"
python3 scripts/extract_url.py "https://example.com" --format markdown --links
```

## Options

| Parameter | Effect |
|-----------|--------|
| `output_format="markdown"` | Markdown with headers |
| `include_links=True` | Preserve hyperlinks |
| `include_images=True` | Include image references |
| `include_tables=True` | Preserve table structure |
| `favor_recall=True` | Extract more (less precision) |

## Fallback: httpx + readability

For pages where trafilatura struggles:

```python
import httpx
from readability import Document

resp = httpx.get(url, follow_redirects=True, timeout=15)
doc = Document(resp.text)
title = doc.title()
content = doc.summary()  # HTML, needs html2text for markdown
```

## JavaScript-heavy sites

For SPAs or JS-rendered content, use playwright (optional):

```python
from playwright.sync_api import sync_playwright

with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page()
    page.goto(url, wait_until="networkidle")
    html = page.content()
    browser.close()
# Then pass html to trafilatura.extract()
```

## Rate Limits

Be respectful: add 1-2 second delays between requests to the same domain.
Set a User-Agent: `trafilatura.fetch_url(url, config=config)` with custom config.
