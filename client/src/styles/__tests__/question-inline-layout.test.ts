import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const appCss = readFileSync(resolve(process.cwd(), "src/styles/app.css"), "utf8");

describe("inline question scrolling contract", () => {
  it("lays out the card as a bounded column so its body can shrink", () => {
    expect(appCss).toMatch(
      /\.question-inline\s*\{[^}]*display:\s*flex;[^}]*flex-direction:\s*column;[^}]*max-height:[^;]+;[^}]*overflow:\s*hidden;/s,
    );
    expect(appCss).toMatch(/\.question-inline__head\s*\{[^}]*flex:\s*none;/s);
    expect(appCss).toMatch(/\.question-inline__footer\s*\{[^}]*flex:\s*none;/s);
  });

  it("owns vertical scrolling in the question body instead of clipping it", () => {
    expect(appCss).toMatch(
      /\.question-inline__body\s*\{[^}]*flex:\s*1 1 auto;[^}]*min-height:\s*0;[^}]*overflow-y:\s*auto;[^}]*overscroll-behavior:\s*contain;/s,
    );
  });
});
