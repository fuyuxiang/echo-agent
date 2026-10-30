# Security Hardening

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

For a server, configure identity, API authorization and execution policy separately. A healthy process alone does not establish a secure or model-ready task path.

## Authentication and exposure

```yaml
security:
  profile: daemon

tools:
  profile: coding
  restrict_to_workspace: true

gateway:
  host: 127.0.0.1
  auth:
    mode: allowlist  # open | allowlist | pairing
    allowed_users: ["cli:local"]
    allowed_hosts: ["localhost", "127.0.0.1"]

permissions:
  approval:
    mode: manual  # manual | smart | off
    cli_auto_approve: false
    unattended_policy: deny
```

Set strong, distinct API/admin tokens through a protected configuration file or JSON environment overrides:

```bash
export ECHO_AGENT_GATEWAY__AUTH__API_TOKENS='["replace-with-random-read-token"]'
export ECHO_AGENT_GATEWAY__AUTH__ADMIN_TOKENS='["replace-with-random-admin-token"]'
```

`auth.mode` controls platform/user allowlist/pairing; configured token checks remain independent, including in `open`. Read tokens do not replace admin tokens. `admin_users` is not a bypass for HTTP management-token guards. Pairing authorizes a platform/user pair and does not issue an API token.

Keep the default loopback binding or use SSH forwarding. External exposure needs TLS termination and explicit Host/Origin configuration. A reverse proxy's loopback peer address is not the final user's identity. See [Gateway authentication](../integrations/gateway/authentication.en.md) and [reverse proxy](../integrations/gateway/reverse-proxy.en.md).

## Tool and approval policy

Use the [security profile matrix](../reference/security-profile-matrix.en.md) for the exact allow/deny sets. `daemon` and `public_gateway` deny skill script execution and MCP calls by default as well as process execution. Explicitly allowing a tool does not remove its execution-time path, guard or approval checks. `auto_deny` wins over prior approvals, trusted channels and `mode: off`.

The `coding` profile includes `cronjob`, with approval and per-job unattended authorization still enforced. Set `tools.deny: [cronjob]` to disable the scheduling tool entirely.

Unattended runs must converge without relying on a desktop. Authorize only the intended Cron content/target; DANGEROUS operations cannot self-authorize. Keep a supported server CLI attach available for approval and clarification. Smart Approval rejects unavailable, truncated or filtered judgments.

Shell pattern checks and the copied-workdir `sandbox` backend are not OS isolation. Python plugins are trusted in-process code. A deployment requiring OS boundaries must enforce them in its selected container/remote execution environment. Network policy filters tool capabilities; it is not a firewall for arbitrary shell/SDK traffic.

## Files, credentials and recovery

Protect workspace directories with `0700`, configuration/secrets with `0600`, and keep credentials out of version control and command-line arguments. CredentialManager requires encryption by default. Its key may be supplied through the configured environment variable or persisted in `.credential_key`; preserve that matching key in backups. YAML secrets are not automatically encrypted by CredentialManager.

Back up SQLite consistently with memory files, source documents, skills, configuration and keys. File checkpoints do not include all Runtime state. See [backup and restore](backup-restore.en.md).

## Monitoring

Review authentication and tool audit failures, readiness, task terminal states and delivery recovery. The five-failure lockout applies to pairing verification, not every invalid API token. Cost limits need `cost.enabled` and a nonzero budget. Set network-level ingress limits and alerts in the deployment environment when required; Dashboard does not configure a general resource-alert engine.
