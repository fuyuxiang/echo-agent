import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const appCss = readFileSync(resolve(process.cwd(), "src/styles/app.css"), "utf8");
const rules = [...appCss.replace(/\/\*[\s\S]*?\*\//g, "").matchAll(/([^{}]+)\{([^{}]*)\}/g)];

function hasDeclarations(selector: string, declarations: RegExp): boolean {
  return rules.some(([, selectors, body]) =>
    selectors.split(",").some((entry) => entry.trim() === selector) && declarations.test(body),
  );
}

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
    expect(hasDeclarations(".iam-item", /min-height:\s*34px;/)).toBe(true);
    expect(hasDeclarations(".iam-sub-item", /min-height:\s*40px;/)).toBe(true);
    expect(hasDeclarations(".iam-submenu__scroll",
      /max-height:\s*min\(300px,\s*calc\(100vh - 28px\)\);/,
    )).toBe(true);
  });

  it("overrides the global focus ring with an inset menu focus treatment", () => {
    for (const selector of ["iam-item", "iam-sub-item", "iam-sub-back"]) {
      expect(hasDeclarations(`.${selector}:focus-visible`, /outline:\s*none;[\s\S]*box-shadow:\s*inset/))
        .toBe(true);
    }
  });
});
