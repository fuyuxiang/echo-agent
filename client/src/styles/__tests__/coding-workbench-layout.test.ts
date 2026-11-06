import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const css = readFileSync(resolve(process.cwd(), "src/styles/coding-workbench.css"), "utf8");

describe("coding workbench grid contract", () => {
  it("keeps Theia and the Agent panel on separate grid tracks", () => {
    expect(css).toMatch(/\.coding-workbench\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\);[^}]*grid-template-rows:\s*max\(48px, 3rem\) minmax\(0, 1fr\);/s);
    expect(css).toMatch(/\.coding-workbench--theia\s*\{[^}]*grid-template-rows:\s*max\(48px, 3rem\) minmax\(0, 1fr\);/s);
    expect(css).toMatch(/\.echo-theia-workspace\s*\{[^}]*grid-column:\s*1;[^}]*grid-row:\s*2;/s);
    expect(css).toMatch(/\.coding-workbench--theia\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\).*var\(--echo-agent-width/s);
    expect(css).toMatch(/\.echo-theia-agent\s*\{[^}]*grid-column:\s*3;/s);
    expect(css).not.toContain(".coding-workbench__activity");
  });

  it("keeps execution cards at natural height and scrolls the transcript viewport", () => {
    expect(css).toMatch(
      /\.coding-agent__stream-shell\s*\{[^}]*flex:\s*1;[^}]*min-height:\s*0;/s,
    );
    expect(css).toMatch(
      /\.coding-agent__stream\s*\{[^}]*height:\s*100%;[^}]*overflow-y:\s*auto;[^}]*scrollbar-gutter:\s*stable;/s,
    );
    expect(css).toMatch(
      /\.coding-agent__stream-content\s*\{[^}]*display:\s*flex;[^}]*flex-direction:\s*column;[^}]*min-height:\s*100%;/s,
    );
    expect(css).toMatch(/\.coding-agent__stream-content\s*>\s*\*\s*\{[^}]*flex:\s*none;/s);
  });

  it("does not retain the removed self-hosted Monaco editor theme", () => {
    expect(css).not.toContain(".coding-monaco");
    expect(css).not.toMatch(/\.coding-monaco\s+\.monaco-editor\s+\.mtk\d+/);
    expect(css).not.toMatch(/\.monaco-editor\.echo-(?:light|dark|hc-light|hc-dark)/);
    expect(css).not.toContain("--echo-monaco-token-");
  });
});
