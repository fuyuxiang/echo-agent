import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const readCss = (name: string) =>
  readFileSync(resolve(process.cwd(), "src/styles", name), "utf8");

const tokensCss = readCss("tokens.css");
const appCss = readCss("app.css");
const toastCss = readCss("toast.css");

function layerValue(name: string): number {
  const match = tokensCss.match(new RegExp(`--echo-layer-${name}:\\s*(\\d+);`));
  if (!match) throw new Error(`missing layer token: ${name}`);
  return Number(match[1]);
}

describe("global overlay layering contract", () => {
  it("orders hover chrome, dialogs, critical prompts and feedback deliberately", () => {
    expect(layerValue("float")).toBeLessThan(layerValue("dialog"));
    expect(layerValue("popover")).toBeLessThan(layerValue("dialog"));
    expect(layerValue("dialog")).toBeLessThan(layerValue("dialog-popover"));
    expect(layerValue("dialog-popover")).toBeLessThan(layerValue("dialog-local"));
    expect(layerValue("dialog-local")).toBeLessThan(layerValue("confirm"));
    expect(layerValue("confirm")).toBeLessThan(layerValue("update"));
    expect(layerValue("update")).toBeLessThan(layerValue("critical"));
    expect(layerValue("critical")).toBeLessThan(layerValue("toast"));
    expect(layerValue("toast")).toBeLessThan(layerValue("tooltip"));
  });

  it("keeps every known full-screen surface on the shared contract", () => {
    expect(appCss).toMatch(/\.conversation-search-modal__overlay\s*\{[^}]*z-index:\s*var\(--echo-layer-dialog\)/s);
    expect(appCss).toMatch(/\.update-dialog__overlay\s*\{[^}]*z-index:\s*var\(--echo-layer-update\)/s);
    expect(appCss).toMatch(/\.trust-dialog__overlay\s*\{[^}]*z-index:\s*var\(--echo-layer-critical\)/s);
    expect(toastCss).toMatch(/\.toast\s*\{[^}]*z-index:\s*var\(--echo-layer-toast\)/s);
    expect(toastCss).toMatch(/\.toast-stack\s*\{[^}]*z-index:\s*var\(--echo-layer-toast\)/s);
    expect(appCss).toMatch(/\.app-dialog-overlay\s*\{[^}]*z-index:\s*var\(--echo-layer-confirm\)/s);
  });

  it("keeps the workspace picker menu on the shared popover layer", () => {
    expect(appCss).toMatch(/\.workspace-picker__menu\s*\{[^}]*z-index:\s*var\(--echo-layer-popover\)/s);
  });

  it("hides page hover chrome behind a modal while allowing hints inside it", () => {
    expect(appCss).toMatch(
      /body:has\(\[aria-modal="true"\]\) \.secondary-sidebar__trigger,[\s\S]*body:has\(\[aria-modal="true"\]\) \.secondary-sidebar__floating\s*\{\s*display:\s*none;/,
    );
    expect(appCss).toMatch(
      /body:has\(\[aria-modal="true"\]\) \.global-tooltip:not\(\.global-tooltip--dialog\)\s*\{\s*display:\s*none;/,
    );
  });

  it("keeps short tooltip copy intrinsically sized instead of forcing early CJK wraps", () => {
    expect(appCss).toMatch(
      /\.global-tooltip\s*\{[^}]*width:\s*max-content;[^}]*max-width:\s*min\(360px,\s*calc\(100vw - 16px\)\);/s,
    );
    expect(appCss).toMatch(/\.global-tooltip\s*\{[^}]*white-space:\s*pre-line;/s);
  });
});
