!!! danger "Public Exposure Warning"

    Gateway listens on `127.0.0.1` by default. If you need external access, you **must** configure:

    1. A reverse proxy (Nginx/Caddy) with TLS
    2. `gateway.auth.mode: allowlist` with strong random `api_tokens` / `admin_tokens`
    3. `gateway.auth.allowed_origins` and `allowed_hosts` restricted to the real domain
    4. `security.profile: public_gateway`, with `tools.profile` reduced as needed
    5. Source-network and rate limits at the firewall or reverse-proxy layer

    See [Security Hardening](../operations/security-hardening.md) and [Gateway Authentication](../integrations/gateway/authentication.md).
