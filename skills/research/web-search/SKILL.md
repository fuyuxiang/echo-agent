---
name: web-search
description: "Free web search via DuckDuckGo — text, news, images. No API key needed. Self-hosted SearXNG as alternative."
version: 1.0.0
metadata:
  echo:
    tags: [Search, Web, DuckDuckGo, SearXNG, Research]
---

# Web Search

Free web search without API keys. Two options: DuckDuckGo (zero config) or SearXNG (self-hosted).

## DuckDuckGo (Primary)

### Python library (recommended)

```bash
pip install duckduckgo_search
```

```python
from duckduckgo_search import DDGS

# Text search
results = DDGS().text("query here", max_results=5)
for r in results:
    print(f"{r['title']}\n{r['href']}\n{r['body']}\n")

# News search
news = DDGS().news("AI agents", max_results=5)

# Image search
images = DDGS().images("cat meme", max_results=3)
```

### CLI one-liner

```bash
python3 -c "
from duckduckgo_search import DDGS
for r in DDGS().text('$QUERY', max_results=5):
    print(f\"{r['title']}\n  {r['href']}\n  {r['body'][:100]}\n\")
"
```

### Helper script

```bash
python3 scripts/web_search.py "your query" --max 5
python3 scripts/web_search.py "AI news" --type news
python3 scripts/web_search.py "cat" --type images
```

## SearXNG (Self-hosted alternative)

For privacy or heavy usage, deploy your own SearXNG instance:

```bash
# JSON API (no key needed for self-hosted)
curl -s "http://localhost:8080/search?q=QUERY&format=json&engines=google,bing,duckduckgo" | python3 -m json.tool
```

Docker deployment:
```bash
docker run -d -p 8080:8080 searxng/searxng
```

## Rate Limits

| Service | Limit | Notes |
|---------|-------|-------|
| DuckDuckGo | ~20-30 req/min | No official limit, be respectful |
| SearXNG | Unlimited (self-hosted) | Depends on upstream engines |

## Tips

- Use specific queries with operators: `site:github.com Python agent`
- For recent results, add year: `"2026" AI agent framework`
- DuckDuckGo respects `region` param: `DDGS().text(query, region="cn-zh")`
