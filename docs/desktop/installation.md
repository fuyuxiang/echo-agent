# Desktop 安装与环境要求

**适用范围：桌面端。** 安装包自带执行引擎和办公文档生成器，可独立运行。

## 安装发行包

1. 打开 [Desktop Releases](https://github.com/fuyuxiang/echo-agent-desktop/releases)。
2. 选择与你的操作系统和处理器架构匹配的安装包。
3. 安装并打开 EchoAgent，再完成[第一个任务](quickstart.md)。

| 平台 | 发行目标 | 使用条件 |
| --- | --- | --- |
| macOS | Apple Silicon、Intel | 电脑操作需要辅助功能与屏幕录制权限 |
| Windows | x86_64 | 电脑操作需要可访问的交互桌面 |
| Linux | 当前无正式桌面发行包 | 部分底层自动化支持不等于提供安装包 |

发行页没有对应包时，可从源码构建。正式安装包自带办公组件使用的 Node.js。代码项目所需的语言运行环境和构建工具按项目准备。

## 模型与网络

在「设置 → 模型」中添加并测试个人模型连接，或登录组织账号后选用组织提供的模型。任务执行取决于所选服务的可用性和模型能力。详细步骤见[模型配置](models-extensions.md)。

个人知识库和记忆使用本机检索模型。任务选用的上下文会提交给当前对话模型，组织知识由组织服务检索。详见[数据管理](operations.md)。

## 从当前仓库源码运行

需要 Node.js 22.23.4+（22.x）或 24.22.1+（24.x）、pnpm 10、Rust 1.96.0+ 和 `protoc`。macOS 还需要 Xcode Command Line Tools。Windows 需要 Visual Studio 2022 C++ 构建工具和 Windows SDK，准备语音资源时需要在 PATH 中提供 Python 或设置 `PYTHON`。

以下命令从当前仓库根目录开始。

=== "macOS"

    ```bash
    cd client
    pnpm setup:mac
    pnpm install --frozen-lockfile
    pnpm tauri dev
    ```

=== "Windows PowerShell"

    ```powershell
    cd client
    pnpm setup:win
    pnpm install --frozen-lockfile
    .\dev.bat
    ```

首次启动会准备 Office Worker、随包 Node.js、检索和语音资源，并编译内嵌执行引擎。`pnpm tauri dev` 启动完整应用。`pnpm dev` 启动前端开发服务器。

## 安装验证

确认可以打开首页、选择目录与模型，并发送一条简单消息。需要代码或自动化时，再验证对应功能。自动化不可用时，先检查[平台条件](automation.md)。
