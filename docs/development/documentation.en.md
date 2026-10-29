# Documentation Guide

How to contribute to Echo Agent documentation.

---

## Tech Stack

- [MkDocs Material](https://squidfunk.github.io/mkdocs-material/)
- [mkdocs-static-i18n](https://github.com/ultrabug/mkdocs-static-i18n) — bilingual support
- Mermaid — diagrams
- Markdown — content format

## Local Preview

Run these commands from the repository root.

```bash
pip install -e "./echo-agent-runtime[docs]"
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

### README Architecture Diagrams

The project README and architecture overview share the bilingual SVGs in `docs/assets/readme/`. Maintain diagrams through their generator and run from the repository root after changing topology or labels:

```bash
python3 scripts/generate-readme-diagrams.py
python3 scripts/generate-readme-diagrams.py --check
```

The generator header records implementation sources. Review modules, arrow direction, process boundaries, and optional features against the code, then render both languages and inspect labels and connectors. `--check` verifies artifact consistency only. Maintain the brand banner and buttons separately; keep architecture diagrams as editable vectors and use real application screenshots with their applicable version identified.

### Configuration Reference

Do NOT manually edit (CI regenerates):

- `docs/reference/configuration.md` / `.en.md`
- `docs/assets/generated/config-example.yaml` / `.en.yaml`
