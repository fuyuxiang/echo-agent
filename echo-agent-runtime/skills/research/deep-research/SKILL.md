---
name: deep-research
description: "Structured multi-step research: search → extract → cross-reference → synthesize with citations."
version: 1.0.0
metadata:
  echo:
    tags: [Research, Deep, Analysis, Synthesis, Citations]
---

# Deep Research

Multi-step research workflow that produces cited reports from multiple sources.

## Workflow

1. **Decompose** — Break the question into 3-5 sub-questions
2. **Search** — Run multiple searches per sub-question (diverse keywords)
3. **Extract** — Fetch and extract content from top URLs
4. **Cross-reference** — Compare claims across sources, flag conflicts
5. **Synthesize** — Write structured report with inline citations

## Usage

```bash
python3 scripts/research_report.py "What are the best practices for LLM agent memory in 2026?"
python3 scripts/research_report.py "Compare FastAPI vs Litestar performance" --depth deep
```

## Output Format

Reports follow this template:

```markdown
# Research Report: {Topic}

**Date:** YYYY-MM-DD
**Sources consulted:** N

## Summary
2-3 sentence overview of findings.

## Key Findings

### Finding 1
Detail with evidence. [Source 1][1] confirms that...

### Finding 2
...

## Conflicting Information
Where sources disagree, note both positions.

## Conclusion
Actionable synthesis.

## Sources
[1]: https://... — Title
[2]: https://... — Title
```

## Depth Levels

| Level | Searches | Pages Read | Time |
|-------|----------|-----------|------|
| quick | 2-3 | 3-5 | ~30s |
| normal | 5-8 | 8-12 | ~2min |
| deep | 10-15 | 15-25 | ~5min |

## Combining with Other Skills

- Use `web-search` for the search step
- Use `web-extract` for the extraction step
- Use `summarize` skill for per-page summaries before synthesis
- Store results with `note-taking` skill
