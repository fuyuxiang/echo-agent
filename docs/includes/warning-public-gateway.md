!!! danger "公网暴露警告"

    Gateway 默认仅监听 `127.0.0.1`。如果你需要从外部访问，**必须**配置以下安全措施：

    1. 使用反向代理（Nginx/Caddy）并启用 TLS
    2. 设置 `gateway.auth.mode` 为 `token` 并配置强 API Token
    3. 限制 `gateway.auth.allowedOrigins` 为你的域名
    4. 考虑启用 `security.profile: strict`

    详见[安全加固](../operations/security-hardening.md)和 [Gateway 认证](../integrations/gateway/authentication.md)。
