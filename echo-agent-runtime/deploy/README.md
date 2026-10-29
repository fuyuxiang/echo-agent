# 部署模板

本目录的文件只依赖 `echo-agent-runtime/` 内的源码。直接 Python 部署见 [Runtime 安装说明](../README.md)。后台服务使用 Runtime 自带的 `echo-agent gateway install`，无需另维护一套 systemd/launchd 模板。

## 源码安装与定时任务

部署模板显式启用调度器，并提供 `cronjob`、`web_fetch` 和 `skill_run`。
`coding` 工具档位也包含 `cronjob`；创建任务仍需经过审批，需要运行写入或命令工具的任务还须获得该任务的无人值守授权。
`public_gateway` 的默认限制、`tools.deny` 和独占 `tools.allow` 列表仍然生效。
创建后检查定时任务页的任务 ID、下次执行时间和授权状态，再做一次测试运行，检查最终投递状态。

部署或扩容后执行 `echo-agent setup doctor --json`，并用 `df -h /实际工作区` 检查数据所在文件系统。
Linux 下可以用 `lsblk` 对比云盘、分区和文件系统容量：云盘从 40GB 扩至 100GB，并不会自动让根文件系统获得这 60GB。
服务应使用自己的系统用户运行诊断；root 保留块不等于普通服务用户的可用空间。
数据分区不足 512 MiB 时 doctor 会告警，不足 64 MiB 时会失败，`/api/v1/ready` 也不会报告就绪。

快照在写入 Git 对象库前过滤运行数据、快照仓库自身和超大文件，并在空间不足时跳过快照，为实际任务和 SQLite 留出空间。
微信状态文件采用原子替换，写盘失败会保留上一次完整记录；文本和媒体发送均识别服务端 `message_id` 回执，同时保留显式错误判断。

## 容器

在本目录执行：

```bash
cp echo-agent.yaml.example echo-agent.yaml
# 编辑模型和两个随机令牌；容器配置的 gateway.host 改为 0.0.0.0
# 挂载文件需允许容器用户 UID 10001 读取，例如 chown 10001:10001 后 chmod 600
docker compose up --build -d
docker compose logs -f echo-agent
```

启动前为 Compose 所在进程设置 `OPENAI_API_KEY`。模板默认安装 `[openai]`；需要其他 provider/工具依赖时修改 Dockerfile 的 extra，并准备相应外部命令。镜像在构建阶段编译 Dashboard，运行阶段只有 Python 和已安装的 Runtime，不需要 Node/pnpm、Rust 或桌面端。

端口默认仅映射到宿主机回环地址。配置文件挂载为只读；工作区使用独立数据卷。镜像以 UID 10001 运行，绑定挂载的文件和目录须提供对应读写权限。模型缓存默认在容器用户家目录；需要保留时另挂载 `/home/echoagent/.echo-agent/models`。完整备份应覆盖工作区、配置和显式外部路径。

这是随 Runtime 提供的部署模板；Python 源码/安装包的 CI 验证不等同于容器运行验证，应在目标 Docker 平台验证后采用。
