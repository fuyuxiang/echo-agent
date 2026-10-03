import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const css = readFileSync(resolve(__dirname, "../visual-polish.css"), "utf8");

function declarationBlock(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = css.match(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`));
  if (!match) throw new Error(`Missing CSS rule: ${selector}`);
  return match[1];
}

describe("model connections layout contract", () => {
  it("uses the workspace as the bounded desktop viewport", () => {
    const panel = declarationBlock(".model-connections");
    const workspace = declarationBlock(".model-connections__workspace");

    expect(panel).toMatch(/height:\s*100%/);
    expect(panel).toMatch(/overflow:\s*hidden/);
    expect(workspace).toMatch(/flex:\s*1 1 0/);
    expect(workspace).toMatch(/min-height:\s*0/);
  });

  it("makes both master-detail columns independently scrollable", () => {
    for (const selector of [".model-connections__source-list", ".model-connections__detail"]) {
      const block = declarationBlock(selector);
      expect(block).toMatch(/min-height:\s*0/);
      expect(block).toMatch(/overflow-y:\s*auto/);
      expect(block).toMatch(/overscroll-behavior:\s*contain/);
    }
  });

  it("allows vertical wheel chaining through the mobile connection strip", () => {
    expect(css).toMatch(
      /@media \(max-width: 760px\)[\s\S]*?\.model-connections__source-list\s*\{[^}]*overscroll-behavior-x:\s*contain;[^}]*overscroll-behavior-y:\s*auto;/,
    );
  });
});
