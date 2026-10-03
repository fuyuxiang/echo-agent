import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const appCss = readFileSync(resolve(process.cwd(), "src/styles/app.css"), "utf8");
const visualPolishCss = readFileSync(resolve(process.cwd(), "src/styles/visual-polish.css"), "utf8");

describe("settings content scrolling contract", () => {
  it("keeps the standalone token usage view vertically scrollable", () => {
    expect(appCss).toMatch(
      /\.quota-panel\s*\{[^}]*min-height:\s*0;[^}]*overflow-x:\s*hidden;[^}]*overflow-y:\s*auto;[^}]*overscroll-behavior:\s*contain;[^}]*scrollbar-gutter:\s*stable;/s,
    );
  });

  it("keeps token usage actions available while scrolling long content", () => {
    expect(appCss).toMatch(
      /\.quota-panel__head\s*\{[^}]*position:\s*sticky;[^}]*top:\s*0;[^}]*z-index:\s*1;/s,
    );
  });

  it("uses the subview scrollport for tabs and long settings sections", () => {
    expect(visualPolishCss).toMatch(
      /\.settings-subview\s*>\s*\.settings-section\s*\{[^}]*flex:\s*0 0 auto;[^}]*overflow-x:\s*clip;[^}]*overflow-y:\s*visible;/s,
    );
    expect(visualPolishCss).toMatch(
      /\.settings-view-tabs__sticky\s*\{[^}]*position:\s*sticky;[^}]*top:\s*0;/s,
    );
  });

  it("lets the settings page own vertical usage scrolling", () => {
    expect(visualPolishCss).toMatch(
      /\.settings-modal__panel \.quota-panel\s*\{[^}]*overflow-x:\s*clip;[^}]*overflow-y:\s*visible;[^}]*overscroll-behavior:\s*auto;/s,
    );
  });
});
