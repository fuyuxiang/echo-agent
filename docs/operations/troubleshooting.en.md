# Troubleshooting

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

Symptom-based troubleshooting guide. Find your problem below.

---

## Installation & Startup

### `echo-agent: command not found`

| Check | Command | Fix |
|-------|---------|-----|
| Installed? | `pip show echo-agent` | `pip install echo-agent` |
| PATH? | `python -m echo_agent --version` | Add pip scripts dir to PATH |
| Venv active? | `which python` | Activate your virtualenv |

### Setup wizard fails to validate model

- Check API key is correct
- Check network connectivity to provider endpoint
- If behind proxy, set `HTTP_PROXY`/`HTTPS_PROXY`
- Try: `echo-agent config validate`

## Gateway

### Port already in use

```bash
echo-agent gateway status  # check if already running
# Or use a different port:
# gateway.port: 0  (auto-assign)
```

### Dashboard returns 401/403

- Verify token in request matches `gateway.auth.apiTokens`
- Admin operations require `adminTokens`, not `apiTokens`
- Pairing verification has a separate five-failure lockout; it does not explain an ordinary token mismatch.

### Dashboard blank page

- Ensure Dashboard is built: `echo-agent dashboard build`
- Check browser console for errors
- Verify Gateway is running: `curl http://127.0.0.1:58123/api/v1/health`

## Channels

### Channel not receiving messages

1. Check channel is enabled: `echo-agent status`
2. Verify credentials in config
3. Check `allowFrom` list (empty = allow all)
4. Review Gateway logs for connection errors

### Duplicate messages

- Check for multiple agent instances running
- Verify deduplication (some channels handle this internally)

## Memory & Knowledge

### Memory not recalling relevant information

- Check `memory.enabled: true`
- Verify vector store is working: embedding model available?
- Check `retrievalTopK` is not set too low
- Review memory via Dashboard Memory page

### FAISS/embedding unavailable

- Install vector extra: `pip install "echo-agent[vector]"`
- Falls back to BM25 text search without vector support

## Tools

### Tool stuck waiting for approval

- Review `permissions.approval.mode` (`manual`, `smart`, or `off`) and `unattended_policy` before changing an unattended deployment.
- Or approve through the terminal client's approval prompt.
- Check `tools.profile` allows the tool

## Service

### Service won't start after upgrade

Inspect `echo-agent gateway logs` and run `echo-agent config validate`. SQLite schema migrations run automatically during database initialization. `echo-agent migrate` handles USER memory data, not startup schema changes. If the installed version cannot open existing data, restore a compatible [pre-upgrade backup](backup-restore.en.md) after stopping the service.

## Getting Help

If none of the above resolves your issue:

1. Collect: version, OS, config (redacted), full error log
2. Search [existing issues](https://github.com/fuyuxiang/echo-agent/issues)
3. Open a new issue with the bug report template
