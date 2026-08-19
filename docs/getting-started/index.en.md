# Getting Started

This section guides you through installing and running Echo Agent from scratch.

---

## Contents

| Section | Description |
|---------|-------------|
| [Installation](installation.en.md) | System requirements, install methods, dependencies |
| [Quickstart](quickstart.en.md) | Your first conversation in 5 minutes |
| [Upgrade & Uninstall](upgrade-uninstall.en.md) | Version upgrades, data migration, full removal |

---

## Overview

Installing Echo Agent is straightforward:

```bash
pip install echo-agent[all]
echo-agent setup
echo-agent run
```

Three commands to launch an AI Agent with memory and skills. The `setup` wizard walks you through configuring your model API key and basic parameters.

!!! tip "Recommended Environment"
    Linux or macOS is recommended for production. Windows users should prefer WSL2, though native Windows is also supported.
