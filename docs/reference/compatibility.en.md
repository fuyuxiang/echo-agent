# Compatibility Reference

This page documents platform support, version requirements, and known limitations for Echo Agent v0.3.7 Beta.

---

## Python Version Support

`pyproject.toml` declares `requires-python = ">=3.11"`.

| Python Version | Status | Notes |
|---------------|--------|-------|
| 3.10 and below | Not supported | Below `requires-python`; pip refuses to install |
| 3.11 | Supported | Minimum required version, covered by CI |
| 3.12 | Supported | Covered by CI |
| 3.13 and above | Unverified | Installs, since it satisfies `requires-python`, but is not in the CI matrix and is not declared in the PyPI classifiers |

"Supported" here means CI runs the full test suite on that version. The matrix currently covers 3.11 and 3.12; running on anything newer is possible but unverified, and issue reports are welcome.

!!! warning "Python 3.11 minimum"
    Echo Agent uses `TaskGroup`, `ExceptionGroup`, and other features introduced in Python 3.11. Earlier versions will fail at import time.

---

## Operating System Support

| OS | Version | Status | Notes |
|----|---------|--------|-------|
| Ubuntu | 22.04+ | Fully supported | Primary development platform |
| Debian | 12+ (Bookworm) | Fully supported | — |
| Fedora | 38+ | Supported | — |
| Arch Linux | Rolling | Supported | Use latest Python |
| RHEL / Rocky | 9+ | Supported | May need Python from AppStream |
| macOS | 13+ (Ventura) | Fully supported | ARM64 and x86_64 |
| macOS | 12 (Monterey) | Partial | No longer tested in CI |
| Windows 11 | WSL2 | Fully supported | Recommended Windows method |
| Windows 11 | Native | Partial | Reduced tool functionality |
| Windows 10 | WSL2 | Supported | Requires WSL2 update |
| Windows 10 | Native | Not recommended | Limited shell/process tools |

### Linux Requirements

- glibc 2.35+ (Ubuntu 22.04, Debian 12, Fedora 38)
- SQLite 3.37+ (included with Python)
- OpenSSL 3.0+

### macOS Requirements

- Xcode Command Line Tools (for some optional dependencies)
- Homebrew recommended for Python installation

### Windows (WSL2)

```powershell
# Install WSL2 with Ubuntu
wsl --install -d Ubuntu-22.04

# Inside WSL2
sudo apt update && sudo apt install python3.12 python3.12-venv
```

!!! tip "Windows native limitations"
    On native Windows (without WSL2):
    
    - `shell` tool uses PowerShell instead of bash
    - `process` tool has limited signal support (no SIGUSR1, SIGHUP)
    - File paths use backslashes (handled automatically)
    - Some plugins may not support Windows paths
    - Cron jobs use Windows Task Scheduler instead of crontab

---

## LLM Provider Compatibility

| Provider | Status | Models Tested | Notes |
|----------|--------|---------------|-------|
| Anthropic | Fully supported | Claude Sonnet 4, Claude Opus 4, Claude Haiku 3.5 | Primary provider |
| OpenAI | Supported | GPT-4o, GPT-4 Turbo, o1, o3 | Full tool use support |
| Google | Supported | Gemini 2.5 Pro, Gemini 2.5 Flash | Via google-genai SDK |
| Azure OpenAI | Supported | GPT-4o (Azure deployment) | Requires base_url config |
| AWS Bedrock | Supported | Claude via Bedrock | Requires AWS credentials |
| Ollama | Supported | Llama 3, Mistral, Qwen | Local inference |
| OpenRouter | Supported | Various | Via OpenAI-compatible API |
| LiteLLM | Supported | Proxy to 100+ models | Via OpenAI-compatible API |
| Together AI | Supported | Llama, Mixtral | Via OpenAI-compatible API |
| Groq | Supported | Llama, Mixtral | Via OpenAI-compatible API |

!!! warning "Tool use compatibility"
    Not all providers support function calling / tool use equally. Providers with limited tool support may fall back to prompt-based tool invocation, which is less reliable. Anthropic and OpenAI provide the best tool use experience.

### Provider Configuration

Providers are a **list** under `models.providers`, each entry naming a provider and the models it serves. There is no per-alias mapping such as `models.default` or `models.azure`:

```yaml
models:
  default_model: claude-sonnet-4-20250514
  fallback_model: gpt-4o-mini
  providers:
    - name: anthropic
      api_key: sk-ant-...
      models: ["claude-sonnet-4-20250514"]

    # Local model via Ollama
    - name: ollama
      api_base: http://localhost:11434
      models: ["llama3:70b"]

    # Azure OpenAI
    - name: azure
      api_key: ...
      api_base: https://myinstance.openai.azure.com/
      models: ["gpt-4o"]
```

The connection field is `api_base`, not `base_url`. Which model is used comes from `default_model` plus the `models.routes` rules, not from an alias key.

---

## Channel Compatibility by Platform

| Channel | Linux | macOS | Windows (WSL2) | Windows (Native) |
|---------|-------|-------|----------------|------------------|
| CLI | Yes | Yes | Yes | Yes |
| Telegram | Yes | Yes | Yes | Yes |
| Discord | Yes | Yes | Yes | Yes |
| Slack | Yes | Yes | Yes | Yes |
| WhatsApp | Yes | Yes | Yes | Yes |
| Webhook | Yes | Yes | Yes | Yes |
| Weixin | Yes | Yes | Yes | Partial |
| QQ Bot | Yes | Yes | Yes | Partial |
| Feishu | Yes | Yes | Yes | Yes |
| DingTalk | Yes | Yes | Yes | Yes |
| Cron | Yes | Yes | Yes | Limited |

!!! tip "Network channels"
    All network-based channels (Telegram, Discord, Slack, etc.) work identically across platforms since they use outbound HTTPS connections. Platform differences only affect local tools and the gateway.

---

## Database Requirements

| Component | Minimum | Recommended | Notes |
|-----------|---------|-------------|-------|
| SQLite | 3.37 | 3.42+ | Bundled with Python; WAL mode required |
| SQLite extensions | — | — | FTS5 for full-text search (usually included) |

Echo Agent uses SQLite exclusively for data storage. No external database server is required.

### SQLite WAL Mode

WAL (Write-Ahead Logging) is required for concurrent read performance:

```python
# Verified automatically at startup
PRAGMA journal_mode=WAL;
```

!!! warning "Network filesystems"
    SQLite does not work reliably on NFS, CIFS/SMB, or other network filesystems. Store `data/echo_agent.db` on local disk.

---

## Optional Dependencies

| Dependency | Purpose | Required For |
|------------|---------|--------------|
| `chromium` / `chrome` | Browser tool rendering | `browser` tool with JS rendering |
| `ffmpeg` | Audio/video processing | `tts` tool, voice messages |
| `pandoc` | Document conversion | `document` tool PDF/DOCX export |
| `git` | Version control | `shell` tool git operations |
| `node` (18+) | JavaScript execution | `code_exec` with language: javascript |
| `docker` | Container isolation | Sandboxed code execution |

Install optional dependencies:

```bash
# Ubuntu/Debian
sudo apt install chromium-browser ffmpeg pandoc git nodejs

# macOS
brew install chromium ffmpeg pandoc git node

# Check what's available
echo-agent deps status
```

---

## Hardware Recommendations

### Minimum Requirements

| Resource | Minimum | Notes |
|----------|---------|-------|
| RAM | 512 MB | CLI-only, single session |
| Disk | 200 MB | Base install without data |
| CPU | 1 core | Adequate for single user |
| Network | 1 Mbps | For LLM API calls |

### Recommended (Multi-Channel)

| Resource | Recommended | Notes |
|----------|-------------|-------|
| RAM | 2 GB | Multiple channels + knowledge base |
| Disk | 5 GB | Data storage, logs, knowledge index |
| CPU | 2+ cores | Concurrent sessions |
| Network | 10 Mbps | Media handling |

### Production (Gateway Exposed)

| Resource | Production | Notes |
|----------|-----------|-------|
| RAM | 4+ GB | Multiple concurrent users |
| Disk | 20+ GB | Full logging, large knowledge base |
| CPU | 4+ cores | Parallel request handling |
| Network | 100 Mbps | Multiple media streams |

!!! tip "Memory usage"
    Memory consumption scales primarily with:
    
    - Number of concurrent sessions
    - Knowledge base size (vector index in memory)
    - Browser tool usage (headless Chrome)
    - Spill cache size

---

## Container Support

### Docker

```dockerfile
FROM python:3.12-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    git ffmpeg && \
    rm -rf /var/lib/apt/lists/*

RUN pip install echo-agent==0.3.7

EXPOSE 3000
VOLUME /root/.echo-agent/data

CMD ["echo-agent", "gateway", "foreground"]
```

| Container Runtime | Status | Notes |
|-------------------|--------|-------|
| Docker | Supported | Recommended container runtime |
| Podman | Supported | Rootless mode works |
| containerd | Supported | Via nerdctl or Kubernetes |
| Docker Compose | Supported | Multi-service deployments |

### Kubernetes

Echo Agent can run as a Deployment or StatefulSet. Use a PersistentVolumeClaim for `data/`:

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: echo-agent
spec:
  replicas: 1  # Single writer only
  template:
    spec:
      containers:
        - name: echo-agent
          image: echo-agent:0.3.7
          ports:
            - containerPort: 3000
          volumeMounts:
            - name: data
              mountPath: /root/.echo-agent/data
          env:
            - name: ECHO_AGENT_GATEWAY__HOST
              value: "0.0.0.0"
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: echo-agent-data
```

!!! danger "Single writer constraint"
    Do not scale Echo Agent beyond 1 replica. SQLite requires single-writer access. For horizontal scaling, use separate instances with isolated data directories.

---

## Network Requirements

| Direction | Port | Protocol | Purpose |
|-----------|------|----------|---------|
| Outbound | 443 | HTTPS | LLM API calls, channel APIs |
| Outbound | 80/443 | HTTP/S | Web/browser tools |
| Inbound | 3000 (configurable) | HTTP/WS | Gateway (if exposed) |
| Inbound | 8080-8083 (configurable) | HTTP | Webhook channels |
| Localhost | 11434 | HTTP | Ollama (if used) |

### Firewall Rules

For minimal operation (CLI-only, outbound LLM):

```bash
# Only outbound HTTPS needed
iptables -A OUTPUT -p tcp --dport 443 -j ACCEPT
```

For gateway exposure:

```bash
# Allow inbound to gateway port
iptables -A INPUT -p tcp --dport 3000 -j ACCEPT
```

---

## Known Limitations

| Area | Limitation | Workaround |
|------|-----------|------------|
| Windows native | Shell tool limited to PowerShell | Use WSL2 |
| Windows native | No Unix signals for process tool | Use WSL2 |
| NFS/SMB storage | SQLite corruption risk | Use local disk |
| ARM32 | Not tested | Use ARM64 or x86_64 |
| Python 3.10 | Not supported | Upgrade to 3.11+ |
| Concurrent writes | Single writer only | Do not run multiple instances |
| Knowledge base | Large indexes (>10GB) slow to load | Split into workspaces |
| Browser tool | Requires system Chromium | Install chromium package |

!!! note "Knowledge base scaling characteristics"
    No maximum size is enforced, and no benchmarked ceiling is published — the practical limit depends on your hardware, so treat the shape of the cost rather than a specific number as the guidance.

    The index is `IndexFlatIP`, an exact search that compares the query against every stored vector, so query time grows linearly with chunk count and the whole matrix is held in memory. There is no approximate index (IVF, HNSW) to keep latency flat as the corpus grows.

    Consequently a large corpus is better handled by indexing less: narrow `knowledge.allowedExtensions` and the documents directory to what the agent actually needs to consult, rather than pointing it at an entire archive. `knowledge.chunkSize` also trades directly against vector count — smaller chunks mean more precise hits and more vectors to compare.
