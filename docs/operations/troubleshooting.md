# 故障排查

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

先确认实际加载的配置、工作区与启动方式：

```bash
echo-agent --version
echo-agent config validate
echo-agent config explain
echo-agent status
```

`echo-agent gateway status` 查看已安装的后台服务状态；前台运行的 `echo-agent run` 或 `echo-agent gateway` 需检查其终端输出。后台日志可用 `echo-agent gateway logs` 或 `echo-agent gateway logs --follow` 查看。对外分享配置和日志前应检查密钥、令牌及私人数据。

## Gateway 无法启动

- 查看启动错误和服务日志，确认监听端口没有被占用。
- 运行 `echo-agent config validate`；检查 `gateway.host`、`gateway.port` 及生效的配置文件。默认监听地址是 `127.0.0.1:58123`。
- 绑定 `0.0.0.0`、`::` 或其他非回环地址时，至少要配置 `gateway.auth.api_tokens`；否则启动会被拒绝。
- 后台服务安装时会固化工作区和配置路径。若路径或 Python 环境改变，重新运行 `echo-agent gateway install` 并重启服务。
- SQLite 表结构迁移在数据库初始化时自动执行；`echo-agent migrate` 不是数据库修复或表结构升级命令。

## CLI 连接失败

```bash
echo-agent gateway status
curl -v http://127.0.0.1:58123/api/v1/health
curl -H 'X-Echo-Agent-Token: your-token' http://127.0.0.1:58123/api/v1/capabilities
```

健康接口可检查端口和进程；`capabilities` 可检查配置了令牌时的 API 认证。若端口被 `gateway.port` 或 `--port` 改写，应同步修改 URL。`echo-agent cli` 连接本机 Gateway；检查 `--port`、`--token` 和正在运行的实例。浏览器请求被拒绝时，还要核对 `gateway.auth.allowed_hosts`、`allowed_origins` 以及代理传来的 Host/Origin。

## 模型请求失败或超时

```bash
ECHO_AGENT_OBSERVABILITY__LOG_LEVEL=DEBUG echo-agent run
echo-agent cost
```

检查模型提供商配置、密钥来源、网络连接和服务端错误信息。调试日志可能含敏感信息。`echo-agent config explain` 可用于定位环境变量对 YAML 的覆盖；配置文件中的 `${VAR}` 不会自动展开。更改超时前，先核对具体模型提供商的配置项与错误类型，避免把鉴权失败当成超时。

## 记忆或知识检索不到内容

确认当前实例使用了预期工作区、用户与会话身份；`storage.memory_dir`、`knowledge.docs_dir` 可能已被配置改写。默认位置和存储内容见[文件系统布局](../reference/filesystem-layout.md)。数据库完整性可以在停止写入后检查：

```bash
sqlite3 "$HOME/.echo-agent/data/echo_agent.db" "PRAGMA integrity_check;"
```

若存储文件损坏，从对应的完整备份恢复。`echo-agent checkpoint` 排除数据库、会话、记忆与日志，不能替代[备份与恢复](backup-restore.md)。

## 配置修改没有生效

```bash
echo-agent config validate
echo-agent config explain
echo-agent config dump
```

加载器在搜索目录中按 `echo-agent.yaml`、`echo-agent.yml`、`config.yaml`、`config.yml` 查找。`-c/--config` 可指定文件。环境变量覆盖的路径层级使用双下划线，例如 `ECHO_AGENT_GATEWAY__PORT`；单下划线只属于字段名本身。变更后台服务使用的配置路径后，应重新安装并启动服务。

## 工具执行失败

检查 `tools.profile`、`tools.exec.host`、`permissions.approval` 及具体工具的外部依赖。`echo-agent deps status` 列出依赖状态。默认 `sandbox` 执行后端使用工作目录副本，但仍在主机进程中执行命令，不提供操作系统级隔离；需要隔离时应配置容器或远端后端。细节见[执行后端](../guides/execution-backends.md)与[工具权限](../guides/tools-permissions.md)。

## 磁盘空间或停机问题

先查看实际工作区及数据库、日志、溢写、产物和检查点目录的占用，再按数据保留策略清理。`echo-agent checkpoint prune` 只清理工作区文件快照；它不会清理数据库或日志。运行 `VACUUM` 前应安排合适的维护窗口并备份数据库。

后台服务有 60 秒停止超时，这是服务管理器的退出窗口，不是活跃任务查询结果。`echo-agent gateway status` 只报告服务状态。若停止失败，查看服务管理器日志，确认进程状态及数据写入情况后再决定如何处理。
