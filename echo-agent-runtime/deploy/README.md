# 部署模板

本目录的文件只依赖 `echo-agent-runtime/` 内的源码。直接 Python 部署见 [Runtime 安装说明](../README.md)。后台服务使用 Runtime 自带的 `echo-agent gateway install`，无需另维护一套 systemd/launchd 模板。

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
