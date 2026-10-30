# Documentation contribution

The site uses MkDocs Material, bilingual Markdown, and SVG / Mermaid diagrams. Chinese is the default language. English pages cover the same topics and feature boundaries.

## Content responsibilities

| Type | Include | Avoid |
| --- | --- | --- |
| Quickstart | Prerequisites, shortest working steps, success criteria | Unverified timing promises or hidden requirements |
| Guide | Goals, operations, results, next steps | Feature lists without instructions |
| Concept | Design purpose, responsibilities, interaction, data, limits | Source snippets, class-field inventories, source paths |
| Reference | Exact commands, fields, APIs, defaults | Hand-maintained copies of generated data |
| Development | Engineering locations, extension, build, verification | Implementation details in ordinary user flows |

State product scope. Verify Desktop TOML and the embedded Rust engine separately from standalone Runtime YAML and Python. Similar names do not establish shared implementation. A helper module does not prove a capability is available in user workflows.

## Verify facts

Check requirements against dependency declarations and release scripts; commands against parsers; examples against schemas and loading rules; permissions against execution paths; UI terms against current components. State disabled defaults, remote dependencies, inbound-only support, and manual rollback near the relevant claim.

Use service-provided model IDs rather than static recommendations. Generate field references and keep guide examples focused. Distinguish execution state, operation success, and delivery; model text is not execution evidence.

## Diagram conventions

| Purpose | Diagram | Requirements |
| --- | --- | --- |
| Systems and external dependencies | C4 system context | Name Person, Software System, and relationship meanings; omit source modules |
| Expand a running unit | C4-style components | State scope; distinguish components, external entities, stores, optional features |
| Interaction order | UML sequence | Participants, lifelines, solid requests, dashed returns, alt / opt / loop / par fragments |
| Lifecycle | UML state | Initial state, states, event/condition transitions; final nodes only for actual termination |
| Control flow | Standard flowchart | Terminals, processing rectangles, decision diamonds, labeled outcomes |
| Data movement | Yourdon / DeMarco DFD | External rectangles, process circles, parallel-line stores, data-named arrows |

Provide scope and a legend where needed. DFD numbers are not step order. Dependencies do not imply shared state. Decision conditions belong on branches. Do not draw unsupported directions. Color supplements explicit meaning.

SVG uses readable text, arrows, and vector shapes. Mermaid uses the relevant diagram grammar. Inspect both languages for clipping, crossings, boundaries, fonts, and narrow-screen scaling. Generator checks do not replace visual review.

## Generate and preview

From the repository root:

```bash
pip install -e "./echo-agent-runtime[docs]"
python -m echo_agent.config.docgen --output-dir docs/reference --lang both
python3 scripts/generate-readme-diagrams.py
mkdocs serve
```

Open <http://127.0.0.1:8000/>. Generate configuration references before preview and strict builds; do not edit generated references manually.

## Verify before submitting

```bash
python3 scripts/generate-readme-diagrams.py --check
python -m pytest scripts/tests/test_docs_structure.py echo-agent-runtime/tests/test_docs_consistency.py
mkdocs build --strict
```

Check built internal links and inspect bilingual landing, onboarding, and diagram pages in a browser. The documentation workflow checks links after building and deploys Pages after a push to the primary branch. A successful local build does not establish a live update.

## Files and generated artifacts

Chinese pages use `docs/section/page.md`; English uses `page.en.md`. Maintain navigation in `mkdocs.yml`, preserve stable routes, and link new content from relevant entry pages.

Configuration references and examples under `docs/assets/generated/` are generated. Architecture, flowchart, and DFD SVGs under `docs/assets/readme/` are maintained by `scripts/generate-readme-diagrams.py`. Update diagram sources and explanations when relations change.
