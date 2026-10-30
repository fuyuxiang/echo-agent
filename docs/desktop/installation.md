# Desktop 安装与环境要求

**适用范围：桌面端。** 安装应用即可使用内嵌执行引擎、IDE 和办公文档生成器，无需先安装 Python Runtime 或部署 Gateway。

## 安装发行包

1. 打开 [Desktop Releases](https://github.com/fuyuxiang/echo-agent-desktop/releases)。
2. 选择与你的操作系统和处理器架构匹配的安装包。
3. 安装并打开 EchoAgent，再完成[第一个任务](quickstart.md)。

| 平台 | 发行目标 | 使用条件 |
| --- | --- | --- |
| macOS | Apple Silicon、Intel | 电脑操作需要辅助功能与屏幕录制权限 |
| Windows | x86_64 | 电脑操作需要可访问的交互桌面 |
| Linux | 当前无正式桌面发行包 | 部分底层自动化支持不等于提供安装包 |

发行页没有对应包时，可从源码构建。正式安装包自带 IDE 所需的 Node.js 和办公组件，普通使用不需要开发工具链。代码项目自身的语言运行环境和构建工具仍需按项目准备。

## 模型与网络

应用提供内置模型，无需填写个人 API Key；推理请求访问项目配置的远端服务。也可以在“设置 → 模型”添加连接。联网条件、服务可用性和模型能力会影响任务执行。

本机保存状态不代表全部处理离线。模型会接收本轮选用的任务内容，启用的嵌入与重排服务也可能接收检索文本。详见[数据管理](operations.md)。

## 从当前仓库源码运行

需要 Node.js 22.23.4+（22.x）或 24.22.1+（24.x）、pnpm 10、npm 11.21.1+、Rust 1.96.0+ 和 `protoc`。macOS 还需要 Xcode Command Line Tools；Windows 需要 Visual Studio 2022 C++ 构建工具与 Windows SDK。

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

首次启动会准备内嵌执行引擎、Theia IDE 和 Office Worker，耗时取决于下载与构建。`pnpm tauri dev` 启动完整应用；`pnpm dev` 只启动前端开发服务器。

## 安装验证

确认可以打开首页、选择目录与模型，并发送一条简单消息。需要代码或自动化时，再验证对应功能。自动化不可用时，先检查[平台条件](automation.md)。
