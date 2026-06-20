[中文](CONTRIBUTING.md) · [English](CONTRIBUTING.en.md)

# Contributing

Thanks for taking the time to contribute to Echo Agent. Bug fixes, features and docs are all welcome.

## Set up a dev environment

```bash
git clone https://github.com/fuyuxiang/echo-agent.git   # mirror: https://gitee.com/fuyuxiang/echo-agent.git
cd echo-agent
uv venv venv --python 3.11 && source venv/bin/activate
uv pip install -e ".[all,dev]"
```

No `uv`? A standard venv works too:

```bash
python3.11 -m venv venv && source venv/bin/activate
pip install -e ".[all,dev]"
```

## Pre-submit checks

Make sure lint and tests pass before opening a PR — CI runs the same checks on every PR:

```bash
ruff check .
pytest
```

## Opening a PR

- Branch off `master` and keep each PR focused on a single topic.
- Keep commit messages about the change itself — clean and to the point.
- For user-facing changes, update both READMEs (`README.md` / `README.en.md`).
- When you change config fields, run `echo-agent config gen-docs` to regenerate the config reference.

## Good entry points

Not sure where to start? These areas could use a hand:

- **Channel adapters** — connect more messaging platforms
- **Built-in tools** — grow the out-of-the-box toolset
- **MCP integrations** — wire up more MCP servers
- **Skill examples** — contribute reusable skill samples
- **Eval datasets** — enrich the self-evolution eval cases
- **Documentation** — tutorials, examples and guides
- **Deployment templates** — Docker, Kubernetes, cloud providers

## Community

- [GitHub Discussions](https://github.com/fuyuxiang/echo-agent/discussions) — design discussion, usage questions
- [GitHub Issues](https://github.com/fuyuxiang/echo-agent/issues) — bugs and feature requests
- QQ group: [47572014](https://qm.qq.com/q/JWOPDBNssw)
