# Changelog

本文件记录 Echo Agent 各版本的用户可见变化。格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [Unreleased]

### Added
- 建立完整文档站（MkDocs Material + 中英文双语）
- 会话历史接口区分「人类可读记录」与「LLM 上下文」：过滤掉压缩注入的摘要对，工具调用 / 结果带 `internal` 标记由 Dashboard 折叠显示

### Fixed
- 会话历史接口的 `limit` 增加范围校验（1–500）：此前 `limit=0` / 负数经 Python 切片会返回全部历史；`total` 改为整段历史条数，新增 `returned` 表示本次返回条数
- **安全**：`gateway.host` 留空曾被当作本机回环，导致未配置 token 的网关可以启动并暴露到所有网卡。空字符串与 `::` 现按通配绑定处理，与 `0.0.0.0` 一致
- 绑定地址判定统一到 `gateway/host_rules.py`（此前向导、`_check_bind_safety`、启动告警、`GatewayAuth` 各持一份判据且互相矛盾）；`127.0.0.0/8` 全段与 `[::1]` 不再被误判为非本机
- `auth.allowed_hosts` 条目在比对前规范化：大小写、端口、IPv6 方括号不再影响匹配，从地址栏粘贴的值可以直接用；通配地址不再作为有效条目被静默接受
- 向导在通配绑定下不再用绑定地址预填 `allowed_hosts`（`[0.0.0.0]` 匹配不到任何浏览器请求，却会让告警闭嘴），改为探测本机地址作建议值
- 向导在绑定改回本机时会提示清理遗留的 `allowed_hosts`——显式白名单会覆盖默认的本机放行规则，此前会让本机 Dashboard 的管理页面全部 403

## [0.3.7] - 2026-08-16

### Fixed
- 修复通道体系 5 项 P1 与 4 项 P2 安全及数据一致性缺陷
- 修复通道体系剩余 5 项缺陷与体验改善
- 修复通道体系 13 项 P0/P1/P2 安全与数据一致性缺陷
- 修复 Dashboard 9 项 P0/P1 数据一致性与安全缺陷

## [0.3.6] 及更早版本

> 历史版本变更记录待补录。欢迎贡献者协助从 git 历史中整理。

[Unreleased]: https://github.com/fuyuxiang/echo-agent/compare/v0.3.7...HEAD
[0.3.7]: https://github.com/fuyuxiang/echo-agent/releases/tag/v0.3.7
