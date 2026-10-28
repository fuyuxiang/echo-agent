# 技能系统使用指南

Echo Agent 内置 35 个 SKILL.md，分布在 10 个类别。技能是供模型读取的操作说明，可能附带脚本；存在说明文件不表示第三方账户、外部 API 或本地程序已经配置可用。

## 发现、选择与执行

1. 启用的技能以名称、类别和描述注入有字符预算的清单；超预算会明确说明省略，模型可用 `skills_list` 查找。
2. 模型根据请求选择技能；没有单独的匹配度排序、互斥或触发器引擎。
3. `skill_view` 读取 SKILL.md 或支持文件，并提示声明的缺失依赖/环境变量；读取不自动安装依赖。
4. `skill_run` 在技能目录中运行脚本，按既有审批门控安装已声明且允许的依赖，受执行开关、工具策略、超时和进程清理约束。

`tools.exec.enabled: false` 同时关闭技能脚本执行。`daemon`/`public_gateway` 默认拒绝 `skill_run`，需要运行时由操作者明确放行；技能是主机代码，不是 OS 沙箱。禁用技能后，列表、读取与执行都按同一禁用判据处理。

## 正确的 frontmatter

````markdown
---
name: my-report
version: 1.0.0
description: "Generate a report using an explicitly configured API account."
metadata:
  echo:
    tags: [Report]
    requires:
      pip: [httpx]
      env: [MY_REPORT_API_KEY]
---

# My Report

Describe the actual script arguments, output files and failure handling here.
````

类别来自技能目录。`metadata.echo.tags` 提供标签，依赖由 `metadata.echo.requires.pip`/`requires.bins` 声明，凭据键由 `metadata.echo.requires.env` 声明；插件顶层 `requires_env` 不是这里的等价写法。安装声明还可使用 `metadata.echo.install`，须与实际依赖一致。

脚本只继承安全基础环境及显式声明的凭据键，不继承整个 Runtime 环境。通过服务进程环境或受保护的 systemd EnvironmentFile 设置变量，配置树没有通用顶层 `env` 字段；不要把密钥放在脚本参数中。

## 内置技能的实际条件

| 技能 | 凭据与边界 |
|---|---|
| image-gen | OpenAI 用 `OPENAI_API_KEY`，Stability 用 `STABILITY_API_KEY`；其他路径按当前脚本说明 |
| web-search | DuckDuckGo 或配置的 SearXNG；不承诺 SERPAPI/Tavily 集成 |
| tts-voice | Edge TTS 路径无密钥；OpenAI 路径用 `OPENAI_API_KEY` |
| stocks | 公共行情接口，不强制 ALPHA_VANTAGE_KEY；可用性受对端约束 |
| notion-sync | `NOTION_API_TOKEN` |
| calendar | CalDAV 地址与账户参数；本地 ICS 示例是独立配方，不是 Google OAuth 集成 |

自动简报、监控、行情提醒等说明中的 YAML 可能是给 Agent/操作者的组合配方，只有脚本明确声明读取的配置才会自动生效。周期执行使用核心 `cronjob` 创建并授权，结果仍须通过既有投递链路确认。

## 安装、变更与管理

Dashboard Skills 可查看正文/文件/依赖，启停或安装已声明依赖；外部导入和写入使用 admin 权限与技能准入流程。候选、审批、晋升和回滚参见[进化与评估](../../concepts/evolution-evaluation.md)。Python 插件注册新工具，技能组合已有工具，两者的信任和生命周期不同。

## Cohesivity（可选技能）

Cohesivity 位于仓库的 `optional-skills/cohesivity/`，不属于 35 个内置技能，不随 Python wheel/sdist 分发，也不在默认技能目录中。默认安装、启动和普通的应用/数据库/部署请求不会加载它。需要使用时，用户必须主动选择 Cohesivity，并从已审查的仓库版本导入该目录。

在已有源码检出的环境中，明确要求 Agent 安装该本地技能时，`skill_install` 参数如下（不是运行时 YAML 配置）：

```json
{
  "source": "local",
  "location": "/absolute/path/to/echo-agent/optional-skills/cohesivity",
  "run_install": false
}
```

这一步只导入说明文件，走现有技能扫描与准入流程；不会执行 npm、访问 Cohesivity、安装全局技能或创建租户。若源码不在本机，先取得审查过的提交，不要把整个仓库根目录作为技能导入源。

实际开通后端需单独授权。技能固定使用 `@cohesivity/init@0.9.4`，执行前必须说明第三方代码执行、全局 `~/.agents/skills` 写入、项目指令文件修改、机器标识及 72 小时租户过期；不接受这些影响即可停止初始化。部署、付费、认领、MCP 配置和反馈发送也需相应的明确授权。上游安装器的行为属于第三方边界，不因技能被安装而自动获准。

- [技能目录](catalog.md)
- [编写技能](../../development/skill-authoring.md)
- [工具与权限](../../guides/tools-permissions.md)

## deed.page（可选技能）

deed.page 位于仓库的 `optional-skills/deed-page/`，不属于 35 个内置技能，与 Cohesivity 一样不随包分发，也不被默认加载。普通的"做个网页""分享一下"请求不会选中它；用户必须明确要求用 deed.page 发布，并从已审查的仓库版本导入该目录：

```json
{
  "source": "local",
  "location": "/absolute/path/to/echo-agent/optional-skills/deed-page",
  "run_install": false
}
```

导入只安装说明文件：没有安装依赖，不上传任何内容，也不创建令牌。每次发布都需单独授权：Agent 需说明要上传的文件、页面将公开可见以及过期时间（默认七天，用户要求时才永久保留）。首次发布返回的所有者令牌保存在 `~/.config/deed-page/token`（权限 `0600`），命令输出中会被遮蔽。删除页面、升级付费、自定义域名、Webhook 和公开画廊展示均需各自的明确授权。
