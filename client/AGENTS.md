# 提交与同步约定

本约定适用于 `origin` 指向 `fuyuxiang/echo-agent-desktop` 的本项目仓库。

- 本项目直接在现有的 `main` 分支上修改文件、提交并推送。不要新建分支。
- 用户要求提交并推送本项目时，同步将该提交中所有已跟踪的项目文件更新到 `fuyuxiang/echo-agent` 的 `master` 分支 `client/` 目录；两个提交使用相同的提交信息。用户明确要求仅本地提交或取消同步时，按用户当次要求执行。
- 同步时保留目标 `client/` 中仅目标仓库拥有的文件；不要改动目标仓库的其他目录，也不要加入本项目未跟踪或被忽略的文件。
- 不使用强制推送。完成后核对两个远端分支的提交，并向用户报告两个提交号；任何一步失败，都说明已完成的部分和失败原因，不得声称两边均已提交。

## 提交前检查

每次提交前，在仓库根目录对拟提交的内容运行并通过以下检查。Node.js、pnpm 和 Rust 使用工作流与仓库工具链配置指定的版本，依赖按锁文件安装。

```sh
pnpm install --frozen-lockfile
cargo fmt --manifest-path src-tauri/Cargo.toml --check
node scripts/check-rust-desktop.mjs
pnpm exec tsc --noEmit
pnpm test
pnpm test:scripts
pnpm office:test
pnpm build
git diff --cached --check
```

- 变更涉及 Rust 宿主时，跑 `pnpm test:rust`。涉及内置 Runtime、记忆或恢复逻辑时，再跑 `.github/workflows/ci.yml` 对应的 Runtime、记忆和恢复验证。
- 变更涉及界面时，跑相关的 `pnpm test:ui:*` 验证。优先用布局、分组和视觉层级表达关系，文字只在帮助用户操作、识别状态或理解风险时保留。
- 变更涉及依赖、工作流或打包时，分别跑对应的依赖审计、`actionlint -shellcheck= -pyflakes=` 或平台打包预检，按工作流中的实际命令核对。
- 任一适用检查失败时，修复后重跑通过再提交。涉及当前机器无法执行的平台验证时，记录依赖远端验证的项目，并跟进 GitHub Actions 的实际结果。
- 推送后按提交 SHA 核对 `ci`、`desktop-validation` 和适用的 `workflow-validation`。仍在运行、取消或失败的验证需要继续跟进，直到适用检查通过，或确认存在需要用户介入的阻塞。

## 架构与源码归属

- 先查阅 [架构与维护边界](docs/architecture.md)，按数据所有权和实际调用入口选择模块。
- `src/` 负责界面和前端状态。`src-tauri/` 负责 Tauri IPC、平台服务与桌面业务。`src-tauri/src/harness/` 协调桌面 Runtime 代次、会话、权限和事件。
- Agent 多轮执行、模型流、工具调度、上下文与会话持久化由 `agent/crates/` 的现有 crate 维护。桌面通过进程内 ACP 接入。`agent/terminal/` 保留终端入口和独立验证。
- 通用依赖由锁文件管理。局部定制源码集中在 `third-party/`，记录来源、版本或提交、完整性、许可证、修改与验证入口。资源许可证在 `LICENSES/` 或其随包资源目录维护。
- 调整模块或路径时，核对 Windows、macOS、Linux 的条件分支与 Cargo workspace，检查 CI 触发、审计、资源打包和旧数据读取，并运行 `node scripts/verify-agent-source.mjs`。
- DOCX 渲染器在 `third-party/docx-preview/` 从 TypeScript 构建。修改分页、分节和字体加载时，验证源码包类型、原始 Word 回归与 `pnpm test:ui:office-workbench`。
