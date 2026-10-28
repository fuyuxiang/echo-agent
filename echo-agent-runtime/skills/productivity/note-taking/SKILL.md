---
name: note-taking
description: "Manage a local Markdown knowledge base — create, search, link, and organize notes. No external service needed."
version: 1.0.0
metadata:
  echo:
    tags: [Notes, Knowledge, Markdown, Wiki, Productivity]
---

# Note Taking

Local Markdown knowledge base stored in `~/.echo-agent/notes/`.

## Operations

| Action | Command |
|--------|---------|
| Create | `python3 scripts/notes_manager.py create "Title" --content "..."` |
| Search | `python3 scripts/notes_manager.py search "keyword"` |
| List | `python3 scripts/notes_manager.py list` |
| Today | `python3 scripts/notes_manager.py daily` |
| Tags | `python3 scripts/notes_manager.py tags` |
| Links | `python3 scripts/notes_manager.py backlinks "note-name"` |

## Note Format

Notes use YAML frontmatter:

```markdown
---
title: My Note
tags: [project, idea]
created: 2026-06-13
---

# My Note

Content here. Link to [[other-note]] using wiki-links.
```

## Features

- **Wiki-links**: Use `[[note-title]]` to link between notes
- **Tags**: YAML frontmatter tags for categorization
- **Daily notes**: Auto-created `YYYY-MM-DD.md` journal entries
- **Full-text search**: Grep-based search across all notes
- **Backlinks**: Discover which notes link to a given note

## Storage

```
~/.echo-agent/notes/
├── daily/
│   ├── 2026-06-13.md
│   └── 2026-06-12.md
├── projects/
│   └── echo-agent.md
└── ideas/
    └── new-feature.md
```
