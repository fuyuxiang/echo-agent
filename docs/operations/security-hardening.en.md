# Security Hardening

A checklist-driven guide to securing Echo Agent deployments.

---

## Security Profiles

Echo Agent provides three built-in security profiles:

| Profile | Use Case | Default Tool Access |
|---------|----------|-------------------|
| `personal_cli` | Local development | Most tools allowed |
| `daemon` | Background service | Blocks exec, process, skill_install |
| `public_gateway` | Public-facing | Blocks all high-risk tools |

```yaml
security:
  profile: daemon  # or personal_cli, public_gateway
```

## Hardening Checklist

### Network

- [ ] Gateway binds to `127.0.0.1` (default)
- [ ] Use reverse proxy with TLS for external access
- [ ] Set `gateway.auth.allowedOrigins` to your domain
- [ ] Set `gateway.auth.allowedHosts` for DNS rebinding protection
- [ ] Review `gateway.mediaAllowPrivateAddresses: false`

### Authentication

- [ ] Set `gateway.auth.mode: token`
- [ ] Configure strong `api_tokens` and `admin_tokens`
- [ ] Use separate admin tokens for Dashboard management
- [ ] Review `gateway.auth.allowedUsers` for user restriction

### Tool Permissions

- [ ] Set appropriate `tools.profile`
- [ ] Review `permissions.approval.mode` (auto/ask/deny)
- [ ] Restrict file system paths
- [ ] Disable unnecessary execution backends

### Credentials

- [ ] API keys via environment variables, not config files
- [ ] Enable credential encryption if supported
- [ ] Review config file permissions (600)

### Monitoring

- [ ] Enable audit logging
- [ ] Monitor failed auth attempts (lockout after 5 failures)
- [ ] Set up cost budget alerts
- [ ] Regular review of Gateway logs

## Common Mistakes

!!! danger
    - Running Gateway on `0.0.0.0` without authentication
    - Using `personal_cli` profile for public-facing deployments
    - Storing API keys in version-controlled config files
    - Enabling `shell` tool in public gateway mode
