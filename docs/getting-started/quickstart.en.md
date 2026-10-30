# Runtime quickstart

**Scope: standalone Python Runtime.** Prepare the environment, configure a model, send a first request, and optionally connect a terminal. Desktop installer users should follow the [Desktop quickstart](../desktop/quickstart.md).

## 1. Install in a virtual environment

Prepare Python 3.11+, a reachable model service, and its required credentials. Downloads and binary dependencies vary by platform.

=== "Linux / macOS / WSL2"

    ```bash
    python3 -m venv .venv
    source .venv/bin/activate
    python -m pip install "echo-agent[all]"
    echo-agent --version
    ```

=== "Windows PowerShell"

    ```powershell
    python -m venv .venv
    .\.venv\Scripts\Activate.ps1
    python -m pip install "echo-agent[openai,anthropic,tui]"
    echo-agent --version
    ```

`all` is a predefined collection, not every extra. The Windows example selects a smaller set; add other extras as needed. Prefer WSL2 if native binary dependencies block installation. See [Installation](installation.md).

## 2. Configure and verify a model

```bash
echo-agent setup --flow quickstart
echo-agent config validate
```

Setup guides language, provider, endpoint, credentials, model selection, and necessary settings. Use model IDs published or returned by your service. Verify the connection and model before enabling integrations.

For this exercise, save configuration without registering a background service. The wizard reports the actual saved location. Use `-c` and `-w` for dedicated configuration and workspace paths, keeping them consistent across commands. See [Filesystem layout](../reference/filesystem-layout.md).

Validation establishes parseable configuration, not network connectivity or tool support. Verify an actual request next.

## 3. Start Gateway

```bash
echo-agent gateway
```

Keep the terminal open and visit <http://127.0.0.1:58123/>. If setup already started a Gateway, use that instance. For custom ports, use startup logs.

Release wheels include Dashboard assets. Source installations need a frontend build; otherwise the root may show the simpler playground. See [Dashboard](../guides/dashboard.md).

If authentication is enabled, supply the configured API or admin token for the operation. Keep admin tokens out of public URLs.

## 4. Send a first request

Open the Gateway conversation page at <http://127.0.0.1:58123/playground> and send:

> Explain what you can do in three sentences. Do not read or write files or call external tools yet.

Success means a complete relevant answer without model authentication or connectivity errors. In the same session, ask it to shorten the second sentence to verify continuity.

## 5. Connect a terminal if needed

With Gateway running, open another terminal and activate the same environment:

```bash
echo-agent cli
```

The default is inline interaction. With the `tui` extra installed, use:

```bash
echo-agent cli --tui
```

CLI/TUI attaches over WebSocket without starting another engine. For local terminal-only work, stop Gateway first and run `echo-agent run`. Its configuration controls optional Gateway exposure; noninteractive stdin disables the local CLI channel.

## 6. Verify and stop

```bash
echo-agent status
echo-agent cost
```

State and usage help inspection. Cost is an estimate based on configured prices, separate from provider billing. Stop a foreground Gateway with `Ctrl+C` and wait for cleanup. See [Background services](../operations/background-service.md) for managed shutdown.

| Problem | Check first |
| --- | --- |
| Command unavailable | Activate the installation environment |
| Authentication failure | Provider, key, model ID |
| Valid configuration but failed request | Network, endpoint, model test |
| Page unavailable | Startup logs, address, port |
| CLI connection failure | Running Gateway, matching port and token |

## Next steps

Follow the [first workflow](../guides/first-workflow.md), then review [permissions](../guides/tools-permissions.md), [memory](../guides/memory-management.md), and [persistent hosting](../operations/background-service.md). Long-term recall is separate from conversation history; a single remembered sentence after restart is not sufficient verification.
