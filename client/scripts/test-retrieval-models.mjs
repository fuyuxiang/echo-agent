import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { prepareModels } from "./prepare-retrieval-models.mjs";

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), "echo-model-packaging-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const source = join(root, "source"), directory = join(root, "resources");
  const manifest = { pipelineVersion: "test", assets: [] };
  for (const role of ["embedding", "reranker"]) {
    await mkdir(join(source, role), { recursive: true });
    for (const name of ["model.onnx", "tokenizer.json"]) {
      const data = Buffer.from(`${role}/${name}`);
      const path = `${role}/${name}`;
      manifest.assets.push({ path, size: data.length, sha256: createHash("sha256").update(data).digest("hex"), repo: "fixed/model", revision: "a".repeat(40), remote: path });
      await writeFile(join(source, path), data);
    }
  }
  return { source, directory, manifest, log() {}, fetcher() { throw new Error("network must not be used"); } };
}

async function releaseFixture({ source, manifest }) {
  const name = "test-models.tar.gz";
  execFileSync("tar", ["-czf", name, "--", ...manifest.assets.map(asset => asset.path)], { cwd: source });
  const bytes = await readFile(join(source, name));
  const release = {
    url: "https://github.com/fuyuxiang/echo-agent-desktop/releases/download/retrieval-models-v1/retrieval-models-v1.tar.gz",
    size: bytes.length,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  };
  return { bytes, release };
}

test("offline build stages only the pinned files; subsequent build and installed-resource verification need no network", async t => {
  const options = await fixture(t);
  await writeFile(join(options.source, "unrelated-secret.txt"), "never package this");
  await prepareModels(options);
  await assert.rejects(readFile(join(options.directory, "unrelated-secret.txt")));
  await prepareModels({ ...options, source: undefined, offline: true });
  await prepareModels({ ...options, check: true });
  // Installed resources are validated using the same fixed manifest.
  assert.deepEqual(JSON.parse(await readFile(join(options.directory, "verified.json"))), options.manifest);
});

test("same-sized corrupt weights cannot pass checks or leave a ready marker after a failed repair", async t => {
  const options = await fixture(t);
  await prepareModels(options);
  const asset = options.manifest.assets[0];
  await writeFile(join(options.directory, asset.path), Buffer.alloc(asset.size));
  await assert.rejects(prepareModels({ ...options, check: true }), /SHA256/);
  await assert.rejects(prepareModels({ ...options, source: undefined, offline: true }), /离线构建缺少/);
  await assert.rejects(readFile(join(options.directory, "verified.json")));
  // A valid offline source repairs the incomplete staging area.
  await prepareModels(options);
  await prepareModels({ ...options, check: true });
});

test("bad offline packages fail closed and leave no usable readiness marker", async t => {
  const options = await fixture(t);
  const asset = options.manifest.assets[0];
  await writeFile(join(options.source, asset.path), Buffer.alloc(asset.size));
  await assert.rejects(prepareModels(options), /SHA256/);
  await assert.rejects(readFile(join(options.directory, "verified.json")));
});

test("one GitHub download repairs missing files and preserves a verified local file", async t => {
  const options = await fixture(t);
  await prepareModels(options);
  const [valid, ...missing] = options.manifest.assets;
  for (const asset of missing) await rm(join(options.directory, asset.path));
  const validPath = join(options.directory, valid.path);
  const original = await stat(validPath);
  const { bytes, release } = await releaseFixture(options);
  const urls = [];
  await prepareModels({ ...options, source: undefined, release, fetcher: async url => {
    urls.push(url);
    return new Response(bytes);
  } });
  assert.deepEqual(urls, [release.url]);
  assert.equal((await stat(validPath)).mtimeMs, original.mtimeMs);
  await prepareModels({ ...options, check: true });
});

test("truncated, oversized or tampered archives cannot leave a ready marker", async t => {
  const options = await fixture(t);
  const { bytes, release } = await releaseFixture(options);
  for (const [invalid, error] of [
    [bytes.subarray(0, -1), /大小错误/],
    [Buffer.concat([bytes, Buffer.of(0)]), /超过预期/],
    [Buffer.alloc(bytes.length), /模型包 SHA256/],
  ]) {
    await assert.rejects(prepareModels({ ...options, source: undefined, release,
      fetcher: async () => new Response(invalid),
    }), error);
    await assert.rejects(readFile(join(options.directory, "verified.json")));
  }
});

test("a verified archive still has to match each pinned model checksum", async t => {
  const options = await fixture(t);
  const asset = options.manifest.assets[0];
  await writeFile(join(options.source, asset.path), Buffer.alloc(asset.size));
  const { bytes, release } = await releaseFixture(options);
  await assert.rejects(prepareModels({ ...options, source: undefined, release,
    fetcher: async () => new Response(bytes),
  }), /模型 SHA256/);
  await assert.rejects(readFile(join(options.directory, "verified.json")));
});

test("Tauri packages the model directory and prepares it for development and release", async () => {
  const config = JSON.parse(await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url)));
  assert.equal(config.bundle.resources["resources/retrieval-models"], "retrieval-models");
  for (const hook of ["beforeDevCommand", "beforeBuildCommand"]) assert.match(config.build[hook], /^pnpm retrieval:prepare &&/);
  assert.equal(config.build.beforeBundleCommand, "pnpm retrieval:check");
});
