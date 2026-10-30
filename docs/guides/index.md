# Runtime 使用指南

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

完成安装后，先执行[第一个工作流](first-workflow.md)，再按目标选择功能。每项集成都应先验证单次运行，再引入后台或定时执行。

| 使用目标 | 对应指南 | 需要确认 |
| --- | --- | --- |
| 接入云端或本地模型 | [模型接入](models/index.md) | 提供商、地址、凭据、模型能力 |
| 控制可执行操作 | [工具与权限](tools-permissions.md) | 工具可见性、审批和执行范围 |
| 选择命令执行环境 | [执行后端](execution-backends.md) | 本地、工作副本、容器或远程的实际边界 |
| 使用网页与媒体能力 | [浏览器与媒体](browser-media.md) | 可选依赖、网络与服务凭据 |
| 保存稳定事实 | [记忆管理](memory-management.md) | 写入结果、来源与作用域 |
| 从资料中查找答案 | [知识库](knowledge-base.md) | 导入、索引状态和检索来源 |
| 继续或管理对话 | [会话](sessions.md) | 历史、重置、归档与访问边界 |
| 执行多步工作并交付文件 | [任务与规划](tasks-planning.md) | 计划、实际状态、产物与验证 |
| 按时间触发任务 | [定时任务](scheduled-jobs.md) | 调度、预授权与结果投递 |
| 管理运行实例 | [Dashboard](dashboard.md) | 连接、认证与管理权限 |
| 控制资源用量 | [成本控制](cost-control.md) | 预算、价格配置与实际账单差异 |

系统设计见[核心概念](../concepts/index.md)，常驻运行见[部署与运维](../operations/index.md)，精确参数见[参考手册](../reference/index.md)。
