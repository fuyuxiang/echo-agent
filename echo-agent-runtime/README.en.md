[中文](README.md) · [English](README.en.md)

# Echo Agent Runtime

A standalone Python Agent Runtime containing the CLI, Gateway, Dashboard, bundled skills, configuration resources and deployment templates. This directory can be downloaded, built and installed without `client/`, `docs/` or any parent-repository files. The distribution and command remain `echo-agent`; the Python import remains `echo_agent`.

## Install from source

Requires Python 3.11+. Building the Dashboard also requires Node.js 22/24 and pnpm, pinned in `web/package.json`. Configure a model service and any external tools required by your selected skills.

Enter `echo-agent-runtime/` if you downloaded the whole repository. If you downloaded only this directory, run these commands directly inside it:

```bash
python3 -m venv .venv
source .venv/bin/activate
(cd web && pnpm install --frozen-lockfile && pnpm build)
python -m pip install ".[openai]"
python -m pip check
echo-agent setup
echo-agent gateway
```

Replace `[openai]` with `[anthropic]`, `[gemini]`, `[bedrock]` or `[all]` as appropriate. The Gateway serves the Dashboard at `/` and the lightweight interaction page at `/playground`, by default at `http://127.0.0.1:58123/`. Configure models, tokens and tool permissions before use.

For development, install with `python -m pip install -e ".[all,dev]"`, build the frontend and start the Gateway. `echo-agent dashboard build` rebuilds the source Dashboard.

In Windows PowerShell activate `.venv\Scripts\Activate.ps1` and run the frontend commands separately inside `web/`. Service registration supports Linux/systemd and macOS/launchd; Windows users can use WSL2.

## Install a Runtime release archive

`echo-agent-runtime-<version>.tar.gz` contains the complete sources and prebuilt `web/dist/`. Extract it, enter `echo-agent-runtime/`, create the Python environment and install as above, **skipping the frontend build**. The server needs no Node/pnpm. Git source archives require a frontend build.

Wheels and Python sdists also bundle the Dashboard and skills. The sdist retains frontend sources, deployment templates and tests; rebuilding its wheel needs no Node. Ordinary `pip install .` rejects sources missing Dashboard assets. Do not use `ECHO_ALLOW_NO_DASHBOARD` to produce deployment packages.

This is not an offline dependency bundle: Python dependencies, local models and external tools must still be prepared or downloaded.

## Linux / macOS / WSL2 installer

```bash
# Local sources; no Git clone/update. Creates venv/ here by default.
bash scripts/install.sh --source .

# Put the virtual environment elsewhere; keep the source tree in place.
bash scripts/install.sh --source . --dir /srv/echo-agent
```

The installer prepares dependencies and the Dashboard before configuration and service setup. Existing valid model configuration is preserved. Use `--reconfigure` to replace it, `--skip-setup` to skip the wizard, or `--skip-dashboard` to defer the build until `echo-agent dashboard build`. See `bash scripts/install.sh --help`.

An installer downloaded separately obtains a sparse Runtime checkout without desktop sources. Running it directly from a Runtime tree automatically selects local installation.

## Server configuration and data

Copy `deploy/echo-agent.yaml.example` to your configuration file. Set the model and distinct random API/admin tokens, restrict file permissions, and supply the model key through the environment variable named by `api_key_env`.

```bash
echo-agent config validate --config /absolute/path/echo-agent.yaml
echo-agent gateway --config /absolute/path/echo-agent.yaml --workspace /srv/echo-agent/workspace
```

Verify foreground operation before registering a service with `echo-agent gateway install`. The service needs the same config, workspace and model-key environment. Use SSH forwarding for remote access, or configure the appropriate domain and Host/Origin rules for a reverse proxy.

Keep production data separate from the source tree. The default workspace is `~/.echo-agent/`. Databases, logs, user skills, model caches and credentials must stay out of release archives. `data/eval/` contains versioned evaluation examples. Back up the actual workspace and any configured external data paths before upgrades.

Container templates: [deploy/README.en.md](deploy/README.en.md).

## Build and validate

```bash
python -m pip install build
bash scripts/build.sh
```

The local `dist/` contains a wheel, a Python sdist and `echo-agent-runtime-<version>.tar.gz`. The script builds the frontend, builds Python packages, generates the standalone source archive, and verifies Dashboard assets, skills, config and third-party licenses. Use `bash scripts/build.sh --prebuilt-dashboard` with existing trusted assets.

```bash
python -m pip install -e ".[all,dev]"
ruff check .
python -m pytest tests/
(cd web && pnpm test --run)
```

`scripts/publish.sh` additionally requires `twine` and explicitly uploads the wheel/sdist to PyPI. Building alone never publishes. CI builds outside the repository and exercises extracted-source installation and real Gateway processes.

[Documentation](https://fuyuxiang.github.io/echo-agent/en/) · [Changelog](CHANGELOG.md) · [MIT License](LICENSE). Bundled FastEmbed provenance and licensing: [VENDOR.md](echo_agent/_vendor/fastembed/VENDOR.md).
