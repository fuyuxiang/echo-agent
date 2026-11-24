# Desktop installation and requirements

**Scope: Desktop.** The application includes its execution engine, IDE, and office-document generator. A Python Runtime installation or Gateway deployment is not required.

## Install a release

1. Open [Desktop Releases](https://github.com/fuyuxiang/echo-agent-desktop/releases).
2. Choose an installer matching your operating system and processor.
3. Install and launch EchoAgent, then follow the [quickstart](quickstart.md).

| Platform | Release targets | Conditions |
| --- | --- | --- |
| macOS | Apple Silicon and Intel | Computer automation requires Accessibility and Screen Recording |
| Windows | x86_64 | Computer automation requires an accessible interactive desktop |
| Linux | No official desktop installer currently | Low-level automation support does not imply an installer |

Build from source if no suitable package is available. Releases include Node.js for the IDE and office components. Project-specific language runtimes and build tools must still be prepared separately.

## Models and networking

Add and test a personal model connection under **Settings → Models**, or sign in to use an organization-provided model. Task execution depends on service availability and model capabilities. See [model setup](models-extensions.md).

Personal knowledge and memory use on-device retrieval models. The selected chat model receives task context, and organization knowledge is retrieved by the organization service. See [Data management](operations.md).

## Run from this repository

Requirements: Node.js 22.23.4+ on 22.x or 24.22.1+ on 24.x, pnpm 10, npm 11.21.1+, Rust 1.96.0+, and `protoc`. macOS requires Xcode Command Line Tools. Windows requires Visual Studio 2022 C++ Build Tools and Windows SDK.

Run from the repository root.

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

The first launch prepares the engine, Theia IDE, and Office Worker. Download and build times vary. `pnpm tauri dev` starts the full application; `pnpm dev` starts only the frontend development server.

## Verify installation

Open the home screen, select a directory and model, and send a simple message. Then check any coding or automation features you need. For unavailable automation, check [platform requirements](automation.md) first.
