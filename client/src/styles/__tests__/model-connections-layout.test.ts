import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const css = readFileSync(resolve(__dirname, "../model-connections.css"), "utf8");

function declarationBlock(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = css.match(new RegExp(`^${escaped}\\s*\\{([^}]*)\\}`, "m"));
  if (!match) throw new Error(`Missing CSS rule: ${selector}`);
  return match[1];
}

describe("model settings layout", () => {
  it("lets the outer settings pane scroll the list of models", () => {
    for (const selector of [".model-connections", ".provider-card", ".provider-card__models"]) {
      expect(declarationBlock(selector)).not.toMatch(/overflow|height:\s*100%/);
    }
  });

  it("uses design tokens instead of hard-coded colors", () => {
    expect(css).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    expect(css).not.toMatch(/rgba?\(/);
  });

  it("keeps long names from pushing row actions out of the card", () => {
    expect(declarationBlock(".provider-card__model-copy")).toMatch(/min-width:\s*0/);
    expect(declarationBlock(".provider-card__model-title strong")).toMatch(/overflow-wrap:\s*anywhere/);
    expect(declarationBlock(".provider-card__model-actions")).toMatch(/grid-column:\s*3/);
  });

  it("keeps model actions visible for every input method", () => {
    expect(declarationBlock(".provider-card__model-actions")).not.toMatch(/(?:opacity|visibility|display:\s*none)/);
  });

  it("keeps row actions on the first line when the failure detail expands", () => {
    expect(declarationBlock(".provider-card__test-detail")).toMatch(/order:\s*1/);
  });

  it("lets only the body of the editor scroll", () => {
    expect(declarationBlock(".model-connection-editor__body")).toMatch(/overflow-y:\s*auto/);
  });
});
