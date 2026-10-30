# 后台服务

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

`echo-agent gateway install` 在 Linux 上安装 systemd 用户级服务，在 macOS 上安装 launchd LaunchAgent。服务管理器运行前台 Gateway 进程并负责重启与日志收集；`echo-agent gateway` 本身也是可直接运行的前台命令。

## 安装与管理

```bash
echo-agent gateway install
echo-agent gateway start
echo-agent gateway status
echo-agent gateway logs
echo-agent gateway logs --follow
echo-agent gateway restart
echo-agent gateway stop
echo-agent gateway uninstall
```

安装自定义工作区或配置时，在 `install` 命令中指定绝对路径，例如：

```bash
echo-agent gateway install --workspace /srv/echo-agent --config /srv/echo-agent/echo-agent.yaml
echo-agent gateway start
```

安装程序会把解析后的工作区与配置路径固化在服务定义中，避免服务启动时依赖登录 shell 的当前目录。修改服务配置或 Python 安装路径后，应重新运行 `echo-agent gateway install`，再重启服务。

## Linux：systemd

默认 unit 路径是 `~/.config/systemd/user/echo-agent.service`。安装程序启用该用户服务，并生成包含 Python 解释器绝对路径、`-m echo_agent gateway`、工作目录及 `_ECHO_AGENT_SUPERVISED=1` 的 unit。unit 设置 `Restart=always`、5 秒重启间隔、300 秒内最多 10 次失败启动，以及 60 秒停止超时。

```bash
systemctl --user status echo-agent
journalctl --user -u echo-agent -n 100
journalctl --user -u echo-agent -f
```

如需在用户退出登录后继续运行，应根据系统策略单独启用 linger；`install` 不会自动启用：

```bash
sudo loginctl enable-linger "$(whoami)"
```

Linux 还支持 `echo-agent gateway install --system` 安装系统级服务；需要相应的系统权限。

## macOS：launchd

默认 plist 路径是 `~/Library/LaunchAgents/com.echo-agent.gateway.plist`。生成的 LaunchAgent 设置 `RunAtLoad`、`KeepAlive`、工作目录及 `_ECHO_AGENT_SUPERVISED=1`。标准输出和错误输出写入 `~/.echo-agent/logs/gateway.log`；可用 `echo-agent gateway logs` 查看。plist 中的 `ExitTimeOut` 为 60 秒。

## 环境变量与关停

服务进程不会自动继承交互式 shell 中临时 `export` 的变量。模型密钥和配置覆盖项应通过服务管理器支持的环境配置方式传入；修改后重启服务。不要把真实密钥写进文档示例或提交到仓库。

停止超时是服务管理器允许的最长退出窗口，并不保证正在进行的 Agent 回合会在 60 秒内自然完成。`echo-agent gateway status` 报告服务状态，不是活跃任务计数。需要备份数据库时，应确认进程已停止，参见[备份与恢复](backup-restore.md)。

在容器中应以前台 `echo-agent gateway` 作为入口，不必安装 systemd 或 launchd 服务。示例见[部署方案](deployment.md)。
