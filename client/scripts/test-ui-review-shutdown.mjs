import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { dirname } from "node:path";
import test from "node:test";
import { createUiReviewServer } from "./ui-review-runtime.mjs";

for (const baseline of [false, true]) {
  test(`UI review releases its listener when crawl idle never arrives (baseline=${baseline})`, { timeout: 15_000 }, async () => {
    const server = await createUiReviewServer(0, { baseline });
    const directory = baseline ? dirname(server.config.cacheDir) : server.config.cacheDir;
    // An aborted fixture import can leave Vite's crawl-idle promise pending.
    // Its public close still has to cancel work and release actual resources.
    server.waitForRequestsIdle = () => new Promise(() => {});
    await server.close();
    assert.equal(server.httpServer.listening, false);
    assert.equal(existsSync(directory), false, "Snapshot and optimizer cache must be reclaimed");
    await server.close();
  });
}
