# 部署与运维

本节涵盖 Echo Agent 的生产部署、日常运维和故障处理。

---

## 章节目录

| 章节 | 说明 |
|------|------|
| [运行模式](runtime-modes.md) | 前台 / Gateway / CLI Client 三种运行方式 |
| [后台服务](background-service.md) | systemd / launchd / tmux 守护进程部署 |
| [部署方案](deployment.md) | 单机、容器、反向代理等部署拓扑 |
| [安全加固](security-hardening.md) | 生产环境安全清单 |
| [备份与恢复](backup-restore.md) | 数据备份策略与灾难恢复 |
| [升级与迁移](upgrade-migrations.md) | 版本升级与数据库迁移 |
| [可观测性](observability.md) | 日志、监控、OpenTelemetry 集成 |
| [性能优化](performance.md) | 资源调优与瓶颈分析 |
| [故障排查](troubleshooting.md) | 按症状索引的排障指南 |

---

## 快速概览

Echo Agent 支持三种运行模式：

```bash
# 前台交互（开发/调试）
echo-agent run

# 后台 Gateway 服务（生产推荐）
echo-agent gateway install
echo-agent gateway start

# CLI 客户端连接远端 Gateway
echo-agent cli
```

!!! tip "生产部署推荐"
    使用 `echo-agent gateway install` 注册系统服务，由 systemd (Linux) 或 launchd (macOS) 管理进程生命周期，获得自动重启、日志收集和开机自启能力。
