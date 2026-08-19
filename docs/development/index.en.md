# Development

Welcome to Echo Agent development! This section covers everything from environment setup to release processes.

## Contents

| Document | Description |
|----------|-------------|
| [Development Setup](setup.en.md) | Python/Node environment, dependencies, IDE configuration |
| [Repository Map](repository-map.en.md) | Directory structure and module responsibilities by subsystem |
| [Adding a Provider](add-provider.en.md) | Integrate a new LLM service provider |
| [Adding a Tool](add-tool.en.md) | Add new tools for the Agent |
| [Adding a Channel](add-channel.en.md) | Develop a new messaging channel adapter |
| [Skill Authoring](skill-authoring.en.md) | Write skills in the SKILL.md format |
| [Plugin API](plugin-api.en.md) | Plugin manifest, lifecycle hooks, sandbox |
| [Dashboard Development](dashboard-development.en.md) | React SPA architecture, components, testing |
| [Testing & Evaluation](testing-evaluation.en.md) | pytest, evaluation framework, coverage gates |
| [Documentation Guide](documentation.en.md) | Docs site architecture, i18n, local preview |
| [Release Process](release-process.en.md) | Versioning, building, release checklist |

## Quick Start

```bash
# Clone the repository
git clone https://github.com/fuyuxiang/echo-agent.git
cd echo-agent

# Backend: install all dependencies (including dev tools)
pip install -e ".[all,dev]"

# Frontend: install Dashboard dependencies
cd web && pnpm install --frozen-lockfile && cd ..

# Verify environment
ruff check .
python -m pytest tests/ -v --cov
cd web && pnpm build && pnpm test --run
```

## Development Principles

- **Test first** — New features must include corresponding test cases
- **Type safety** — Use Pydantic models and type hints
- **Minimal dependencies** — Optional features isolated via extras (e.g., `[openai]`, `[browser]`)
- **Backward compatible** — Config changes go through migration mechanisms, never break existing deployments
