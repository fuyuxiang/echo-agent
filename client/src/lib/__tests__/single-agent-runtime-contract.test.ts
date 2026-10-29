import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const projectRoot = resolve(__dirname, "../../..");
const readProjectFile = (path: string) => readFileSync(resolve(projectRoot, path), "utf8");

describe("single agent runtime architecture", () => {
  it("keeps model selection inside the EchoAgent Runtime lifecycle", () => {
    const commands = readProjectFile("src-tauri/src/commands.rs");

    expect(commands).toContain("agent_runtime::new_session");
    expect(commands).toContain("agent_runtime::prepare_mode_prompt");
    expect(commands).toContain("agent_runtime::send_prepared_prompt");
    expect(commands).not.toContain("is_codex_session");
    expect(commands).not.toContain("codex.new_thread");
    expect(commands).not.toContain("codex.send");
  });

  it("does not bundle a second agent runtime for a model provider", () => {
    const manifest = readProjectFile("package.json");
    const tauriConfig = readProjectFile("src-tauri/tauri.conf.json");
    const rustModules = readProjectFile("src-tauri/src/lib.rs");

    expect(manifest).not.toContain("@openai/codex");
    expect(tauriConfig).not.toContain("externalBin");
    expect(rustModules).not.toContain("mod codex_app_server");
  });
});
