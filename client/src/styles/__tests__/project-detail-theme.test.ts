import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const css = readFileSync(resolve(process.cwd(), "src/styles/app.css"), "utf8");
const projectTabs = readFileSync(
  resolve(process.cwd(), "src/components/project-tabs.tsx"),
  "utf8",
);

function rule(selector: string): string {
  const start = css.indexOf(`${selector} {`);
  expect(start, `missing CSS rule: ${selector}`).toBeGreaterThanOrEqual(0);
  return css.slice(start, css.indexOf("}", start) + 1);
}

describe("project detail theme contract", () => {
  it("uses semantic surfaces for plan columns and cards", () => {
    const column = rule(".pd-board-col");
    const card = rule(".pd-board-card");

    expect(column).toContain("background: var(--echo-bg-secondary);");
    expect(column).toContain("border: 1px solid var(--echo-border-default);");
    expect(column).not.toContain("#f0f0f0");
    expect(card).toContain("background: var(--echo-bg-primary);");
    expect(card).toContain("border: 1px solid var(--echo-border-weak);");
  });

  it("keeps project primary controls legible in both themes", () => {
    const primaryButton = rule(".pd-btn--primary");
    expect(primaryButton).toContain("background: var(--echo-button-primary-bg);");
    expect(primaryButton).toContain("color: var(--echo-button-primary-fg);");

    expect(rule(".pd-pill--on")).toContain("background: var(--echo-control-selected-bg);");
    expect(rule(".pd-pill--on")).toContain("color: var(--echo-control-selected-fg);");

    // 项目 composer 改用首页 `<Composer>` 后,发送按钮由 Composer 提供(.echo-composer__send);
    // 发送操作和普通主操作使用同一组品牌前景、背景；空输入仍保持中性禁用态。
    const sendBlock = rule(".echo-composer__send");
    expect(sendBlock).toContain("background: var(--echo-button-primary-bg);");
    expect(sendBlock).toContain("color: var(--echo-button-primary-fg);");
    // disabled 状态与 empty 共享规则体,索引查找直接定位规则块。
    const disabledStart = css.indexOf(".echo-composer__send:disabled,");
    expect(disabledStart, "missing disabled rule for .echo-composer__send").toBeGreaterThanOrEqual(0);
    const disabledBlockEnd = css.indexOf("}", disabledStart);
    const disabledBlock = css.slice(disabledStart, disabledBlockEnd + 1);
    expect(disabledBlock).toContain("background: var(--echo-bg-tertiary);");
  });

  it("uses theme-aware emphasis and status colours", () => {
    expect(rule(".pd-tab-btn--on")).toContain("border-bottom-color: var(--echo-brand);");
    // Unified work items reuse the shared task-status badge rather than a second palette.
    expect(projectTabs).toContain('className={`pd-session-status pd-session-status--${executionStatus.tone}`}');
    expect(css).toContain('.pd-session-status--finished');
  });

  it("preserves a visible interaction target for each column add action", () => {
    const add = rule(".pd-board-col__add");
    expect(add).toContain("width: 28px;");
    expect(add).toContain("height: 28px;");
    expect(rule(".pd-board-col__add:hover")).toContain("color: var(--echo-text-strong);");
  });
});
