# 部署方案

源码部署工程位于 `echo-agent-runtime/`，包括 Dashboard 的完整 `web/`、内置技能及 `deploy/` 模板，可以单独下载。纯源码需先构建前端；正式 Runtime 压缩包和 Python 安装包已附带前端产物。独立安装步骤见 [Runtime README](https://github.com/fuyuxiang/echo-agent/blob/master/echo-agent-runtime/README.md)。

Echo Agent 可前台运行，也可将 Gateway 注册为系统服务。默认工作区为 `~/.echo-agent`；实际配置和数据位置以 `echo-agent config dump` 为准。

## 本机运行

```bash
pip install "echo-agent[all]"
echo-agent setup
echo-agent run
```

`echo-agent run` 启动完整 Agent；仅当 `gateway.enabled: true` 时同时启动 Gateway。若只需单独启动 Gateway，可运行 `echo-agent gateway`（前台）；用户级后台服务可用 `echo-agent gateway install` 和 `echo-agent gateway start` 管理。服务配置、日志及平台差异见[后台服务](background-service.md)。

默认 Gateway 地址为 `127.0.0.1:58123`，健康检查为 `http://127.0.0.1:58123/api/v1/health`。也可以通过 `--config`、`--workspace` 或对应配置项指定独立的配置和工作区。配置文件搜索顺序与工作区主要目录见[文件系统布局](../reference/filesystem-layout.md)。

## Docker 示例

仓库不提供预构建 Docker 镜像。以下示例需要自行保存为 `Dockerfile` 和 Compose 文件。容器中的 Gateway 必须监听 `0.0.0.0` 才能接收转发流量；绑定非回环地址时必须配置 API 令牌，否则 Gateway 拒绝启动。

```dockerfile
FROM python:3.11-slim
RUN pip install "echo-agent[all]"
EXPOSE 58123
ENTRYPOINT ["echo-agent", "gateway", "--workspace", "/data/echo-agent", "--config", "/data/echo-agent/echo-agent.yaml"]
```

将配置文件保存为 `./echo-agent.yaml`。下面的令牌仅为占位示例，实际部署应使用随机生成的值，并限制该文件的访问权限：

```yaml
workspace: /data/echo-agent
gateway:
  host: "0.0.0.0"
  port: 58123
  auth:
    mode: allowlist
    api_tokens: ["replace-with-a-random-token"]
    allowed_users: ["cli:local"]  # 默认 CLI 用户；其他调用方按实际身份添加
    allowed_hosts: ["localhost", "127.0.0.1"]
```

```yaml
services:
  echo-agent:
    build: .
    ports:
      - "127.0.0.1:58123:58123"
    volumes:
      - echo-agent-data:/data/echo-agent
      - ./echo-agent.yaml:/data/echo-agent/echo-agent.yaml:ro
    restart: unless-stopped

volumes:
  echo-agent-data:
```

启动后可运行 `docker compose up -d`、`docker compose logs -f echo-agent`。端口只映射到宿主机回环地址；若通过域名对外提供服务，在宿主机部署反向代理并设置 `gateway.auth.allowed_hosts` 为该域名。容器内工具执行会使用容器中的文件和进程环境；需要访问项目文件时，应显式挂载相应路径，并按工具配置限制访问范围。

## 反向代理

Gateway 本身提供 HTTP 与 WebSocket。若需要 HTTPS、域名或代理层访问控制，使用 [Gateway 反向代理配置](../integrations/gateway/reverse-proxy.md) 中的 nginx 或 Caddy 示例。代理应转发原始 `Host` 和认证头，并支持 WebSocket 升级。健康探针的默认路径为 `/api/v1/health`。

## 多实例

各实例使用独立的配置文件、工作区、监听端口和服务标识。不要让多个 Agent 进程写同一工作区的 SQLite 数据库与记忆文件，也不要把当前示例直接当作共享数据库的负载均衡方案。例如，分别以前台启动两个实例：

```bash
echo-agent gateway --config /srv/agent-a/echo-agent.yaml --workspace /srv/agent-a
echo-agent gateway --config /srv/agent-b/echo-agent.yaml --workspace /srv/agent-b
```

两个配置中的 `gateway.port` 必须不同。实际生产环境还需分别规划令牌、数据备份和进程管理。

## 相关文档

- [Gateway 认证](../integrations/gateway/authentication.md)
- [备份与恢复](backup-restore.md)
- [安全加固](security-hardening.md)

## 无桌面服务器：从安装到第一个工具任务

运行已打包 wheel 只需 Python 3.11+ 及所选 provider 依赖。Dashboard、简化交互页、内置 skills 和配置资源随正式包安装；运行不需要源码 `web/`、Node/pnpm、Rust/Tauri 或 `client/`。源码构建机则先用 Node 24 LTS 和项目锁定的 pnpm 构建 `web/`，再执行 `python -m build`。不要用允许缺 Dashboard 的构建开关制作正式包。

以下在独立测试工作区运行。将 wheel、模型名和令牌替换为自己的值，模型密钥放环境变量，不写入 shell 历史或 YAML：

```bash
python3.11 -m venv /srv/echo-agent/venv
/srv/echo-agent/venv/bin/python -m pip install '/absolute/path/echo_agent-0.3.8-py3-none-any.whl[openai]'
/srv/echo-agent/venv/bin/python -m pip check
mkdir -p /srv/echo-agent/workspace
```

保存 `/srv/echo-agent/echo-agent.yaml`（限制为当前服务用户可读，例如 `chmod 600`）：

```yaml
workspace: /srv/echo-agent/workspace
security:
  profile: daemon
models:
  default_model: YOUR_MODEL_ID
  providers:
    - name: openai
      api_key_env: OPENAI_API_KEY
      models: [YOUR_MODEL_ID]
      # 自托管兼容API按服务要求填写完整前缀，例如 http://127.0.0.1:8000/v1
      # api_base: http://127.0.0.1:8000/v1
gateway:
  host: 127.0.0.1
  port: 58123
  auth:
    mode: allowlist
    api_tokens: [REPLACE_WITH_RANDOM_API_TOKEN]
    admin_tokens: [REPLACE_WITH_DIFFERENT_RANDOM_ADMIN_TOKEN]
    allowed_users: ["api:operator", "cli:local", "ws:local"]
# 首个任务只需只读文件工具，无需shell或自动批准。
tools:
  profile: coding
  restrict_to_workspace: true
```

先在前台验证配置和运行，再交给服务管理器。确保当前进程已通过受保护的环境加载 `OPENAI_API_KEY`；自建无密钥兼容端点可以省略 key，必须显式设置 `api_base`。

```bash
/srv/echo-agent/venv/bin/echo-agent config validate --config /srv/echo-agent/echo-agent.yaml
/srv/echo-agent/venv/bin/echo-agent gateway --config /srv/echo-agent/echo-agent.yaml --workspace /srv/echo-agent/workspace
```

另一个服务器 shell 中检查并发起任务；先将 API token 安全放入当前 shell 的 `ECHO_API_TOKEN` 环境变量：

```bash
curl --fail http://127.0.0.1:58123/api/v1/health
curl --fail http://127.0.0.1:58123/api/v1/ready
curl --fail-with-body http://127.0.0.1:58123/api/v1/message \
  -H "Authorization: Bearer $ECHO_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: first-tool-task-001' \
  --data '{"platform":"api","user_id":"operator","chat_id":"operator","text":"请实际调用 list_dir 列出当前工作区目录，并根据工具结果回答。","wait":true,"timeout_seconds":120}'
```

`status: completed` 与工具记录共同证明任务执行；模型未配置或任务不完整会返回对应失败/不完整状态。重复相同幂等键和相同请求可查询原执行，不能换内容复用键。`health` 表示服务可响应；`ready` 还检查 Agent、bus、SQLite、provider配置与必需运行依赖，**不主动验证远端密钥/额度/模型可用性**。真实模型是否完成调用仍以该任务的工具记录与最终结果为准。

通过 SSH 端口转发访问 Dashboard，或访问 `/playground` 使用带 token 的简化交互页；简化页从 `/meta` 发现实际 API/WS 路径，不需要关闭认证。管理 token 用于看板写操作、配置、会话管理等；不要把它提供给只需要提交任务的调用方。配置页面显示保存值和运行值，刷新仍保留待重启字段；普通保存不热更新 Runtime。渠道的显式启停/重启是局部生效入口，其余设置重启后再检查 `/ready`。

工具审批与看板任务 review 是两种操作。需要审批/澄清的任务可用支持的交互 WS 或 `echo-agent cli --config ... --workspace ...` 在服务器上接入；通过同一授权会话的 `/approvals`、`/approve`、`/deny`、`/clarify` 完成，不依赖桌面应用。无人接收、拒绝、过期、取消应按状态反馈处理，不用自动批准绕过。

需要持久文件产物时，使用 `artifact_create → artifact_append → artifact_finalize → artifact_deliver`，按返回的 ID/校验结果继续。无附件能力的HTTP/WS入口可接收编号文本块；HTTP等待响应中的 `reply.tool_deliveries` 保存工具投递，`reply.text` 保存最终回答。文件已生成、业务完成和外部平台确认发送不能混为一个成功。

前台通过 SIGTERM 正常关闭后，再按[后台服务](background-service.md)安装用户级 systemd 服务。服务进程的环境变量与SSH shell不同，应给服务用户配置受保护的环境文件或服务环境；配置文件继续使用明确绝对路径。重复工作区 Runtime 或端口冲突退出非零。停止/启用失败不会再打印假成功。

本次真实进程与浏览器验收是在macOS隔离环境进行；Linux包安装进程检查已接入CI，但没有可用Docker daemon/隔离Linux主机供本机实际执行。现有Docker示例保留为部署示例，不是本次新增或已构建通过的容器交付物。完整证据和边界见[服务端评审](../reviews/server-review-and-remediation.md)。
