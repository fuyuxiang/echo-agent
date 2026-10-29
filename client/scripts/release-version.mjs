import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const PROJECT_ROOT = path.resolve(SCRIPT_DIR, "..");
const SEMVER_RE = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;

function fail(message) {
  throw new Error(message);
}

export function validateVersion(version) {
  if (!SEMVER_RE.test(version)) {
    fail(`版本号不是合法的 SemVer：${version}`);
  }
  return version;
}

function readJson(filePath) {
  try {
    return JSON.parse(fs.readFileSync(filePath, "utf8"));
  } catch (error) {
    fail(`无法读取 ${path.relative(PROJECT_ROOT, filePath)}：${error.message}`);
  }
}

function cargoPackageVersion(text, fileName) {
  const packageMatch = text.match(/^\[package\][\r\n]+([\s\S]*?)(?=[\r\n]+\[|$)/);
  if (!packageMatch) {
    fail(`${fileName} 中没有找到 [package]`);
  }
  const versionMatch = packageMatch[1].match(/^version\s*=\s*"([^"]+)"\s*$/m);
  if (!versionMatch) {
    fail(`${fileName} 的 [package] 中没有找到 version`);
  }
  return versionMatch[1];
}

function cargoLockRootVersion(text) {
  const blocks = text.split(/(?=^\[\[package\]\]\s*$)/m);
  const rootBlock = blocks.find((block) => /^name\s*=\s*"echoagent"\s*$/m.test(block));
  if (!rootBlock) {
    fail("src-tauri/Cargo.lock 中没有找到 echoagent 包");
  }
  const versionMatch = rootBlock.match(/^version\s*=\s*"([^"]+)"\s*$/m);
  if (!versionMatch) {
    fail("src-tauri/Cargo.lock 的 echoagent 包中没有找到 version");
  }
  return versionMatch[1];
}

export function readVersions(root = PROJECT_ROOT) {
  const packageJson = readJson(path.join(root, "package.json"));
  const tauriConfig = readJson(path.join(root, "src-tauri", "tauri.conf.json"));
  const cargoTomlText = fs.readFileSync(path.join(root, "src-tauri", "Cargo.toml"), "utf8");
  const cargoLockText = fs.readFileSync(path.join(root, "src-tauri", "Cargo.lock"), "utf8");

  return {
    "package.json": packageJson.version,
    "src-tauri/tauri.conf.json": tauriConfig.version,
    "src-tauri/Cargo.toml": cargoPackageVersion(cargoTomlText, "src-tauri/Cargo.toml"),
    "src-tauri/Cargo.lock": cargoLockRootVersion(cargoLockText),
  };
}

export function checkVersions(expected, root = PROJECT_ROOT) {
  const versions = readVersions(root);
  for (const [fileName, version] of Object.entries(versions)) {
    validateVersion(version);
    if (expected && version !== expected) {
      fail(`${fileName} 的版本 ${version} 与期望版本 ${expected} 不一致`);
    }
  }

  const unique = new Set(Object.values(versions));
  if (unique.size !== 1) {
    const details = Object.entries(versions)
      .map(([fileName, version]) => `${fileName}=${version}`)
      .join("，");
    fail(`项目版本不一致：${details}`);
  }

  return Object.values(versions)[0];
}

function replaceRootJsonVersion(text, version, fileName) {
  const parsed = JSON.parse(text);
  if (typeof parsed.version !== "string") {
    fail(`${fileName} 没有根级 version 字段`);
  }
  const pattern = /^(\s*"version"\s*:\s*)"[^"]+"(\s*,?\s*)$/m;
  if (!pattern.test(text)) {
    fail(`${fileName} 的根级 version 格式无法安全更新`);
  }
  return text.replace(pattern, `$1"${version}"$2`);
}

function replaceCargoPackageVersion(text, version, fileName) {
  const packagePattern = /(^\[package\][\r\n]+)([\s\S]*?)(?=[\r\n]+\[|$)/;
  const packageMatch = text.match(packagePattern);
  if (!packageMatch) {
    fail(`${fileName} 中没有找到 [package]`);
  }
  const versionPattern = /^(version\s*=\s*)"[^"]+"(\s*)$/m;
  if (!versionPattern.test(packageMatch[2])) {
    fail(`${fileName} 的 [package] version 无法安全更新`);
  }
  const updatedSection = packageMatch[2].replace(
    versionPattern,
    `$1"${version}"$2`,
  );
  return text.replace(packagePattern, `$1${updatedSection}`);
}

function replaceCargoLockRootVersion(text, version) {
  const blocks = text.split(/(?=^\[\[package\]\]\s*$)/m);
  let replacements = 0;
  const updated = blocks.map((block) => {
    if (!/^name\s*=\s*"echoagent"\s*$/m.test(block)) {
      return block;
    }
    replacements += 1;
    return block.replace(/^(version\s*=\s*)"[^"]+"(\s*)$/m, `$1"${version}"$2`);
  });
  if (replacements !== 1) {
    fail(`src-tauri/Cargo.lock 中期望 1 个 echoagent 包，实际找到 ${replacements} 个`);
  }
  return updated.join("");
}

function atomicWrite(filePath, content) {
  const temporaryPath = `${filePath}.release-version-${process.pid}`;
  fs.writeFileSync(temporaryPath, content, "utf8");
  fs.renameSync(temporaryPath, filePath);
}

export function setVersion(version, root = PROJECT_ROOT) {
  validateVersion(version);

  const packagePath = path.join(root, "package.json");
  const tauriPath = path.join(root, "src-tauri", "tauri.conf.json");
  const cargoPath = path.join(root, "src-tauri", "Cargo.toml");
  const lockPath = path.join(root, "src-tauri", "Cargo.lock");

  // Compute and validate every edit before touching any source file.
  const updates = [
    [
      packagePath,
      replaceRootJsonVersion(
        fs.readFileSync(packagePath, "utf8"),
        version,
        "package.json",
      ),
    ],
    [
      tauriPath,
      replaceRootJsonVersion(
        fs.readFileSync(tauriPath, "utf8"),
        version,
        "src-tauri/tauri.conf.json",
      ),
    ],
    [
      cargoPath,
      replaceCargoPackageVersion(
        fs.readFileSync(cargoPath, "utf8"),
        version,
        "src-tauri/Cargo.toml",
      ),
    ],
    [
      lockPath,
      replaceCargoLockRootVersion(fs.readFileSync(lockPath, "utf8"), version),
    ],
  ];
  for (const [filePath, content] of updates) {
    atomicWrite(filePath, content);
  }

  return checkVersions(version, root);
}

function usage() {
  process.stderr.write(
    "用法：node scripts/release-version.mjs check [期望版本] | set <版本> | validate <版本>\n",
  );
}

if (import.meta.main) {
  try {
    const [command, argument, ...extra] = process.argv.slice(2);
    if (extra.length > 0) {
      usage();
      process.exitCode = 2;
    } else if (command === "check" && argument === undefined) {
      process.stdout.write(`${checkVersions()}\n`);
    } else if (command === "check") {
      validateVersion(argument);
      process.stdout.write(`${checkVersions(argument)}\n`);
    } else if (command === "set" && argument) {
      process.stdout.write(`${setVersion(argument)}\n`);
    } else if (command === "validate" && argument) {
      process.stdout.write(`${validateVersion(argument)}\n`);
    } else {
      usage();
      process.exitCode = 2;
    }
  } catch (error) {
    process.stderr.write(`错误：${error.message}\n`);
    process.exitCode = 1;
  }
}
