# Third-Party Notices

EchoAgent application code is licensed under the MIT License. The application
also embeds and distributes third-party components under their respective
licenses.

## EchoAgent Agent Runtime (`echo-agent`)

- Runtime name: `echo-agent`
- Rust entry crate: `echo-agent-runtime`
- Source: https://github.com/fuyuxiang/echo-agent
- Pinned revision: `c2ad97f8`
- Integration: source snapshot tracked directly under `vendor/echo-agent-build/`
- License: Apache License 2.0
- Copyright: Copyright 2023-2026 EchoAgent contributors
- Local license copy: `vendor/echo-agent-build/LICENSE`
- Bundled dependency notices: `vendor/echo-agent-build/third_party/NOTICE`

EchoAgent Desktop consumes selected Rust crates from this `echo-agent` source
snapshot as in-process path dependencies. The maintained snapshot uses
EchoAgent crate names, Rust module names, configuration keys, environment
variables, protocol identifiers, and user-facing terminology throughout.
Modified files are maintained by EchoAgent contributors. Copyright, license,
and NOTICE material remain intact.

Additional dependency notices remain available in the embedded runtime source
tree and its crate-specific license files.

## Eclipse Theia Platform

- Source: https://github.com/eclipse-theia/theia
- Version: `v1.74.0`
- Pinned source revision: `172494ea2564ca07690e230805861146007fae79`
- Integration: source snapshot tracked under `vendor/theia-platform/`
- License: EPL-2.0 OR GPL-2.0-only WITH Classpath-exception-2.0
- Local licenses and notices: `vendor/theia-platform/LICENSE-EPL`,
  `vendor/theia-platform/LICENSE-GPL-2.0-ONLY-CLASSPATH-EXCEPTION`,
  `vendor/theia-platform/NOTICE.md`

EchoAgent adds the `@echoagent/theia-bridge` extension and adapts the browser
example application into the Echo Code IDE. The Theia platform source retains
upstream copyright and license headers. Coding Agent operations use EchoAgent's
existing task and Agent runtime; Theia supplies the IDE surface.

The JSON Schema catalog at
`vendor/theia-platform/packages/core/src/browser/catalog.json` comes from the
official `@theia/core@1.74.0` npm package, matching the vendored source tag.

### VS Code Git and Git Base built-in extensions

- Upstream code: https://github.com/microsoft/vscode/tree/1.95.3/extensions/git
  and https://github.com/microsoft/vscode/tree/1.95.3/extensions/git-base
- Theia's built-in extension packaging: https://github.com/eclipse-theia/vscode-builtin-extensions
- Registry metadata: https://open-vsx.org/api/vscode/git/1.95.3 and
  https://open-vsx.org/api/vscode/git-base/1.95.3
- Pinned VSIX files: `vendor/theia-vsix/vscode.git-1.95.3.vsix` and
  `vendor/theia-vsix/vscode.git-base-1.95.3.vsix`
- SHA-256 (also checked against the registry's published `.sha256` files):
  Git `44909b81d3266860f02733c08475ab5fc66c0c1d7259b3c1b164e783559d9291`;
  Git Base `956c41e4e8377a6b113a6a64fcd5308deaad44a827288f6bfcd5cf1c83037483`
- License: MIT, Copyright Microsoft Corporation. Each VSIX contains
  `extension/LICENSE-vscode.txt`, retained in the staged runtime.
- Compatibility: Git declares VS Code API `^1.5.0` and requires `vscode.git-base`;
  Git Base declares API `0.10.0`. The vendored Theia 1.74.0 declares supported
  VS Code API `1.130.0`; both fixed built-ins use the existing plugin host and
  SCM integration, without restoring the removed `@theia/git` package.

The downloaded built-ins are unchanged. Their presence and artifact validation
do not substitute for native application acceptance of the actual Git provider,
stage/commit actions, or repository isolation.

## Tauri updater plugin

- Source: https://github.com/tauri-apps/plugins-workspace/tree/updater-v2.9.0/plugins/updater
- Version: `2.9.0` (unchanged from the original dependency)
- Integration: source snapshot under `vendor/tauri-plugin-updater/`
- License: Apache-2.0 OR MIT
- Local licenses: `vendor/tauri-plugin-updater/LICENSE_APACHE-2.0`,
  `vendor/tauri-plugin-updater/LICENSE_MIT`
- Local patch and upstream package checksum: `vendor/tauri-plugin-updater/ECHOAGENT_PATCH.md`

EchoAgent adds an opt-in installation method that returns control to the host
after starting a Windows installer, so recording and task persistence can finish
before the desktop application exits. Installer launch failures are returned to
the host. The default plugin API, update signature checks, TLS/CA validation,
and archive extraction protections remain intact.

## auto-launch

- Source: https://github.com/zzzgydi/auto-launch
- Version: `0.5.0` (unchanged transitive dependency of Tauri autostart)
- Integration: source under `vendor/auto-launch/`, desktop Cargo source patch
- License: MIT; local copy `vendor/auto-launch/LICENSE`
- Patch provenance/checksum: `vendor/auto-launch/ECHOAGENT_PATCH.md`

The local patch quotes Windows login command arguments and escapes macOS
LaunchAgent XML, preserving paths with spaces, Chinese and XML characters.
Public APIs and other platform startup mechanisms remain unchanged.

## Node.js runtime

- Source: https://nodejs.org/
- Integration: the platform Node.js executable is staged with the Theia backend
  into `src-tauri/resources/theia/node/` during packaging.
- License: Node.js license and bundled third-party notices, copied from the
  selected Node.js distribution into `theia/node/LICENSE` in the app resources.
- Fallback license copy for the pinned development runtime:
  `vendor/nodejs/LICENSE-22.23.4` or `vendor/nodejs/LICENSE-24.22.1`.

## Dependency security backports

These source copies retain their original licenses. `SOURCE.json` in each
directory records upstream versions, advisories and the local changes.

- `vendor/glib/`: GLib Rust bindings 0.18.5, MIT, including the upstream
  `VariantStrIter` out-pointer fix while retaining GTK3 compatibility.
- `vendor/lru-0.12/` and `vendor/lru-0.16/`: lru 0.12.5 and 0.16.4, MIT,
  including upstream node-detachment and shared-key-reference safety fixes.
- `vendor/theia-platform/security/braces/`: braces 3.0.3, MIT, with bounded
  parsing and AST traversal for deeply nested patterns.
- `vendor/theia-platform/security/sprintf-js/`: sprintf-js 1.0.3, BSD-3-Clause,
  with bounded numeric precision and padding.
- `vendor/theia-platform/security/deepmerge/`: deepmerge 4.3.1, MIT, with
  reserved-key guards for both merge operands to prevent prototype poisoning.

Rust patch entries and npm workspace links select these copies. The original
license files are included alongside the sources; the JavaScript copies are
also staged into the packaged IDE runtime.

`scripts/patch-theia-scanoss.mjs` adapts the installed SCANOSS 0.40.2
(MIT, https://github.com/scanoss/scanoss.js) CommonJS and ESM builds to the
maintained stream-json 3 Node stream API. SCANOSS retains its upstream license;
the patch is applied during source installation, builds and runtime staging.

## async-openai

- Source: https://github.com/our-forks/async-openai
- Pinned revision: `95b52ebdedf42143083cf3d6f0e0be7c84e9c808`
- Integration: build-dependency source under `vendor/async-openai/`
- License: MIT
- Copyright: Copyright 2022 Himanshu Neema
- Local license copy: `vendor/async-openai/LICENSE`

The vendored source contains the `async-openai` and `async-openai-macros`
crates used by the embedded Runtime. Non-build example assets and generated API
reference input are intentionally excluded.

## nucleo

- Source: https://github.com/helix-editor/nucleo
- Pinned revision: `5b74652e482f7c07d827f18c6d21e7540c242c69`
- Integration: build-dependency source under `vendor/nucleo/`
- License: Mozilla Public License 2.0
- Local license copy: `vendor/nucleo/LICENSE`

The vendored source contains the `nucleo` and `nucleo-matcher` crates used by
the embedded Runtime. The upstream license and source form are retained.

## Noto Sans CJK SC

- Source: https://github.com/notofonts/noto-cjk
- Integration: `src-tauri/resources/office/fonts/NotoSansCJKsc-Regular.otf` is
  bundled for offline Chinese PDF export.
- License: SIL Open Font License 1.1
- Local license copy: `src-tauri/resources/office/fonts/LICENSE`

The bundled Office worker uses the npm packages `docx`, `exceljs`, `jszip`, `pptxgenjs`,
`pdf-lib`, `@pdf-lib/fontkit`, and `marked`. The published license files are
staged into `office/licenses/` in the app resources during the build; remaining
MIT license metadata for `@pdf-lib/fontkit` is staged there from its published
package manifest. Exact package versions are pinned in `pnpm-lock.yaml`.
