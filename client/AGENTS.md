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
