# Skill 可执行能力声明

普通 `SKILL.md` 仍可只提供说明和工作流。需要执行脚本、依赖本机命令或已连接的服务、或产出可检查文件的 Skill，可以在同一目录附加 `echo.skill.json`。该文件是声明和预检依据，**不会授予新权限**；脚本仍通过普通工具执行，受当前工作区、沙箱和用户审批约束。连接器凭据留在连接器中，不交给 Skill 脚本。

## 示例（schemaVersion 1）

```json
{
  "schemaVersion": 1,
  "capabilities": ["document.docx.create"],
  "runtime": {
    "kind": "python",
    "command": "python3",
    "entrypoints": { "create": "scripts/create.py" },
    "timeoutSeconds": 120
  },
  "requirements": {
    "commands": ["libreoffice"],
    "connectors": [],
    "osPermissions": []
  },
  "permissions": {
    "filesystem": "workspace-write",
    "network": [],
    "externalActions": []
  },
  "artifacts": [
    {
      "id": "document",
      "pattern": "output/*.docx",
      "mimeType": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
      "required": true,
      "maxBytes": 52428800
    }
  ]
}
```

这是第三方 Skill 的示例，`libreoffice` 只属于该示例的依赖；内置办公文档生成不需要安装它。

`runtime.kind` 可选 `python`、`node`、`shell`；`command` 可省略，默认分别是 `python3`、`node` 和系统 Shell。入口文件必须是包内普通文件，命令只能写名称，不能写绝对路径。`requirements.connectors` 可声明连接器 ID、显示名称、是否需要账号及用途；已配置连接器的密钥不会写入清单或提示词。

`permissions.filesystem` 可选 `none`、`workspace-read`、`workspace-write`。`network` 只接受 HTTPS origin，或本机 loopback HTTP origin；不能包含路径、查询、凭据或通配符。`externalActions` 用于声明发邮件、发布页面等外部副作用，执行时仍要经过正常审批。`artifacts` 的路径必须相对工作区，不能向上遍历；运行契约要求 Agent 在报告成功前核对必需产物，清单本身不自动执行产物校验。

## 安装与运行状态

- 没有 `echo.skill.json`：按提示词/工作流 Skill 使用。
- 清单无效：受管安装或直接路径注册会拒绝。
- 缺少命令或连接器：显示「缺少运行依赖」；连接器尚需账号配置或授权：显示「尚未完成配置」。
- 自动化任务只接受提示词 Skill 或预检就绪的可执行 Skill。声明的工作区写入、网络和外部操作会进入安装风险报告。

清单最大 256 KiB；注入运行上下文的契约最大 4 KiB，超出时预检拒绝。具体字段和校验逻辑以 [`capability.rs`](../agent/crates/echo-agent-tools/src/implementations/skills/capability.rs)、[`validate.rs`](../agent/crates/echo-agent-tools/src/implementations/skills/capability/validate.rs) 及桌面端 [`skill_installer.rs`](../src-tauri/src/skill_installer.rs) 为准。
