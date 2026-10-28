---
name: notion-sync
description: "Read, create, and update Notion pages and databases. Requires Notion API integration token."
version: 1.0.0
metadata:
  echo:
    tags: [Notion, Notes, Database, Sync, Productivity]
    requires:
      env: [NOTION_API_TOKEN]
---

# Notion Sync

Notion API integration for pages, databases, and content management.

## Setup

1. Create integration at https://www.notion.so/my-integrations
2. Get Internal Integration Token
3. Share target pages/databases with the integration
4. Set environment: `export NOTION_API_TOKEN=secret_xxx`

## Script

```bash
python3 scripts/notion_client.py --token secret_xxx databases
python3 scripts/notion_client.py --token secret_xxx query <database-id> --limit 20
python3 scripts/notion_client.py --token secret_xxx create <database-id> "Page Title" --content "Body text"
```

## API Usage

```python
import httpx

HEADERS = {
    "Authorization": f"Bearer {NOTION_API_TOKEN}",
    "Notion-Version": "2022-06-28",
    "Content-Type": "application/json",
}

# Search
resp = httpx.post("https://api.notion.com/v1/search",
    headers=HEADERS,
    json={"query": "project plan"})

# Read page blocks
resp = httpx.get(f"https://api.notion.com/v1/blocks/{page_id}/children",
    headers=HEADERS)

# Create page
resp = httpx.post("https://api.notion.com/v1/pages",
    headers=HEADERS,
    json={
        "parent": {"page_id": parent_id},
        "properties": {"title": [{"text": {"content": "New Page"}}]},
        "children": [
            {"paragraph": {"rich_text": [{"text": {"content": "Hello"}}]}}
        ]
    })
```

## Block Types

| Type | Use |
|------|-----|
| paragraph | Normal text |
| heading_1/2/3 | Headers |
| bulleted_list_item | Bullet points |
| numbered_list_item | Numbered lists |
| code | Code blocks |
| toggle | Collapsible sections |
| to_do | Checkboxes |

## Common Workflows

- Save chat notes to Notion page
- Query reading list database
- Create daily journal entry
- Append meeting notes from template
