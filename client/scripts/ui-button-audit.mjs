import assert from "node:assert/strict";

// Compare real computed styles, including feature-specific actions that do not
// use Button. Avatar colours and rendered user/editor content have other roles.
const primarySelector = [
  ".echo-button--primary", ".echo-button--brand", ".btn--primary", ".form-button--primary",
  ".capability-button--primary", ".settings-btn--primary", ".atm-btn--primary",
  ".app-dialog__button--primary", ".atm-create-btn", ".atm-detail-btn--primary",
  ".meeting-primary", ".kb-panel__add-btn", ".org-memory__primary",
  ".feedback-dialog__btn--primary", ".browser-preview__go", ".um-btn--primary",
  ".plan-panel__approve-btn", ".plan-panel__add-btn", ".plan-panel__row-edit-save",
  ".coding-primary-btn", ".pd-invite", ".pd-btn--primary", ".pd-pill--on",
  ".echo-composer__send", ".context-menu__rename-save", ".coding-agent__send", ".echo-theia-agent__send",
  ".coding-agent__decision button.is-primary", ".perm-inline__action--allow",
].join(",");

export async function inspectButtonBackgrounds(page, theme) {
  await page.mouse.move(0, 0);
  return page.evaluate(({primarySelector, theme}) => {
    const probe = document.createElement("span");
    probe.style.backgroundColor = "var(--echo-button-primary-bg)";
    probe.style.color = "var(--echo-button-primary-fg)";
    document.body.append(probe);
    const expected = { background:getComputedStyle(probe).backgroundColor, color:getComputedStyle(probe).color };
    probe.remove();
    const buttons = [...document.querySelectorAll('button,[role="button"]')].filter(e => {
      const rect = e.getBoundingClientRect(), style = getComputedStyle(e);
      return rect.width > 0 && rect.height > 0 && style.visibility !== "hidden"
        && !e.closest(".monaco-editor,.xterm,.markdown-body,.md-content,.create-colleague-avatar-item");
    }).map(e => {
      const style = getComputedStyle(e);
      return { name:(e.getAttribute("aria-label") || e.textContent || e.getAttribute("title") || "").trim().slice(0, 100),
        classes:e.className, background:style.backgroundColor, color:style.color, disabled:e.matches(":disabled,[aria-disabled=true]"),
        primary:e.matches(primarySelector) && !e.matches(".echo-composer__send--empty,.pd-pill--stat") };
    });
    // color-mix() may serialize as color(srgb ...) with 0..1 channels.
    // Let the browser convert CSS Color 4 values instead of parsing them as RGB.
    const canvas = document.createElement("canvas");
    canvas.width = canvas.height = 1;
    const context = canvas.getContext("2d", {willReadFrequently:true});
    const black = theme === "light" ? buttons.filter(button => {
      context.clearRect(0, 0, 1, 1);
      context.fillStyle = button.background;
      context.fillRect(0, 0, 1, 1);
      const [r,g,b,a] = context.getImageData(0, 0, 1, 1).data;
      return a >= 128 && Math.max(r,g,b) < 90 && Math.max(r,g,b) - Math.min(r,g,b) < 15;
    }) : [];
    const drift = buttons.filter(button => button.primary && !button.disabled
      && (button.background !== expected.background || button.color !== expected.color));
    return { expected, buttons, black, drift };
  }, {primarySelector, theme});
}

export function assertButtonBackgrounds(audit, label) {
  assert.deepEqual(audit.black, [], `${label}: unexpected black button backgrounds`);
  assert.deepEqual(audit.drift, [], `${label}: primary action palette differs from shared tokens`);
}
