---
name: web-search
description: "Free web search via 360 Search, Bing RSS and DDGS — text, news, images. No API key needed. Self-hosted SearXNG as alternative."
version: 1.0.0
metadata:
  echo:
    tags: [Search, Web, DDGS, SearXNG, Research]
    requires:
      pip: ["ddgs>=9.16.1,<10", "lxml>=4.9.4"]
---

# Web Search

Free web search without API keys. Two options: DDGS metasearch (zero config) or SearXNG (self-hosted).

For Chinese queries, the helper first uses 360 Search and returns source links
and snippets. Other text queries use Bing RSS. It falls back to DDGS if these
sources fail. News and
images use DDGS. If script execution requires approval, basic text search can
also use `web_fetch` with `https://cn.bing.com/search?q=<URL-encoded query>&format=rss`.

## DDGS library (direct use)

### Python library (recommended)

```bash
pip install "ddgs>=9.16.1,<10"
```

```python
from ddgs import DDGS

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
from ddgs import DDGS
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

Run the helper from the agent with `skill_run(name="web-search", script="scripts/web_search.py", args=["your query", "--max", "5", "--json"])`. Script paths are relative to this skill directory.

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
