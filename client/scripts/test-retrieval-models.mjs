import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
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

test("truncated or oversized downloads cannot produce a distributable package", async t => {
  for (const extra of [-1, 1]) {
    const options = await fixture(t);
    await assert.rejects(prepareModels({ ...options, source: undefined,
      fetcher: async () => new Response(Buffer.alloc(options.manifest.assets[0].size + extra)),
    }), /大小错误|超过预期/);
    await assert.rejects(readFile(join(options.directory, "verified.json")));
  }
});

test("Tauri packages the model directory and prepares it for development and release", async () => {
  const config = JSON.parse(await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url)));
  assert.equal(config.bundle.resources["resources/retrieval-models"], "retrieval-models");
  for (const hook of ["beforeDevCommand", "beforeBuildCommand"]) assert.match(config.build[hook], /^pnpm retrieval:prepare &&/);
  assert.equal(config.build.beforeBundleCommand, "pnpm retrieval:check");
});
