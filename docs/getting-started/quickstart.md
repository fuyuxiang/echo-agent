# Runtime 快速开始

**适用范围：独立 Python Runtime。** 本页完成环境准备、模型配置和首次请求，再说明终端接入。桌面安装包用户请使用 [Desktop 快速开始](../desktop/quickstart.md)。

## 1. 准备环境并安装

需要 Python 3.11+、可连接的模型服务，以及该服务要求的凭据。建议在独立虚拟环境安装；下载时长和可用依赖随平台而异。

=== "Linux / macOS / WSL2"

    ```bash
    python3 -m venv .venv
    source .venv/bin/activate
    python -m pip install "echo-agent[all]"
    echo-agent --version
    ```

=== "Windows PowerShell"

    ```powershell
    python -m venv .venv
    .\.venv\Scripts\Activate.ps1
    python -m pip install "echo-agent[openai,anthropic,tui]"
    echo-agent --version
    ```

`all` 是预列的常用依赖集合，不含每个独立 extra。Windows 示例选择部分依赖，使用其他功能时再添加对应 extra；二进制依赖受阻时优先使用 WSL2。完整选项见[安装指南](installation.md)。

## 2. 配置并验证模型

```bash
echo-agent setup --flow quickstart
echo-agent config validate
```

向导引导你选择语言、模型提供商、服务地址、凭据与模型，并处理必要设置。模型 ID 应以你所用服务实际返回或公布的名称为准。优先完成连接与模型测试，再启用其他集成。

首次练习在向导结束时只保存配置，先不注册后台服务。默认配置位于用户数据目录，向导会显示实际保存位置。需要独立配置和工作区时，使用 `-c` 与 `-w`，并在后续命令中保持一致；具体规则见[文件布局](../reference/filesystem-layout.md)。

配置校验通过说明配置可解析，不代表网络或模型工具调用已验证。首次实际请求仍需检查。

## 3. 启动 Gateway

```bash
echo-agent gateway
```

保持终端运行，打开 <http://127.0.0.1:58123/>。若向导已经启动了 Gateway，直接使用已有实例，不要重复启动。端口被自定义时以启动日志为准。

Gateway 提供 Dashboard 与 API。正式发布的 wheel 包含前端产物；源码安装应先构建 Dashboard，缺少产物时根页面可能显示简化 playground。详见 [Dashboard 指南](../guides/dashboard.md)。

启用了认证时，按已有配置提供 API 或管理令牌；需要哪类令牌取决于页面操作。不要把管理令牌附在公开 URL 中。

## 4. 完成首次请求

打开 Gateway 的交互页 <http://127.0.0.1:58123/playground>，发送：

> 请用三句话说明你能做什么。暂时不要读写文件或调用外部工具。

成功标准是收到与请求相关的完整回复，且服务没有报告模型认证或连接错误。随后在同一会话追问“请把刚才的第二句话改得更简洁”，验证会话连续性。

## 5. 按需连接终端

保持 Gateway 运行，在另一个已激活同一虚拟环境的终端执行：

```bash
echo-agent cli
```

默认是终端内联交互；安装了 `tui` 依赖后，可使用全屏界面：

```bash
echo-agent cli --tui
```

CLI/TUI 通过 WebSocket 连接已有 Gateway，不再启动执行引擎。若只想本地终端交互，可先停止 Gateway，再执行 `echo-agent run`。`run` 是否同时开放 Gateway 由配置决定；非交互标准输入不会启动本地 CLI 通道。

## 6. 验证并停止

```bash
echo-agent status
echo-agent cost
```

状态与费用可帮助检查运行记录；费用为基于配置的估算，应与模型服务账单区分。前台 Gateway 用 `Ctrl+C` 停止，等待收尾后再关闭终端。后台服务使用自己的停止命令，详见[后台服务](../operations/background-service.md)。

| 问题 | 优先处理 |
| --- | --- |
| 找不到命令 | 激活安装时的虚拟环境 |
| 模型认证失败 | 核对提供商、Key 与模型 ID |
| 配置有效但请求失败 | 检查网络、服务地址与模型测试 |
| 网页打不开 | 查看启动日志、监听地址和实际端口 |
| CLI 连接失败 | 确认 Gateway 正在运行、端口与令牌一致 |

## 下一步

完成[第一个工作流](../guides/first-workflow.md)，再了解[工具权限](../guides/tools-permissions.md)、[记忆管理](../guides/memory-management.md)与[常驻部署](../operations/background-service.md)。长期记忆为独立机制，重启后能否回忆一句话不宜作为唯一验收方式。
