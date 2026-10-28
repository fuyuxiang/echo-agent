---
name: rss-watcher
description: "Monitor RSS/Atom feeds for new articles. Poll on schedule, summarize updates, deliver via any channel."
version: 1.0.0
metadata:
  echo:
    tags: [RSS, Atom, Feed, Monitor, News, Research]
---

# RSS Watcher

Monitor RSS/Atom feeds and get notified of new articles.

## Configuration

Create `~/.echo-agent/feeds.yaml`:

```yaml
feeds:
  - url: https://hnrss.org/frontpage
    category: tech
    label: Hacker News
  - url: https://arxiv.org/rss/cs.AI
    category: ai
    label: arXiv AI
  - url: https://feeds.feedburner.com/ruanyifeng
    category: blog
    label: 阮一峰
poll_interval: "*/30 * * * *"  # every 30 minutes
max_items: 10
```

## Usage

```bash
pip install feedparser

python3 scripts/feed_monitor.py check        # check all feeds for new items
python3 scripts/feed_monitor.py list         # list configured feeds
python3 scripts/feed_monitor.py add "https://example.com/feed.xml" --category tech
```

## How It Works

1. Reads feed URLs from `feeds.yaml`
2. Parses via `feedparser` library
3. Tracks seen entries in SQLite (`~/.echo-agent/feeds.db`) by entry ID/link
4. Returns only new (unseen) entries
5. Delivers updates through active channel

## Output Format

```
📰 New Articles:

[tech] Hacker News
  • Title of Article 1
    https://link-to-article.com
  • Title of Article 2
    https://link-to-article.com

[ai] arXiv AI
  • New Paper Title
    https://arxiv.org/abs/2406.xxxxx
```

## Integration

Combine with Echo Agent's cron channel for automatic polling. Set `poll_interval` in feeds.yaml to control check frequency.
