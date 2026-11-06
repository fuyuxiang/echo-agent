import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const appCss = readFileSync(resolve(process.cwd(), "src/styles/app.css"), "utf8");

describe("InputAddMenu layout contract", () => {
  it("keeps actions compact and gives catalog descriptions more reading space", () => {
    expect(appCss).toMatch(
      /\.iam-popover\s*\{[^}]*width:\s*14rem;[^}]*max-width:\s*calc\(100vw - 16px\);/s,
    );
    expect(appCss).toMatch(
      /\.iam-submenu\s*\{[^}]*width:\s*var\(--iam-catalog-width\);[^}]*max-width:\s*calc\(100vw - 16px\);/s,
    );
  });

  it("uses compact rows that grow with text and viewport-bounded submenu scrolling", () => {
    expect(appCss).toMatch(/\.iam-item\s*\{[^}]*min-height:\s*34px;/s);
    expect(appCss).toMatch(/\.iam-sub-item\s*\{[^}]*min-height:\s*40px;/s);
    expect(appCss).toMatch(
      /\.iam-submenu__scroll\s*\{[^}]*max-height:\s*min\(300px,\s*calc\(100vh - 28px\)\);/s,
    );
  });

  it("overrides the global focus ring with an inset menu focus treatment", () => {
    for (const selector of ["iam-item", "iam-sub-item", "iam-sub-back"]) {
      expect(appCss).toMatch(
        new RegExp(`\\.${selector}:focus-visible\\s*\\{[^}]*outline:\\s*none;[^}]*box-shadow:\\s*inset`, "s"),
      );
    }
  });
});
