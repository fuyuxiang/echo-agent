import { execFile, spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { copyFile, lstat, mkdir, mkdtemp, open, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { verifyAsset } from "./prepare-retrieval-models.mjs";
import { withMeetingSpeechLock } from "./lib/meeting-speech-lock.mjs";

const run = promisify(execFile);
const project = fileURLToPath(new URL("../src-tauri/", import.meta.url));
const manifestPath = join(project, "meeting-speech/model-assets.json");
const directory = join(project, "resources/meeting-speech");
const cache = join(project, "target/meeting-speech-assets");
const binaries = join(project, "binaries");
const release = "https://github.com/fuyuxiang/echo-agent-desktop/releases/download/meeting-speech-models-v1";
const runtimeVersion = "v1.13.8";
const xasr = "sherpa-onnx-x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03";
const pyannote = "sherpa-onnx-pyannote-segmentation-3-0";
const sources = {
  xasr: {
    archive: xasr, size: 136396739,
    sha256: "5d02c36d7b44e886b7c8f0d8e051f8713acab96c264bb6ef9e718be39a6a2224",
  },
  pyannote: {
    archive: pyannote, size: 6958444,
    sha256: "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488",
  },
  eres2net: {
    file: "3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx",
    size: 39593761, sha256: "1a331345f04805badbb495c775a6ddffcdd1a732567d5ec8b3d5749e3c7a5e4b",
  },
};
const platforms = {
  "x86_64-apple-darwin": ["osx-x64-static-no-tts-lib", 19433402, "8ffc3ede9f997fec547b5c99b8e2073b8e04fd48a5f65e1a6d5d314ad0106ad9"],
  "aarch64-apple-darwin": ["osx-arm64-static-no-tts-lib", 19756929, "3d7f9b8a496694af13d9802c33b8133231e397bdef302f543d19468765e83136"],
  "x86_64-pc-windows-msvc": ["win-x64-static-MD-Release-no-tts-lib", 94095902, "542348e56e827b59c6d249fd0dfd38dc34b7bd0c521a1ebe9eb6e2db154fa15d"],
  "x86_64-unknown-linux-gnu": ["linux-x64-static-no-tts-lib", 21755415, "36f2ebd0b9aa09248ff6461a30dcb7edfe7eecf2deb67dfb0ceb3aaf4906051f"],
};

async function runPreparationPhase(label, command, args, { timeout, env = process.env, cwd, stdin = "ignore" }) {
  console.log(label);
  // Forward progress and diagnostics as they happen. In particular, Cargo's
  // lock and compiler messages must survive a cancelled packaging job.
  const child = spawn(command, args, {
    env, cwd, windowsHide: true, stdio: [stdin, "inherit", "inherit"],
    signal: AbortSignal.timeout(timeout), killSignal: "SIGKILL",
  });
  await new Promise((resolve, reject) => {
    let failure;
    child.once("error", error => { failure = error; });
    // Even after an abort, wait for termination before deleting the child's
    // working directory. Otherwise Windows can mask the real error with EBUSY.
    child.once("close", (code, signal) => {
      if (failure) reject(new Error(
        failure.name === "AbortError" ? `${label}超时（${timeout / 1000} 秒）` : `${label}：${failure.message}`,
        { cause: failure },
      ));
      else if (code === 0) resolve();
      else reject(new Error(`${label}失败（${signal ?? `退出码 ${code}`}）`));
    });
  });
}

async function extractBzipArchive(label, archive, directory, members = []) {
  if (process.platform === "win32") {
    // Tauri's Windows build environment can put Git's MSYS tar/bzip2 first
    // in PATH. They do not share Node's native Windows handle conventions.
    // The Windows-provided bsdtar supports local drive paths and bzip2 itself.
    const windows = process.env.SystemRoot ?? process.env.WINDIR;
    if (!windows) throw new Error("无法定位 Windows 系统归档工具：缺少 SystemRoot");
    const tar = join(windows, "System32", "tar.exe");
    await runPreparationPhase(`${label}（Windows 系统 tar）`, tar,
      ["-xf", archive, ...(members.length ? ["--", ...members] : [])],
      { cwd: directory, timeout: 600_000 });
    return;
  }
  // A source path containing a colon can select a remote archive in GNU tar.
  // Give POSIX tar a local file as standard input and explicitly select bzip2
  // for input without a filename.
  const input = await open(archive, "r");
  try {
    await runPreparationPhase(label, "tar", ["-xjf", "-", ...(members.length ? ["--", ...members] : [])], {
      cwd: directory, stdin: input.fd, timeout: 600_000,
    });
  } finally {
    await input.close();
  }
}

async function checkedFile(path, { size, sha256 }) {
  const info = await lstat(path);
  if (!info.isFile() || info.size !== size) throw new Error(`语音资源大小错误：${path}`);
  const hash = createHash("sha256");
  for await (const bytes of createReadStream(path)) hash.update(bytes);
  if (hash.digest("hex") !== sha256) throw new Error(`语音资源 SHA-256 错误：${path}`);
}

async function sourceFile(spec, input, offline) {
  const file = spec.file ?? `${spec.archive}.tar.bz2`;
  const path = join(input ?? cache, file);
  if (input || offline) {
    await checkedFile(path, spec);
    return path;
  }
  try {
    await checkedFile(path, spec);
    return path;
  } catch { /* Download a verified replacement. */ }
  const temp = await mkdtemp(join(cache, "download-"));
  try {
    const part = join(temp, file);
    const url = `${release}/${file}`;
    await runPreparationPhase(`下载语音资产 ${file}`, "curl", [
      "--fail", "--location", "--silent", "--show-error", "--retry", "2", "--retry-all-errors",
      "--connect-timeout", "30", "--speed-limit", "8192", "--speed-time", "60",
      "--max-time", "900", "--retry-max-time", "900", "--output", part, url,
    ], { timeout: 960_000 });
    await checkedFile(part, spec);
    await rm(path, { force: true });
    await rename(part, path);
    return path;
  } finally {
    await rm(temp, { recursive: true, force: true });
  }
}

async function checkModels(root, manifest) {
  if (JSON.stringify(JSON.parse(await readFile(join(root, "verified.json"), "utf8"))) !== JSON.stringify(manifest)) {
    throw new Error("语音模型清单版本不一致");
  }
  for (const asset of manifest.assets) await verifyAsset(root, asset);
}

async function checkInstalledSpeech(app, manifest) {
  const appDirectory = dirname(resolve(app));
  const models = process.platform === "darwin"
    ? resolve(appDirectory, "../Resources/meeting-speech")
    : join(appDirectory, "meeting-speech");
  await checkModels(models, manifest);
  const samples = 16_000;
  const pcm = Buffer.alloc(8 + samples * 2);
  pcm.writeUInt32LE(samples, 4);
  // A nonzero sample exercises inference instead of the silence fast path.
  pcm.writeInt16LE(1, 8);
  const worker = join(appDirectory, process.platform === "win32" ? "echo-meeting-speech.exe" : "echo-meeting-speech");
  const result = spawnSync(worker, [], {
    cwd: models, input: pcm, encoding: "utf8", timeout: 90_000, windowsHide: true,
  });
  if (result.error || result.status !== 0) {
    throw new Error(`安装包语音进程无法运行：${result.error ?? (result.stderr || result.signal || result.status)}`);
  }
  const [ready, response] = result.stdout.trimEnd().split(/\r?\n/);
  if (ready !== "ready" || !response || !Array.isArray(JSON.parse(response).Ok?.pieces)) {
    throw new Error("安装包语音进程未完成离线推理");
  }
}

async function stageModels(manifest, raw, input, offline) {
  try {
    await checkModels(directory, manifest);
    return;
  } catch { /* Stage verified resources. */ }
  const scratch = await mkdtemp(join(cache, "models-"));
  try {
    for (const [key, source] of Object.entries(sources)) {
      const archive = await sourceFile(source, input, offline);
      const assets = manifest.assets.filter(asset => asset.source === key);
      if (source.archive) {
        await extractBzipArchive(`解包语音模型 ${source.archive}`, archive, scratch,
          assets.map(asset => `${source.archive}/${asset.member}`));
      }
      for (const asset of assets) {
        await copyFile(source.archive ? join(scratch, source.archive, asset.member) : archive, join(scratch, asset.path));
        await verifyAsset(scratch, asset);
      }
    }
    await mkdir(directory, { recursive: true });
    if (!(await lstat(directory)).isDirectory()) throw new Error("语音模型资源目录无效");
    await rm(join(directory, "verified.json"), { force: true });
    for (const asset of manifest.assets) {
      const path = join(directory, asset.path);
      await rm(path, { force: true });
      await rename(join(scratch, asset.path), path);
    }
    await writeFile(join(scratch, "verified.json"), raw);
    await rename(join(scratch, "verified.json"), join(directory, "verified.json"));
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

async function stageWorker(target, input, offline) {
  const platform = platforms[target];
  if (!platform) throw new Error(`内置语音暂不支持构建目标 ${target}`);
  const [variant, size, sha256] = platform;
  const archive = `sherpa-onnx-${runtimeVersion}-${variant}`;
  const path = await sourceFile({ archive, size, sha256 }, input, offline);
  const library = join(cache, archive, "lib");
  const marker = join(cache, archive, "archive.sha256");
  if (await readFile(marker, "utf8").catch(() => "") !== sha256) {
    const scratch = await mkdtemp(join(cache, "runtime-"));
    try {
      await extractBzipArchive(`解包语音运行库 ${archive}`, path, scratch);
      await writeFile(join(scratch, archive, "archive.sha256"), sha256);
      await rm(join(cache, archive), { recursive: true, force: true });
      await rename(join(scratch, archive), join(cache, archive));
    } finally {
      await rm(scratch, { recursive: true, force: true });
    }
  }
  const build = join(project, "target/meeting-speech");
  const extension = target.includes("windows") ? ".exe" : "";
  await runPreparationPhase(`编译语音进程 ${target}`, "cargo", ["build", "--manifest-path", join(project, "meeting-speech/Cargo.toml"),
    "--locked", "--release", "--target", target, ...(offline ? ["--offline"] : [])], {
    env: { ...process.env, CARGO_TARGET_DIR: build, SHERPA_ONNX_LIB_DIR: library },
    timeout: 1_800_000,
  });
  await mkdir(binaries, { recursive: true });
  const staged = join(binaries, `echo-meeting-speech-${target}${extension}`);
  const binary = join(build, target, "release", `echo-meeting-speech${extension}`);
  const built = await lstat(binary);
  const previous = await lstat(staged).catch(() => null);
  if (!previous || previous.size !== built.size || previous.mtimeMs < built.mtimeMs) {
    await copyFile(binary, `${staged}.part`);
    await rm(staged, { force: true });
    await rename(`${staged}.part`, staged);
  }
}

const args = process.argv.slice(2);
const option = name => {
  const index = args.indexOf(name);
  if (index < 0) return undefined;
  if (!args[index + 1] || args[index + 1].startsWith("--")) throw new Error(`${name} 需要参数`);
  return args[index + 1];
};

try {
  const raw = await readFile(manifestPath, "utf8");
  const manifest = JSON.parse(raw);
  const installedApp = option("--installed-app");
  if (installedApp) {
    await checkInstalledSpeech(installedApp, manifest);
    console.log("安装包语音资源和离线推理校验通过");
  } else if (args.includes("--check")) {
    await checkModels(resolve(option("--directory") ?? directory), manifest);
    console.log("内置录音转写模型校验通过");
  } else {
    const input = option("--source") ?? process.env.ECHO_MEETING_SPEECH_SOURCE;
    const offline = args.includes("--offline") || process.env.CARGO_NET_OFFLINE === "true";
    const platform = { darwin: "apple-darwin", windows: "pc-windows-msvc", linux: "unknown-linux-gnu" }[
      process.env.TAURI_ENV_PLATFORM
    ];
    const tauriTarget = platform && process.env.TAURI_ENV_ARCH
      ? `${process.env.TAURI_ENV_ARCH}-${platform}`
      : undefined;
    let target = option("--target") ?? process.env.CARGO_BUILD_TARGET ?? tauriTarget;
    if (!target) {
      console.log("检测语音进程的 Rust 构建目标");
      target = (await run("rustc", ["-vV"], { timeout: 120_000, windowsHide: true })).stdout.match(/^host: (.+)$/m)?.[1];
    }
    if (!platforms[target]) throw new Error(`内置语音暂不支持构建目标 ${target ?? "（未能检测）"}`);
    console.log(`校验语音模型与运行库：${target}`);
    // This lock is workspace state, not a restorable build cache entry.
    await withMeetingSpeechLock(join(project, ".meeting-speech-prepare"), async () => {
      await mkdir(cache, { recursive: true });
      await stageModels(manifest, raw, input && resolve(input), offline);
      await stageWorker(target, input && resolve(input), offline);
      console.log(`内置录音转写已准备：${target}`);
    });
  }
} catch (error) {
  console.error(`内置录音转写准备失败：${error.message}`);
  process.exitCode = 1;
}
