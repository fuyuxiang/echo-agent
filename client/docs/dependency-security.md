# 依赖安全维护

依赖维护需要同时验证漏洞修复、调用方兼容性和最终安装包。审计通过只能说明所扫描的版本图未报告漏洞，不能代替编译、功能测试或桌面验收。

## 2026-10-27 修复依据

桌面和 Agent workspace 的锁文件均使用 `zstd 0.14.3`、`zstd-safe 8.2.0`、`zstd-sys 2.1.2+zstd.1.5.7`，处理 RUSTSEC-2026-0391、RUSTSEC-2026-0395 至 0406 和 RUSTSEC-2026-0410。直接依赖门槛同时调整为 `zstd 0.14.3`、`tonic 0.14.7` 和 `async-compression 0.4.51`，使传递依赖也选用修复版本。修复边界见 [zstd 公告](https://rustsec.org/advisories/RUSTSEC-2026-0391.html)、[zstd-safe 公告](https://rustsec.org/advisories/RUSTSEC-2026-0395.html)和 [zstd-sys 公告](https://rustsec.org/advisories/RUSTSEC-2026-0410.html)。`block-buffer 0.12.0` 升至上游修复版 `0.12.1`。

对 GHSA-qwgh-2vcv-g2f7 的 `block-buffer 0.10.4` 命中进行单独源码复核：[该版本源码](https://docs.rs/block-buffer/0.10.4/src/block_buffer/lib.rs.html)将 `pos: u8` 与数据缓冲区分开存储，也没有公告中的 `ReadBuffer`。因此判断其不受该公告描述的内嵌游标破坏影响；这不是上游对受影响版本范围的更正。审计例外只接受这个包名、精确版本、公告编号及原始 crates.io 包的 SHA-256 `3078c7629b62d3f0439517fa394996acacc5cbc91c5a20d8c658e77abd503a71`，其他版本、公告、来源或校验值仍正常阻断。原始公告和复核依据保留在审计证据中，不全局忽略该公告。

`src-tauri/tests/block_buffer_panic_safety.rs` 通过实际 `sha2 0.10` 依赖的 buffer API，分别验证带特殊字节输入的压缩回调、写入特殊字节的生成回调 panic 后，捕获 panic 并继续复用时游标不被覆盖。该测试由现有宿主测试 job 执行；审计 job 另运行 `python3 -m unittest discover -s scripts -p 'test_audit_dependencies.py'`，验证精确匹配、非原始发布包保护及其他安全发现仍使审计失败。

## 2026-10-24 修复依据

宿主和 Runtime 的 Tokio 锁至 1.53.3、ctutils 锁至 0.4.4，分别处理 [RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html) 和 [RUSTSEC-2026-0385](https://rustsec.org/advisories/RUSTSEC-2026-0385.html)。两个 workspace 分别更新，不以其中一个锁文件代替另一个。

Theia 的 body-parser 保留 1.x / 2.x API，分别要求 1.20.9+ / 2.4.0+；multer 要求 2.5.0+，serialize-javascript 要求 7.1.3+。Puppeteer / puppeteer-core 在同一 25.x 内升级，移除旧 Chromium BiDi 的 Zod 3 依赖。构建 manifest 和生产运行时 manifest 继承同一 overrides，并分别重建锁文件。

`test:security` 从 Theia 与 MCP SDK 的实际模块路径分别加载两套 body-parser，在独立子进程的真实 HTTP 服务中验证 [GHSA-27r3-jxmh-p828](https://github.com/expressjs/body-parser/security/advisories/GHSA-27r3-jxmh-p828) 的流生命周期边界。有效 gzip 帧后加 NUL 和 100 KiB 尾部用于触发旧版未捕获的解压错误；分阶段 deflate 请求在解析回调后才发送最后字节，要求服务能读完 HTTP 请求，并保持另一路 SHA-256 消费者的数据完整。普通 JSON、合法 gzip、坏 gzip 的 400 响应及后续请求也逐项断言，子进程必须输出完整成功证据且以 0 退出。

本地对照使用同一 fixture：1.20.8 / 2.3.0 各 5 次均因未捕获的 `Z_DATA_ERROR` 退出 1，1.20.9 / 2.4.0 各 5 次均通过。旧版本只安装在临时目录，不加入项目或 CI 依赖。单独检查已安装版本可执行 `node scripts/fixtures/body-parser-recovery.mjs /absolute/path/to/body-parser/index.js`；不能以短后缀 gzip 请求得到 200/400 作为漏洞已修复的证据。触发样例依据[上游回归用例](https://github.com/expressjs/body-parser/blob/1.20.9/test/json.js)。

VSIX 开发工具通过 `@azure/identity` 引用 MSAL。当前最新 Identity 4.14.0 仍依赖 MSAL 6，而 [MSAL 公告](https://github.com/AzureAD/microsoft-authentication-library-for-js/security/advisories/GHSA-xcx6-jx8p-x3h5) 的修复从 7.1.0 开始，因此临时固定 Identity 4.13.0（使用 MSAL 3.8.10，满足 VSCE 的 `^4.1.0` 约束）。该开发依赖不进入 IDE 生产图；待上游发布兼容修复后再解除固定，不强制为 Identity 替换不同主版本的认证内核。真实 Azure 登录未在本地执行。

本次完整命令、结果和未验证范围见 [源码审计记录](source-audit-2026-10-24.md)。

## 工具链与依赖图

使用 Node.js 22.23.4+（22.x）或 24.22.1+（24.x）、pnpm 10、npm 11.21.1+、Rust 1.96.0+、Python 3.11+。IDE 构建和打包入口会检查 Node.js、npm 版本。不要用旧 npm 重新生成 IDE 的 workspace 锁文件。

| 范围 | 配置与锁文件 | 安装或更新入口 |
| --- | --- | --- |
| 桌面前端与 Office Worker | 根目录 `package.json`、`pnpm-lock.yaml` | `pnpm install --frozen-lockfile`；修改依赖后用 pnpm 更新锁文件 |
| Theia 源码、开发及可选扩展 | `vendor/theia-platform/package.json`、同目录 `package-lock.json` | `npm ci --prefix vendor/theia-platform` |
| 安装包中的 IDE 生产依赖 | `scripts/theia-runtime-package-lock.json` | `pnpm ide:lock` 更新；`pnpm ide:stage` 用 `npm ci --omit=dev` 安装 |
| Tauri 桌面程序 | `src-tauri/Cargo.toml`、`src-tauri/Cargo.lock` | `cargo check --locked --manifest-path src-tauri/Cargo.toml --lib` |
| 内嵌 Agent workspace | `vendor/echo-agent-build/Cargo.toml`、同目录 `Cargo.lock` | 用该 manifest 运行 Cargo；其锁文件与桌面锁文件分别维护 |
| Markdown fuzz workspace | `vendor/echo-agent-build/crates/codegen/echo-agent-markdown/fuzz/Cargo.toml`、同目录 `Cargo.lock` | 在该独立 workspace 中更新 Cargo 锁文件 |
| Nucleo 独立 workspace 与基准程序 | `vendor/nucleo/Cargo.toml`、同目录 `Cargo.lock` | 用该 manifest 更新锁文件并验证；桌面构建使用桌面锁文件，不能据此遗漏独立锁文件的审计 |
| 独立原生验收 harness | `scripts/desktop-native-check/Cargo.toml`、同目录 `Cargo.lock` | `pnpm test:desktop-native`；复用原生模块并有独立锁文件，须独立纳入审计 |
| 桌面恢复 harness | `scripts/stability-check/Cargo.toml`、同目录 `Cargo.lock` | `pnpm test:stability` 与 `pnpm test:stability:native`；独立依赖图同样纳入 Rust 审计 |

`scripts/theia-runtime-manifest.mjs` 将 Theia 根配置的版本门槛、overrides、`allowScripts` 和安全补丁 workspaces 带入生产依赖图，并移除应用自身的构建脚本和开发依赖。`allowScripts` 保留原生依赖安装所需的明确许可，也保留明确禁止执行的条目，避免生产安装偏离源码的执行策略。npm 默认会对未审阅的脚本发出警告；严格模式会拒绝未审阅的依赖。回归测试用本地安装包验证许可的安装脚本执行、禁止的脚本不执行；本地安装包使用源地址作为许可键，registry 依赖则使用可信包名。修改 Theia 依赖、overrides 或本地补丁包的依赖后，应重新运行 `pnpm ide:lock`，不能只更新源码锁文件。

## 本地补丁及来源

以下目录保留原始许可证；各自的 `SOURCE.json` 记录上游来源、公告和被修改文件的 SHA-256。审计脚本会核对补丁摘要和选用补丁的配置。

| 补丁 | 用途与兼容边界 |
| --- | --- |
| `vendor/glib/` | 基于 GLib Rust bindings 0.18.5，回移 `VariantStrIter` 的可变输出指针修复，保留 Tauri 在 Linux 上依赖的 GTK3 ABI。由 `src-tauri/Cargo.toml` 的 patch 选用。 |
| `vendor/lru-0.12/`、`vendor/lru-0.16/` | 分别基于 lru 0.12.5、0.16.4；修复析构可能 panic 时的节点脱链顺序，并为旧版本回移迭代器共享键引用修复。Agent workspace 和 fuzz workspace 各自配置 patch。 |
| `vendor/theia-platform/security/braces/` | 基于 braces 3.0.3，限制嵌套解析和调用方提供的 AST 遍历深度。通过 npm workspace 链接选用。 |
| `vendor/theia-platform/security/sprintf-js/` | 基于 sprintf-js 1.0.3，在原生数字格式化前限制精度和填充长度，保留合法格式行为。通过 npm workspace 链接选用。 |

上游引用与公告以各目录的 `SOURCE.json` 为准。修改补丁时，先确认安全边界与回归用例，再更新对应的 `patchedSha256`；不能为了使审计通过而仅重写摘要。上游有可替代修复时，应验证兼容性后移除本地副本、patch/workspace 配置及关联断言。

### SCANOSS 与 stream-json 的兼容补丁

`scripts/patch-theia-scanoss.mjs` 使 SCANOSS 0.40.2 的 CommonJS、ESM 两种发布构建使用 stream-json 3 的 `parserStream`、`streamObject.asStream()` 和新模块路径，保留安全版 `stream-json` 的覆盖配置。

Theia 源码安装的 `postinstall` 自动应用补丁；IDE 构建入口会再次确保补丁生效。生产打包在安装依赖后也调用该补丁，未包含可选 SCANOSS 包时直接跳过。补丁可重复执行，遇到未知版本或不匹配的源码会停止构建，避免静默生成不可用的工具。升级 SCANOSS 时需核实其上游是否已兼容新 API，再调整或移除此补丁。

`scripts/test-theia-scanoss.mjs` 验证真实 CLI 加载、两种构建的流式报告输出、中文和嵌套数据完整性，以及重复应用和源码变更保护。不要只测试 `--help`：解析器工厂在 3.x 也发生了变化，命令可加载不代表扫描结果可以写出。

## 升级流程

1. 从依赖公告和上游修复确定受影响范围，分别检查前端、Theia 源码、IDE 生产图和五个 Rust 锁文件（包括独立原生验收 harness）。优先选用上游兼容修复。
2. 调整直接依赖或最小必要范围的 override。跨主版本覆盖时，逐一核对调用方的导出方式、模块路径、参数、流事件和取消行为；不能仅依据审计输出选择版本。
3. 使用对应包管理器更新锁文件。修改 Theia 的生产依赖策略后运行 `pnpm ide:lock`，再用 `pnpm ide:build`、`pnpm ide:stage` 验证实际构建及生产安装。不要手工拼接锁文件。
4. 修改 Rust 依赖时分别更新实际受影响的 workspace 锁文件，随后以 `--locked` 编译；桌面程序不会自动继承另一个 workspace 根目录的 `[patch.crates-io]`。
5. 完成下述验证并检查 `git diff --check`。提交源码、许可证、补丁来源和锁文件；构建产物、审计报告及测试用户数据保存在临时目录或 CI artifacts 中。

### AI SDK 升级验收

Vercel AI 适配器同时验证普通文本、工具成功、无效 JSON、未知工具、处理函数异常、流中断和取消。失败调用需要带有可展示的错误结果并结束状态；后续正常模型响应仍应继续显示。UI 将失败明确标出并默认展开原因，成功结果保留折叠展示。除合成事件测试外，也使用真实 SDK 和本地模拟模型验证事件转换，避免仅修改测试事件名称而遗漏 SDK 的新行为。

## 验证命令

在仓库根目录执行：

```sh
pnpm install --frozen-lockfile
npm ci --prefix vendor/theia-platform
pnpm test:security
pnpm test
pnpm test:scripts
pnpm build
pnpm test:production-startup
pnpm test:mermaid
pnpm office:test
pnpm test:rust
```

IDE 的 AI、工具卡片和 SCM 定向验证在 `vendor/theia-platform` 目录执行：

```sh
node node_modules/typescript/bin/tsc -b packages/ai-vercel-ai packages/ai-chat-ui packages/scm
node node_modules/mocha/bin/mocha.js --config configs/mocharc.yml packages/ai-vercel-ai/lib/node/vercel-ai-language-model.spec.js packages/ai-chat-ui/lib/browser/chat-response-renderer/toolcall-part-renderer.spec.js packages/scm/lib/browser/dirty-diff/diff-computer.spec.js
```

认证及内存安全补丁另有 CI 检查：Agent 的 `auth::` 测试、GLib release 模式的 `variant_iter` 测试，以及两个 lru 副本的 `security` 测试。GLib 需要系统 GLib 开发库；发行包还需在各自的 macOS、Windows 环境运行 `.github/workflows/desktop-validation.yml` 的构建和验收。

日常 CI 的 `build` job 在生产构建后运行启动冒烟检查：只通过静态 HTTP 服务加载 `dist/`，验证真实侧栏和主界面已出现，并拒绝模块异常、启动错误页、其他控制台错误及资源加载失败。它使用全新的浏览器上下文，不注入成功的原生 IPC 响应；静态浏览器缺少 Tauri 宿主时，两条已知的原生 IPC 控制台提示单独记录，其余错误仍使检查失败。这项检查覆盖生产分块初始化，不能代替原生安装包验收。

本地可以复用 Theia 安装的 Playwright，或通过 `PLAYWRIGHT_MODULE` 指定独立工具目录中的 `playwright/index.mjs`；浏览器可执行文件可通过 `CHROME_BIN` 指定。`PRODUCTION_SMOKE_OUTPUT` 指定截图与 `report.json` 的输出目录，默认使用系统临时目录。检查其他构建目录时执行 `pnpm test:production-startup /path/to/dist`。

## 联网审计与证据

```sh
python3 scripts/audit-dependencies.py --output /tmp/echo-dependency-audit
# 只检查一个生态时：
python3 scripts/audit-dependencies.py npm --output /tmp/echo-npm-audit
python3 scripts/audit-dependencies.py rust --output /tmp/echo-rust-audit
```

不指定输出目录时会创建系统临时目录。npm 部分并行审计前端、Theia 源码和重建的 IDE 生产依赖图；Rust 部分向 OSV 查询六个锁文件中的 registry 包版本，保留公告、包清单和分类结果。`test:security` 对照 Git 跟踪清单检查锁文件覆盖，避免新增 harness 被漏审。本地补丁通过来源摘要及配置检查验证，不以全局忽略公告替代修复。

- 退出码 `0`：本次扫描完成且没有安全发现。
- 退出码 `1`：扫描完成但发现需处理的安全问题。
- 退出码 `2`：扫描不完整，包括网络、工具、报告格式或补丁校验失败；不能作为安全检查通过。

Rust 的纯停止维护公告单独记录在 `rust.json` 的 `maintenance` 中，安全发现保存在 `security` 中，经过精确版本复核的不受影响项及依据保存在 `not_affected` 中。CI 的 `dependency-security` job 保留审计产物。报告结论取决于执行时间和公告库覆盖范围；网络不可用、缺少平台环境或测试未完成时，应明确记录未验证部分。

## Dependabot 告警复核

先核对默认分支、告警关联的具体锁文件和 GitHub 依赖图中的实际版本，再判定是否需要升级。同一公告可能分别关联 manifest 和锁文件；某条告警显示 Fixed，不代表其他文件的告警也已同步更新。若当前锁文件和 GitHub 依赖图都已使用修复版本，保留联网审计证据，复核告警状态；不要为了消除旧状态而反复升级或改写锁文件。

`glib` 的 GTK3 兼容补丁仍保留 0.18.5 版本号，低于公告的上游修复门槛 0.20.0。核查此项时，应同时确认桌面 manifest 的 `[patch.crates-io]`、锁文件中该包没有 registry 来源、`SOURCE.json` 摘要和 release 模式的 `variant_iter` 回归测试。若告警仍按旧版本号报告，按具体公告记录回移修复证据后处理；不要全局忽略公告，也不要直接把 GTK3 所需的 GLib 改成不兼容的大版本。
