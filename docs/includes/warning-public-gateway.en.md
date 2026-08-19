!!! danger "Public Exposure Warning"

    Gateway listens on `127.0.0.1` by default. If you need external access, you **must** configure:

    1. A reverse proxy (Nginx/Caddy) with TLS
    2. `gateway.auth.mode: token` with a strong API token
    3. `gateway.auth.allowedOrigins` restricted to your domain
    4. Consider enabling `security.profile: strict`

    See [Security Hardening](../operations/security-hardening.md) and [Gateway Authentication](../integrations/gateway/authentication.md).
