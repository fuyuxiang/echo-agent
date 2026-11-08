import { spawnSync } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, it } from "vitest";
import { runNativeTestProcess } from "./lib/native-test-process.mjs";

const helper = new URL("./lib/native-test-process.mjs", import.meta.url).href;
function run(source, github = "true") {
  return spawnSync(process.execPath, ["--input-type=module", "-e", `
    import { runNativeTestProcess } from ${JSON.stringify(helper)};
    process.exitCode = await runNativeTestProcess(process.execPath, ['-e', ${JSON.stringify(source)}]);
  `], { encoding: "utf8", env: { ...process.env, GITHUB_ACTIONS: github }, timeout: 10_000 });
}

it("streams both channels and preserves a successful exit without an annotation", () => {
  const result = run("console.log('stdout 中文'); console.error('stderr warning');");
  expect(result.status).toBe(0);
  expect(result.stdout).toContain("stdout 中文");
  expect(result.stderr).toContain("stderr warning");
  expect(result.stderr).not.toContain("::error");
});

it("preserves the failure code and puts escaped diagnostics in the CI annotation", () => {
  const result = run("console.log('progress'); console.error('panic 100%\\r\\n中文'); process.exitCode = 7;");
  expect(result.status).toBe(7);
  expect(result.stdout).toContain("progress");
  expect(result.stderr).toContain("panic 100%\r\n中文");
  expect(result.stderr).toContain("::error title=Native host tests failed::Exit code 7%0A");
  expect(result.stderr).toContain("panic 100%25%0D%0A中文");
});

it("bounds the annotation tail while retaining the full ordinary output", () => {
  const result = run("console.error('final failure'); setTimeout(() => console.log('x'.repeat(20_000)), 20); process.exitCode = 101;");
  expect(result.status).toBe(101);
  expect(result.stdout).toContain("x".repeat(20_000));
  const annotations = result.stderr.split("::error title=").slice(1);
  expect(annotations.some(annotation => annotation.includes("final failure"))).toBe(true);
  expect(annotations.every(annotation => annotation.length < 3000)).toBe(true);
});

it("keeps failed test stdout visible when Cargo fills stderr with compiler warnings", () => {
  const result = run("console.log('panicked at failed cross-platform assertion'); console.error('compiler warning\\n'.repeat(2000)); process.exitCode = 101;");
  expect(result.status).toBe(101);
  const annotations = result.stderr.split("::error title=").slice(1);
  expect(annotations).toHaveLength(2);
  expect(annotations[0]).toContain("stdout%0Apanicked at failed cross-platform assertion");
  expect(annotations[1]).toContain("stderr%0A");
  expect(annotations.every(annotation => annotation.length < 4096)).toBe(true);
});

it("preserves complete stream artifacts even when annotations must be shortened", async () => {
  const output = mkdtempSync(join(tmpdir(), "echo-native-log-test-"));
  try {
    const status = await runNativeTestProcess(process.execPath, ["-e", "console.log('artifact stdout'); console.error('artifact stderr'); process.exitCode = 9;"],
      { env: { ...process.env, GITHUB_ACTIONS: "false", NATIVE_TEST_OUTPUT: output } });
    expect(status).toBe(9);
    const directory = join(output, readdirSync(output)[0]);
    expect(readFileSync(join(directory, "stdout.log"), "utf8")).toBe("artifact stdout\n");
    expect(readFileSync(join(directory, "stderr.log"), "utf8")).toBe("artifact stderr\n");
  } finally { rmSync(output, { recursive: true, force: true }); }
});

it("keeps local failures free of GitHub annotation commands", () => {
  const result = run("console.error('local failure'); process.exitCode = 3;", "false");
  expect(result.status).toBe(3);
  expect(result.stderr).toContain("local failure");
  expect(result.stderr).not.toContain("::error");
});

it("reports a signal-terminated test process as a failure", () => {
  const result = run("process.kill(process.pid, 'SIGTERM');");
  expect(result.status).toBe(1);
  expect(result.stderr).toContain("::error title=Native host tests failed::Exit code 1%0A");
});

it("propagates process startup errors", async () => {
  await expect(runNativeTestProcess("__echo_missing_native_test_command__", []))
    .rejects.toHaveProperty("code", "ENOENT");
});
