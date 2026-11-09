// Pin, verify and stage the model resources before Tauri builds the installer.
// Weights stay out of Git; release builds may use an existing offline package.
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import { copyFile, lstat, mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Readable, Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import { promisify } from "node:util";

const project = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const manifestPath = join(project, "vendor/echo-agent-build/crates/codegen/echo-agent-memory/src/model-assets.json");
export const defaultDirectory = join(project, "src-tauri/resources/retrieval-models");
const archiveName = "retrieval-models-v1.tar.gz";
const modelRelease = {
  url: `https://github.com/fuyuxiang/echo-agent-desktop/releases/download/retrieval-models-v1/${archiveName}`,
  size: 303445676,
  sha256: "7842bd91c728a266231170ed6b1f480b17f84aef1d83e8c5cbc8bd9052247d4e",
};
const run = promisify(execFile);

export async function verifyAsset(directory, asset) {
  await verifyFile(join(directory, asset.path), asset);
}

async function verifyFile(path, asset) {
  const info = await lstat(path);
  if (!info.isFile() || info.isSymbolicLink() || info.size !== asset.size) throw new Error(`模型文件缺失或大小错误：${path}`);
  // Reject symlinked role directories as well as symlinked files.
  if (!(await lstat(dirname(path))).isDirectory() || (await lstat(dirname(path))).isSymbolicLink()) throw new Error(`模型目录无效：${dirname(path)}`);
  const hash = createHash("sha256");
  for await (const block of createReadStream(path)) hash.update(block);
  if (hash.digest("hex") !== asset.sha256) throw new Error(`模型 SHA256 校验失败：${path}`);
}

async function downloadRelease(release, workdir, assets, fetcher) {
  const signal = AbortSignal.timeout(900_000);
  const response = await fetcher(release.url, { signal });
  if (!response.ok || !response.body) throw new Error(`GitHub 模型包下载失败（HTTP ${response.status}）：${release.url}`);
  const hash = createHash("sha256");
  let bytes = 0;
  await pipeline(Readable.fromWeb(response.body), new Transform({
    transform(block, _encoding, callback) {
      bytes += block.length;
      if (bytes > release.size) return callback(new Error("GitHub 模型包超过预期大小"));
      hash.update(block);
      callback(null, block);
    },
  }), createWriteStream(join(workdir, archiveName), { flags: "wx" }), { signal });
  if (bytes !== release.size) throw new Error("GitHub 模型包大小错误");
  if (hash.digest("hex") !== release.sha256) throw new Error("GitHub 模型包 SHA256 校验失败");
  // Authenticate the archive before invoking tar, and extract only the pinned model files.
  await run("tar", ["-xzf", archiveName, "--", ...assets.map(asset => asset.path)], { cwd: workdir });
}

export async function prepareModels({ directory = defaultDirectory, source, check = false, offline = false, manifest, release = modelRelease, fetcher = fetch, log = console.log } = {}) {
  const raw = manifest ? JSON.stringify(manifest) : await readFile(manifestPath, "utf8");
  const expected = manifest ?? JSON.parse(raw);
  for (const asset of expected.assets) {
    if (!/^(embedding|reranker)\/(model\.onnx|tokenizer\.json)$/.test(asset.path)
      || !/^[a-f0-9]{40}$/.test(asset.revision) || !/^[a-f0-9]{64}$/.test(asset.sha256)) throw new Error("模型清单格式错误");
  }
  const marker = join(directory, "verified.json");
  if (check) {
    if (JSON.stringify(JSON.parse(await readFile(marker, "utf8"))) !== JSON.stringify(expected)) throw new Error("内置模型清单与代码版本不一致");
    for (const asset of expected.assets) await verifyAsset(directory, asset);
    log(`内置模型校验通过：${directory}`);
    return;
  }
  await mkdir(dirname(directory), { recursive: true });
  const lock = `${directory}.lock`;
  await mkdir(lock).catch(() => { throw new Error(`模型资源正在准备，或上次构建中断；确认没有构建进程后可删除 ${lock}`); });
  try {
    await mkdir(directory, { recursive: true });
    if ((await lstat(directory)).isSymbolicLink()) throw new Error("模型资源目录不能是符号链接");
    let input = source;
    for (const asset of expected.assets) {
      try { await verifyAsset(directory, asset); continue; } catch { /* Stage a verified replacement. */ }
      await rm(marker, { force: true });
      if (!input) {
        if (offline) throw new Error(`离线构建缺少 ${asset.path}；请设置 ECHO_RETRIEVAL_MODELS_SOURCE 或先运行 pnpm retrieval:prepare`);
        log(`从 GitHub Release 下载固定版本模型：${release.url}`);
        await downloadRelease(release, lock, expected.assets, fetcher);
        input = lock;
      }
      const target = join(directory, asset.path);
      await mkdir(dirname(target), { recursive: true });
      if ((await lstat(dirname(target))).isSymbolicLink()) throw new Error("模型资源目录不能是符号链接");
      const part = `${target}.part`;
      await rm(part, { force: true });
      try {
        await verifyAsset(input, asset);
        await copyFile(join(input, asset.path), part);
        // Verify the staged bytes even after validating their source.
        await verifyFile(part, asset);
        await rename(part, target);
      } finally { await rm(part, { force: true }); }
    }
    // Do not touch a matching marker on every build (avoids needless relinking).
    const previous = await readFile(marker, "utf8").catch(() => "");
    if (previous !== raw) {
      await writeFile(`${marker}.part`, raw);
      await rename(`${marker}.part`, marker);
    }
    log(`内置模型已准备：${expected.assets.reduce((sum, asset) => sum + asset.size, 0)} bytes / ${directory}`);
  } finally { await rm(lock, { recursive: true, force: true }); }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2);
    const value = flag => { const i = args.indexOf(flag); if (i < 0) return undefined; if (!args[i + 1] || args[i + 1].startsWith("--")) throw new Error(`${flag} 需要路径`); return resolve(args[i + 1]); };
    const allowed = new Set(["--check", "--offline", "--source", "--directory"]);
    for (let i = 0; i < args.length; i++) { if (!allowed.has(args[i])) throw new Error(`未知参数：${args[i]}`); if (["--source", "--directory"].includes(args[i])) i++; }
    await prepareModels({ check: args.includes("--check"), offline: args.includes("--offline"), directory: value("--directory"), source: value("--source") ?? process.env.ECHO_RETRIEVAL_MODELS_SOURCE });
  } catch (error) { console.error(`内置检索模型准备失败：${error.message}`); process.exitCode = 1; }
}
