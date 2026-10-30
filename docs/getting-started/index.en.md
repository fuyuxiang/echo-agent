# Getting started

Choose an operating mode before preparing your environment and model. Installation, configuration, and storage are separate for the two products.

| Goal | Reading path |
| --- | --- |
| Desktop coding, documents, and automation | [Desktop installation](../desktop/installation.md) → [First task](../desktop/quickstart.md) |
| Terminal use or self-hosting | [Runtime installation](installation.md) → [Runtime quickstart](quickstart.md) |
| Connect your application | [Runtime quickstart](quickstart.md) → [Gateway](../integrations/gateway/index.md) |

## Short Runtime path

Prepare Python 3.11+ and a working model service. Install, run setup, and choose terminal interaction or Gateway.

```bash
pip install "echo-agent[all]"
echo-agent setup
echo-agent gateway
```

Open <http://127.0.0.1:58123/>. See [Quickstart](quickstart.md) for environment preparation, model verification, and success criteria. Use `echo-agent run` for terminal-only work. Do not start two full Runtimes on one workspace.

## After first use

Complete a [first workflow](../guides/first-workflow.md), then configure [models](../guides/models/index.md), [channels](../integrations/channels/index.md), or [background hosting](../operations/background-service.md). Read [Upgrade and uninstall](upgrade-uninstall.md) before replacing versions or deleting data.
