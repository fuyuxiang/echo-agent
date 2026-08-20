# Skill Authoring Guide

Skills are Echo Agent's knowledge extension units, written in Markdown format with optional Python scripts. The Agent automatically selects and executes appropriate Skills based on user intent.

## Directory Structure

```
skills/
├── utility/
│   ├── calculator/
│   │   ├── SKILL.md         # Skill definition (required)
│   │   └── scripts/         # Optional scripts
│   │       └── calc.py
│   └── text-tools/
│       └── SKILL.md
├── productivity/
│   └── ...
└── research/
    └── ...
```

## SKILL.md Format

Each Skill is defined by a `SKILL.md` file containing YAML frontmatter and Markdown body:

```markdown
---
name: calculator
description: "Math calculations, unit conversions, date/time arithmetic, and currency rates. Python-powered, no API needed for math."
version: 1.0.0
metadata:
  echo:
    tags: [Math, Calculator, Units, Date, Currency, Utility]
---

# Calculator

Math, units, dates, and currency conversion.

## Math Expressions

Safe evaluation via Python:

```python
import ast
result = eval(compile(ast.parse("2**10 + 3.14 * 2", mode='eval'), '', 'eval'))
```

## Script

```bash
python3 scripts/calc.py "2**32 - 1"
python3 scripts/calc.py convert 100 km mi
```
```

## Frontmatter Fields

| Field | Required | Description |
|-------|----------|-------------|
| `name` | Yes | Unique Skill identifier (lowercase, hyphen-separated) |
| `description` | Yes | Functionality description (Agent uses this to decide when to invoke) |
| `version` | No | Semantic version number |
| `metadata.echo.tags` | No | Category tags (for search and filtering) |
| `metadata.echo.dependencies` | No | Python package dependency list |
| `metadata.echo.requires_env` | No | Required environment variables |
| `metadata.echo.risk_level` | No | Risk level: `read` / `write` / `exec` |

## Writing Principles

### 1. Description is Key

The `description` is the sole basis for the Agent to decide whether to use the Skill. Requirements:

- Clearly list capabilities (keyword-rich)
- Mention abilities that don't require external APIs (lowers usage barriers)
- Keep to 1-2 sentences

```yaml
# Good description
description: "Math calculations, unit conversions, date/time arithmetic, and currency rates. Python-powered, no API needed for math."

# Poor description
description: "A calculator tool"
```

### 2. Body Provides Execution Guidance

The Markdown body serves as a reference manual when the Agent executes the Skill. It should include:

- Concrete code examples (Agent references these during execution)
- Available commands and parameters
- Common usage patterns
- Edge cases and caveats

### 3. Scripts are Optional

Skills can be:

- **Pure knowledge** — Only SKILL.md, Agent reasons based on content
- **Script-assisted** — Includes a `scripts/` directory, Agent invokes scripts for concrete tasks

## Full Example: Web Search Skill

```markdown
---
name: web-search
description: "Search the web for current information, news, documentation, and answers. Uses DuckDuckGo, no API key required."
version: 1.0.0
metadata:
  echo:
    tags: [Search, Web, News, Research]
    dependencies: [duckduckgo_search]
    risk_level: read
---

# Web Search

Search the internet for up-to-date information.

## Basic Search

```python
from duckduckgo_search import DDGS

with DDGS() as ddgs:
    results = list(ddgs.text("query here", max_results=5))
    for r in results:
        print(f"- [{r['title']}]({r['href']})")
        print(f"  {r['body']}")
```

## News Search

```python
with DDGS() as ddgs:
    news = list(ddgs.news("topic", max_results=5))
    for n in news:
        print(f"- {n['title']} ({n['date']})")
        print(f"  {n['body']}")
```

## Best Practices

- Use specific, targeted queries
- Combine multiple searches for comprehensive coverage
- Verify facts from multiple sources
- Include date constraints for time-sensitive queries
```

## Skills with Dependencies

If a Skill requires additional Python packages, declare them in metadata:

```yaml
metadata:
  echo:
    dependencies: [duckduckgo_search, trafilatura]
    requires_env: []  # No API Key needed
```

Corresponding Python packages should be declared in `pyproject.toml` under `[project.optional-dependencies] skills`.

## Skill Categories

Organize Skills into directories by domain:

| Directory | Domain | Examples |
|-----------|--------|----------|
| `creative/` | Creative generation | Writing assistance, brainstorming |
| `development/` | Software development | Code review, refactoring suggestions |
| `devops/` | DevOps automation | Deployment, monitoring |
| `finance/` | Finance | Exchange rates, budget calculations |
| `health/` | Health management | Nutrition calculations, exercise planning |
| `learning/` | Learning assistance | Flashcards, note organization |
| `media/` | Multimedia | Image processing, audio conversion |
| `productivity/` | Productivity | Scheduling, task management |
| `research/` | Research/analysis | Data collection, literature review |
| `utility/` | General utilities | Calculator, text processing |

## Testing Skills

### Manual Testing

```bash
# Start the agent, then trigger the skill by talking to it on the CLI channel
echo-agent run
```

If a gateway is already running, attach to the same instance as a thin client:

```bash
echo-agent cli
```

Neither command takes the message as an argument — start it, then type your prompt in the interactive session, e.g. "Calculate 2^32 - 1".

### Evaluation Testing

Add test cases to the evaluation dataset (see [Testing & Evaluation](testing-evaluation.en.md)):

```yaml
- id: calculator_power
  input: "Calculate 2 to the power of 32 minus 1"
  expected_contains: ["4294967295"]
  expected_tools: ["skill_run"]
  tags: [skill, calculator]
```

## Checklist

- [ ] `SKILL.md` contains valid YAML frontmatter
- [ ] `name` is globally unique (lowercase, hyphenated)
- [ ] `description` is keyword-rich, clearly describes capability boundaries
- [ ] Body contains executable code examples
- [ ] If dependencies exist, declare in both metadata and pyproject.toml
- [ ] Placed in the correct category directory
- [ ] Manual test verifies Agent can correctly trigger the Skill

!!! note "There is no mutual exclusion or priority between skills"
    Every enabled skill is injected into the system prompt as one flat list of name, category and description, and the model decides which one applies this turn. The framework offers no exclusion declaration and does not rank skills.

    That makes the `description` the only place to encode distinctness: when two skills describe overlapping territory, the model is left guessing from wording and the outcome is unstable. Writing each skill's scope so it excludes the others is more effective than trying to correct the choice afterwards.
