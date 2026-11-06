import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const stylesDir = resolve(process.cwd(), "src/styles");
const readCss = (fileName: string) =>
  readFileSync(resolve(stylesDir, fileName), "utf8").replace(/\r\n?/g, "\n");
const appCss = readCss("app.css");
const globalCss = readCss("global.css");
const tokensCss = readCss("tokens.css");
const visualPolishCss = readCss("settings-center.css");

function themeColors(theme: "light" | "dark") {
  const selector = theme === "dark" ? ':root[data-theme="dark"]' : ':root,';
  const start = tokensCss.indexOf(selector);
  const body = tokensCss.slice(tokensCss.indexOf("{", start) + 1, tokensCss.indexOf("}", start));
  const tokens = Object.fromEntries([...body.matchAll(/(--echo-[\w-]+):\s*([^;]+);/g)].map(match => [match[1], match[2].trim()]));
  const resolveToken = (name: string, seen = new Set<string>()): string => {
    if (seen.has(name)) throw new Error(`Circular theme token: ${name}`);
    seen.add(name);
    const value = tokens[`--echo-${name}`];
    if (!value) throw new Error(`Missing ${theme} role: ${name}`);
    const alias = value.match(/^var\(--echo-([\w-]+)\)$/);
    return alias ? resolveToken(alias[1], seen) : value;
  };
  return resolveToken;
}
function contrast(a: string, b: string) {
  const luminance = (hex: string) => {
    expect(hex).toMatch(/^#[\da-f]{6}$/i);
    return [1, 3, 5].map(index => Number.parseInt(hex.slice(index, index + 2), 16) / 255)
      .map(value => value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4)
      .reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0);
  };
  const [low, high] = [luminance(a), luminance(b)].sort((x, y) => x - y);
  return (high + .05) / (low + .05);
}

function composite(foreground: string, background: string) {
  expect(foreground).toMatch(/^#[\da-f]{8}$/i);
  const alpha = parseInt(foreground.slice(7, 9), 16) / 255;
  expect(alpha).toBeGreaterThan(0);
  expect(alpha).toBeLessThan(1);
  return "#" + [1, 3, 5].map(index => Math.round(
    parseInt(foreground.slice(index, index + 2), 16) * alpha
    + parseInt(background.slice(index, index + 2), 16) * (1 - alpha),
  ).toString(16).padStart(2, "0")).join("");
}

function sourceFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(dir, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return /\.(?:css|ts|tsx)$/.test(entry.name) ? [path] : [];
  });
}

describe("theme token contract", () => {
  for (const theme of ["light", "dark"] as const) {
    it(`${theme}: action, selection and status pairs remain readable on their own surfaces`, () => {
      const color = themeColors(theme);
      expect(color("button-primary-bg")).not.toBe(color("control-selected-bg"));
      // Line and character decorations overlap. Check syntax against both
      // actual composites, and the boundary that identifies the changed range.
      for (const [kind, status] of [["added", "success"], ["removed", "error"]]) {
        const line = composite(color(`code-diff-${kind}-bg`), color("surface-content"));
        const characters = composite(color(`code-diff-${kind}-text-bg`), line);
        const overlapping = composite(color(`code-diff-${kind}-text-bg`), characters);
        expect(characters).not.toBe(line);
        for (const background of [line, characters, overlapping]) {
          for (const foreground of [
            theme === "dark" ? "#569CD6" : "#0000FF",
            theme === "dark" ? "#CE9178" : "#A31515",
            theme === "dark" ? "#9CDCFE" : "#001080",
            color("code-comment"), color("code-type"), color("code-number"), color("code-constant"),
          ]) expect(contrast(foreground, background), `${theme}: ${kind} syntax on ${background}`).toBeGreaterThanOrEqual(4.5);
          expect(contrast(color(`status-${status}`), background)).toBeGreaterThanOrEqual(3);
        }
      }
      for (const [fg, bg] of [
        ["code-comment", "surface-content"], ["code-type", "code-match-bg"], ["code-number", "code-match-bg"], ["button-primary-fg", "button-primary-bg"], ["button-primary-fg", "button-primary-bg-hover"],
        ["button-primary-fg", "button-primary-bg-active"], ["control-selected-fg", "control-selected-bg"],
        ["control-selected-fg", "control-selected-bg-hover"], ["control-selected-fg", "control-selected-bg-active"],
        ["status-success", "status-success-surface"], ["status-warning", "status-warning-soft"],
        ["status-error", "status-error-soft"], ["status-info", "status-info-soft"],
        ["status-purple", "status-purple-soft"], ["status-pink", "status-pink-soft"],
      ]) expect(contrast(color(fg), color(bg)), `${theme}: ${fg} on ${bg}`).toBeGreaterThanOrEqual(4.5);
      for (const surface of ["surface-canvas", "surface-sidebar", "surface-content", "surface-raised", "surface-conversation"]) {
        for (const text of ["text-strong", "text-medium", "text-weak"]) {
          expect(contrast(color(text), color(surface)), `${theme}: ${text} on ${surface}`).toBeGreaterThanOrEqual(4.5);
        }
        expect(contrast(color("border-control"), color(surface)), `${theme}: functional control boundary`).toBeGreaterThanOrEqual(3);
        expect(contrast(color("focus-ring"), color(surface)), `${theme}: keyboard focus`).toBeGreaterThanOrEqual(3);
      }
      expect(color("control-disabled-bg")).not.toBe(color("button-primary-bg"));
    });
  }

  it("全局兼容层不再覆盖核心主题令牌", () => {
    for (const token of [
      "--echo-bg-primary",
      "--echo-bg-secondary",
      "--echo-bg-tertiary",
      "--echo-text-strong",
      "--echo-text-medium",
      "--echo-text-weak",
      "--echo-border-default",
    ]) {
      expect(globalCss).not.toMatch(new RegExp(`${token}\\s*:`));
    }
  });

  it("迁移中的共享页面使用的历史语义名都有统一映射", () => {
    for (const token of [
      "--echo-text-primary",
      "--echo-text-secondary",
      "--echo-text-light",
      "--echo-bg-elevated",
      "--echo-bg-input",
      "--echo-border",
      "--echo-border-soft",
      "--echo-danger",
      "--echo-link",
    ]) {
      expect(globalCss).toMatch(new RegExp(`${token}\\s*:`));
    }

    expect(appCss).not.toContain("--echo-brand-rgb");
    expect(appCss).not.toContain("--echo-palette-green-500");
  });

  it("源码引用的主题变量都存在定义", () => {
    const files = sourceFiles(resolve(process.cwd(), "src"));
    const sources = files.map((file) => readFileSync(file, "utf8"));
    const definitions = new Set(
      sources
        .filter((_, index) => files[index].endsWith(".css"))
        .flatMap((source) => [...source.matchAll(/(--(?:echo|ui|atm)-[a-z0-9-]+)\s*:/g)])
        .map((match) => match[1]),
    );
    const references = new Set(
      sources
        .flatMap((source) => [...source.matchAll(/var\((--(?:echo|ui|atm)-[a-z0-9-]+)/g)])
        .map((match) => match[1]),
    );

    expect([...references].filter((token) => !definitions.has(token)).sort()).toEqual([]);
  });

  it("通知筛选的激活态在浅色背景上保持可见", () => {
    const baseRule = visualPolishCss.indexOf(".notification-filter {");
    const activeRule = visualPolishCss.indexOf(
      ".notification-filter.notification-filter--active {",
    );
    const activeBlock = visualPolishCss.slice(
      activeRule,
      visualPolishCss.indexOf("}", activeRule) + 1,
    );

    expect(baseRule).toBeGreaterThanOrEqual(0);
    expect(activeRule).toBeGreaterThan(baseRule);
    expect(activeBlock).toContain("background: var(--echo-brand-soft);");
    expect(activeBlock).toContain("color: var(--echo-color-text-brand-default);");
  });

});
