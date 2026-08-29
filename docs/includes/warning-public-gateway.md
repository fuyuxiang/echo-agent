!!! danger "公网暴露警告"

    Gateway 默认仅监听 `127.0.0.1`。如果你需要从外部访问，**必须**配置以下安全措施：

    1. 使用反向代理（Nginx/Caddy）并启用 TLS
    2. 设置 `gateway.auth.mode: allowlist`，并配置强随机 `api_tokens` / `admin_tokens`
    3. 将 `gateway.auth.allowed_origins` 与 `allowed_hosts` 限制到真实域名
    4. 设置 `security.profile: public_gateway`，并按需收紧 `tools.profile`
    5. 在防火墙或反向代理层限制来源网段和请求频率

    详见[安全加固](../operations/security-hardening.md)和 [Gateway 认证](../integrations/gateway/authentication.md)。
