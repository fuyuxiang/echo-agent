import { spawnSync } from "node:child_process";
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
  const annotation = result.stderr.split("::error title=")[1];
  expect(annotation).toContain("final failure");
  expect(annotation.length).toBeLessThan(12_100);
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
