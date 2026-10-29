import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const projectRoot = resolve(__dirname, "../../../..");

describe("coding Tauri API contract", () => {
  it("registers every native command exposed by the typed coding client", () => {
    const client = readFileSync(
      resolve(projectRoot, "src/features/coding/lib/tauri-api.ts"),
      "utf8",
    );
    const rust = readFileSync(resolve(projectRoot, "src-tauri/src/lib.rs"), "utf8");
    const invoked = new Set(
      [...client.matchAll(/["'](coding_[a-z0-9_]+)["']/g)].map((match) => match[1]),
    );
    const registered = new Set(
      [...rust.matchAll(/(?:coding::[a-z0-9_]+|coding_[a-z0-9_]+)::(coding_[a-z0-9_]+)/g)]
        .map((match) => match[1]),
    );

    expect([...invoked].filter((command) => !registered.has(command))).toEqual([]);
    expect(invoked.size).toBeGreaterThan(40);
  });
});
