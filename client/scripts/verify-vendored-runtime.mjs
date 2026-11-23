#!/usr/bin/env node

import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, extname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const runtimeRoot = join(projectRoot, "vendor", "echo-agent-build");
const manifestPath = join(runtimeRoot, "ECHOAGENT_VENDOR.json");
const requiredFiles = [
  "Cargo.toml",
  "LICENSE",
  "third_party/NOTICE",
  "crates/codegen/echo-agent-acp/Cargo.toml",
  "crates/codegen/echo-agent-runtime/Cargo.toml",
  "crates/codegen/echo-agent-tools/Cargo.toml",
  "crates/common/echo-agent-tool-runtime/Cargo.toml",
  "crates/common/echo-agent-tool-protocol/Cargo.toml",
  "crates/common/echo-agent-tool-types/Cargo.toml",
];
const vendoredDependencies = [
  {
    name: "async-openai",
    root: join(projectRoot, "vendor", "async-openai"),
    revision: "95b52ebdedf42143083cf3d6f0e0be7c84e9c808",
    license: "MIT",
    requiredFiles: [
      "Cargo.toml",
      "LICENSE",
      "async-openai/Cargo.toml",
      "async-openai/src/lib.rs",
      "async-openai-macros/Cargo.toml",
      "async-openai-macros/src/lib.rs",
    ],
  },
  {
    name: "nucleo",
    root: join(projectRoot, "vendor", "nucleo"),
    revision: "5b74652e482f7c07d827f18c6d21e7540c242c69",
    license: "MPL-2.0",
    requiredFiles: [
      "Cargo.toml",
      "LICENSE",
      "src/lib.rs",
      "matcher/Cargo.toml",
      "matcher/LICENSE",
      "matcher/src/lib.rs",
    ],
  },
];
const approvedThirdPartyPackages = new Set([
  "dagre_rust",
  "graphlib_rust",
  "mermaid-to-svg",
  "ordered_hashmap",
]);
const legacyBrandTokens = [["g", "rok"].join(""), ["x", "ai"].join("")];
const skippedTextExtensions = new Set([
  ".gif",
  ".icns",
  ".ico",
  ".jpeg",
  ".jpg",
  ".png",
  ".svg",
  ".webp",
]);

function fail(message) {
  process.stderr.write(`[ERROR] ${message}\n`);
  process.exit(1);
}

function verifyNoGitCargoSources(root) {
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    if (entry.name === ".git" || entry.name === "target" || entry.name === "node_modules") continue;
    const path = join(root, entry.name);
    if (entry.isSymbolicLink()) {
      fail(`Vendored 源码不得包含符号链接：${path.slice(projectRoot.length + 1)}`);
    }
    if (entry.isDirectory()) {
      verifyNoGitCargoSources(path);
      continue;
    }
    if (entry.name !== "Cargo.toml" && entry.name !== "Cargo.lock") continue;
    const content = readFileSync(path, "utf8");
    if (/(?:^|[,{\s])git\s*=/m.test(content) || /^source\s*=\s*"git\+/m.test(content)) {
      fail(`Cargo 仍包含外部 Git 源码依赖：${path.slice(projectRoot.length + 1)}`);
    }
  }
}

function isProvenanceFile(relativePath) {
  const normalized = relativePath.replaceAll("\\", "/");
  const basename = normalized.split("/").at(-1) ?? "";
  return (
    basename === "LICENSE" ||
    basename === "LICENCE" ||
    basename === "NOTICE" ||
    basename.startsWith("LICENSE.") ||
    basename.startsWith("NOTICE.") ||
    basename.includes("THIRD_PARTY") ||
    basename.includes("THIRD-PARTY") ||
    basename === "ECHOAGENT_VENDOR.json" ||
    basename === "ECHOAGENT_VENDOR.md"
  );
}

function verifyRuntimeBranding(root) {
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    if (entry.name === ".git" || entry.name === "target") continue;
    const path = join(root, entry.name);
    const relativePath = relative(runtimeRoot, path).replaceAll("\\", "/");
    const lowerPath = relativePath.toLowerCase();

    if (legacyBrandTokens.some((token) => lowerPath.includes(token))) {
      fail(`Vendored Runtime 路径仍包含旧品牌：${relativePath}`);
    }
    if (entry.isSymbolicLink()) {
      fail(`Vendored Runtime 不得包含符号链接：${relativePath}`);
    }
    if (entry.isDirectory()) {
      verifyRuntimeBranding(path);
      continue;
    }
    if (isProvenanceFile(relativePath) || skippedTextExtensions.has(extname(entry.name))) {
      continue;
    }

    const bytes = readFileSync(path);
    if (bytes.includes(0)) continue;
    const content = bytes.toString("utf8");
    const legacyProduct = new RegExp(legacyBrandTokens[0], "i");
    const legacyCompany = new RegExp(
      `(?:^|[^a-z0-9])${legacyBrandTokens[1]}(?:$|[^a-z0-9])|space${legacyBrandTokens[1]}`,
      "im",
    );
    if (legacyProduct.test(content) || legacyCompany.test(content)) {
      fail(`Vendored Runtime 源码仍包含旧品牌：${relativePath}`);
    }

    if (entry.name === "Cargo.toml") {
      const packageName = content.match(/^\s*name\s*=\s*"([^"]+)"/m)?.[1];
      if (
        packageName &&
        !packageName.startsWith("echo-agent-") &&
        !approvedThirdPartyPackages.has(packageName)
      ) {
        fail(`Vendored Runtime crate 未使用 echo-agent- 前缀：${packageName} (${relativePath})`);
      }
    }
  }
}

if (!existsSync(manifestPath)) {
  fail("Vendored Runtime 元数据缺失：vendor/echo-agent-build/ECHOAGENT_VENDOR.json");
}

let manifest;
try {
  manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
} catch (error) {
  fail(`Vendored Runtime 元数据无效：${error instanceof Error ? error.message : String(error)}`);
}

if (manifest.integrationMode !== "vendored-source") {
  fail("Vendored Runtime integrationMode 必须为 vendored-source");
}
if (manifest.runtimeName !== "echo-agent") {
  fail("Vendored Runtime runtimeName 必须为 echo-agent");
}
if (manifest.sourceRepository !== "https://github.com/fuyuxiang/echo-agent.git") {
  fail("Vendored Runtime sourceRepository 必须指向 fuyuxiang/echo-agent");
}
if (!/^[0-9a-f]{40}$/i.test(manifest.sourceRevision ?? "")) {
  fail("Vendored Runtime sourceRevision 必须是完整的 40 位 Git 提交哈希");
}
if (manifest.license !== "Apache-2.0") {
  fail("Vendored Runtime 许可证元数据必须为 Apache-2.0");
}
if (manifest.namespace !== "echo.agent") {
  fail("Vendored Runtime 协议命名空间元数据必须为 echo.agent");
}
if (manifest.localRoot !== "vendor/echo-agent-build") {
  fail("Vendored Runtime localRoot 必须为 vendor/echo-agent-build");
}
if (manifest.packagePrefix !== "echo-agent-") {
  fail("Vendored Runtime packagePrefix 必须为 echo-agent-");
}
if (existsSync(join(runtimeRoot, ".git"))) {
  fail("vendor/echo-agent-build 仍包含独立 Git 元数据，不能作为主仓库普通源码管理");
}

for (const relativePath of requiredFiles) {
  if (!existsSync(join(runtimeRoot, relativePath))) {
    fail(`Vendored Runtime 源码不完整，缺少：${relativePath}`);
  }
}

for (const dependency of vendoredDependencies) {
  const sourcePath = join(dependency.root, "SOURCE.json");
  if (!existsSync(sourcePath)) {
    fail(`Vendored 依赖元数据缺失：${dependency.name}/SOURCE.json`);
  }
  let source;
  try {
    source = JSON.parse(readFileSync(sourcePath, "utf8"));
  } catch (error) {
    fail(
      `Vendored 依赖元数据无效：${dependency.name}: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
  if (
    source.integrationMode !== "vendored-source" ||
    source.upstreamRevision !== dependency.revision ||
    source.license !== dependency.license
  ) {
    fail(`Vendored 依赖来源与锁定版本不一致：${dependency.name}`);
  }
  if (existsSync(join(dependency.root, ".git"))) {
    fail(`Vendored 依赖不得包含独立 Git 元数据：${dependency.name}`);
  }
  for (const relativePath of dependency.requiredFiles) {
    if (!existsSync(join(dependency.root, relativePath))) {
      fail(`Vendored 依赖源码不完整：${dependency.name}/${relativePath}`);
    }
  }
}

verifyNoGitCargoSources(join(projectRoot, "src-tauri"));
verifyNoGitCargoSources(join(projectRoot, "vendor"));
verifyRuntimeBranding(runtimeRoot);

process.stdout.write(
  `[OK] Vendored Runtime ${manifest.sourceRevision.slice(0, 8)} 及 Git 源码依赖已就绪，由主仓库直接管理。\n`,
);
