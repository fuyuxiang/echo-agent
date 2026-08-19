# Documentation Guide

How to contribute to Echo Agent documentation.

---

## Tech Stack

- [MkDocs Material](https://squidfunk.github.io/mkdocs-material/)
- [mkdocs-static-i18n](https://github.com/ultrabug/mkdocs-static-i18n) — bilingual support
- Mermaid — diagrams
- Markdown — content format

## Local Preview

```bash
pip install -e ".[docs]"
mkdocs serve
```

Visit `http://127.0.0.1:8000`.

## File Organization

- Chinese is default: `docs/section/page.md`
- English counterpart: `docs/section/page.en.md`
- Both must maintain consistent structure

## Writing Principles

1. **User-task oriented**, not code-structure oriented
2. **Facts from code** — config fields, CLI commands generated or CI-verified
3. **Don't hand-write dynamic model lists** — document how to discover, not enumerate
4. **Don't copy entire configs** — link to auto-generated reference
5. **No absolute promises** — Beta project, avoid "never loses data" claims

## Auto-Generated Files

Do NOT manually edit (CI regenerates):

- `docs/reference/configuration.md` / `.en.md`
- `docs/assets/generated/config-example.yaml` / `.en.yaml`
