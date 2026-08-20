# 部署方案

本文介绍 Echo Agent 在不同环境下的部署拓扑和配置要点。

---

## 部署拓扑一览

| 方案 | 复杂度 | 适用场景 |
|------|--------|---------|
| 单机直接部署 | 低 | 个人使用、小团队 |
| Docker 容器 | 中 | 隔离环境、CI/CD |
| 反向代理 + Gateway | 中 | 需要 HTTPS / 域名访问 |
| 多实例部署 | 高 | 多用户隔离、高可用 |

---

## 单机直接部署

最简方案，适合个人开发者和小团队：

```bash
# 安装
pip install echo-agent[all]
echo-agent setup

# 部署为后台服务
echo-agent gateway install
echo-agent gateway start
```

### 目录结构

```
~/.echo-agent/                 # 全局数据目录
├── config.yaml                # 主配置文件
├── data/
│   ├── echo_agent.db          # SQLite 主数据库
│   ├── memory/                # 记忆存储
│   ├── knowledge/             # 知识库索引
│   ├── spill/                 # 大输出溢写
│   ├── logs/                  # 运行日志
│   └── checkpoints/           # 状态检查点
└── env                        # 环境变量文件（可选）
```

### 工作区数据

除全局目录外，每个项目工作区可有独立数据：

```
./your-project/
└── .echo-agent/               # 工作区级数据
    ├── config.yaml            # 工作区配置覆盖
    └── data/                  # 工作区级记忆/知识
```

---

## Docker 容器部署

### Dockerfile 示例

```dockerfile
FROM python:3.11-slim

RUN pip install echo-agent[all]

# 数据目录挂载点
VOLUME /data/echo-agent

ENV ECHO_AGENT_HOME=/data/echo-agent
ENV ECHO_AGENT_GATEWAY_HOST=0.0.0.0

EXPOSE 58123

ENTRYPOINT ["echo-agent", "gateway", "--foreground"]
```

### docker-compose.yml

```yaml
version: "3.8"
services:
  echo-agent:
    build: .
    ports:
      - "127.0.0.1:58123:58123"
    volumes:
      - echo-agent-data:/data/echo-agent
      - ./config.yaml:/data/echo-agent/config.yaml:ro
    environment:
      - ECHO_AGENT_HOME=/data/echo-agent
    restart: unless-stopped
    mem_limit: 2g

volumes:
  echo-agent-data:
```

### 运行

```bash
docker-compose up -d
docker-compose logs -f echo-agent
```

!!! warning "容器内工具执行"
    容器化部署时，Agent 的工具执行（如 shell 命令）受限于容器环境。需要挂载工作目录或配置远程执行后端。

!!! question "需维护者确认"
    官方是否提供预构建 Docker 镜像？当前需用户自行构建。

---

## 反向代理部署

当需要通过 HTTPS 或域名访问 Gateway 时，推荐在前面放置反向代理。

### Nginx 配置

```nginx
server {
    listen 443 ssl;
    server_name echo-agent.example.com;

    ssl_certificate /etc/ssl/certs/echo-agent.pem;
    ssl_certificate_key /etc/ssl/private/echo-agent.key;

    location / {
        proxy_pass http://127.0.0.1:58123;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # WebSocket 支持（如需要）
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";

        # 超时设置（Agent 任务可能较长）
        proxy_read_timeout 300s;
        proxy_send_timeout 300s;
    }
}
```

### Caddy 配置

```caddyfile
echo-agent.example.com {
    reverse_proxy localhost:58123 {
        # 长连接超时
        transport http {
            read_timeout 300s
        }
    }
}
```

!!! tip "Caddy 自动 HTTPS"
    Caddy 自动申请和续期 Let's Encrypt 证书，是最简单的 HTTPS 部署方案。

### Gateway 配合配置

使用反向代理时，需更新 Gateway 的 Origin 保护配置：

```yaml
gateway:
  host: 127.0.0.1          # 仍然绑定本地
  port: 58123
  auth:
    allowed_origins:
      - "https://echo-agent.example.com"
    allowed_hosts:
      - "echo-agent.example.com"
```

---

## 多实例部署

为不同用户或项目运行独立的 Echo Agent 实例：

### 方案一：不同端口

```yaml
# 实例 A: ~/.echo-agent-alice/config.yaml
gateway:
  port: 58123

# 实例 B: ~/.echo-agent-bob/config.yaml
gateway:
  port: 8421
```

```bash
echo-agent -c ~/.echo-agent-alice gateway start
echo-agent -c ~/.echo-agent-bob gateway start
```

### 方案二：Docker Compose 多实例

```yaml
version: "3.8"
services:
  agent-alice:
    build: .
    ports:
      - "127.0.0.1:58123:58123"
    volumes:
      - alice-data:/data/echo-agent
    environment:
      - ECHO_AGENT_HOME=/data/echo-agent

  agent-bob:
    build: .
    ports:
      - "127.0.0.1:8421:58123"
    volumes:
      - bob-data:/data/echo-agent
    environment:
      - ECHO_AGENT_HOME=/data/echo-agent

volumes:
  alice-data:
  bob-data:
```

!!! danger "数据隔离"
    多实例共享同一数据目录会导致 SQLite 锁冲突和数据损坏。每个实例必须使用独立的数据目录。

---

## 网络安全注意事项

| 部署方式 | 默认绑定 | 公网暴露风险 |
|---------|---------|-------------|
| 直接部署 | `127.0.0.1` | 无 |
| Docker (ports) | 取决于映射 | 注意 `0.0.0.0` 映射 |
| 反向代理 | 代理层控制 | 需配置访问控制 |

!!! danger "切勿将未认证的 Gateway 暴露到公网"
    Gateway 默认绑定 localhost。如果修改为 `0.0.0.0` 并暴露到公网，必须启用 `allowlist` 或 `pairing` 认证模式，并配置防火墙规则。详见 [安全加固](security-hardening.md)。
