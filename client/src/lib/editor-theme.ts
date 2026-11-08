export function readEditorCommentColor(): string | undefined {
  const value = getComputedStyle(document.documentElement).getPropertyValue("--echo-code-comment").trim();
  return /^#[\da-f]{6}$/i.test(value) ? value : undefined;
}
export function readEditorSyntaxColors(): Record<string, string> {
  const style = getComputedStyle(document.documentElement);
  return Object.fromEntries(["type", "number", "constant"].flatMap(role => {
    const value = style.getPropertyValue(`--echo-code-${role}`).trim();
    return /^#[\da-f]{6}$/i.test(value) ? [[role, value]] : [];
  }));
}

/** Map the active ThemeProvider CSS roles to editor colour IDs. The host owns
 * theme state; Monaco and the embedded IDE receive only a resolved snapshot. */
export function readEditorThemeColors(): Record<string, string> {
  let context: CanvasRenderingContext2D | null | undefined;
  const style = getComputedStyle(document.documentElement);
  const roles: Record<string, string> = {
    "editor.background": "surface-content", "editor.foreground": "text-strong",
    "editorLineNumber.foreground": "text-weak", "editorLineNumber.activeForeground": "text-strong",
    "editorLineNumber.dimmedForeground": "text-weak",
    "editor.selectionBackground": "control-selected-bg-active", "editor.selectionForeground": "text-strong",
    "editor.inactiveSelectionBackground": "control-selected-bg", "editor.lineHighlightBackground": "surface-content",
    "editor.wordHighlightBackground": "code-match-bg", "editor.wordHighlightStrongBackground": "code-match-bg",
    "editorCursor.foreground": "control-selected-fg", "editorWidget.background": "surface-raised",
    "editorWidget.border": "border-default", "editorGroup.border": "border-default",
    "editorGutter.background": "surface-content", "editorOverviewRuler.border": "border-default",
    "editorError.foreground": "status-error", "editorWarning.foreground": "status-warning",
    "diffEditor.insertedLineBackground": "code-diff-added-bg", "diffEditor.removedLineBackground": "code-diff-removed-bg",
    "diffEditor.insertedTextBackground": "code-diff-added-text-bg", "diffEditor.removedTextBackground": "code-diff-removed-text-bg",
    "diffEditor.insertedTextBorder": "status-success", "diffEditor.removedTextBorder": "status-error",
    "diffEditorGutter.insertedLineBackground": "status-success-surface", "diffEditorGutter.removedLineBackground": "status-error-soft",
    "foreground": "text-strong", "descriptionForeground": "text-medium", "disabledForeground": "control-disabled-fg",
    "focusBorder": "focus-ring", "widget.border": "border-default",
    "sideBar.background": "surface-sidebar", "sideBar.foreground": "text-medium", "sideBar.border": "border-default",
    "activityBar.background": "surface-sidebar", "activityBar.foreground": "control-selected-fg",
    "activityBar.inactiveForeground": "text-medium",
    "panel.background": "surface-content", "panel.border": "border-default",
    "statusBar.background": "surface-sidebar", "statusBar.foreground": "text-medium", "statusBar.border": "border-default",
    "statusBar.noFolderBackground": "surface-sidebar", "statusBar.debuggingBackground": "control-selected-bg",
    "statusBar.debuggingForeground": "control-selected-fg",
    "tab.activeBackground": "surface-content", "tab.activeForeground": "text-strong",
    "tab.inactiveBackground": "surface-sidebar", "tab.inactiveForeground": "text-medium", "tab.border": "border-default",
    "editorGroupHeader.tabsBackground": "surface-sidebar",
    "list.activeSelectionBackground": "control-selected-bg", "list.activeSelectionForeground": "control-selected-fg",
    "list.inactiveSelectionBackground": "control-selected-bg", "list.inactiveSelectionForeground": "control-selected-fg",
    "list.hoverBackground": "surface-conversation", "list.hoverForeground": "text-strong",
    "input.background": "surface-content", "input.foreground": "text-strong", "input.border": "border-control",
    "input.placeholderForeground": "text-weak", "dropdown.background": "surface-raised", "dropdown.foreground": "text-strong",
    "dropdown.border": "border-control", "button.background": "button-primary-bg", "button.foreground": "button-primary-fg",
    "button.hoverBackground": "button-primary-bg-hover", "button.secondaryBackground": "surface-conversation",
    "button.secondaryForeground": "text-strong", "checkbox.background": "control-selected-bg",
    "checkbox.foreground": "control-selected-fg", "checkbox.border": "border-control",
    "terminal.background": "surface-content", "terminal.foreground": "text-strong",
    "terminalCursor.foreground": "control-selected-fg", "terminal.selectionBackground": "control-selected-bg-active",
    "terminal.ansiRed": "status-error", "terminal.ansiGreen": "status-success", "terminal.ansiYellow": "status-warning",
    "terminal.ansiBlue": "status-info", "terminal.ansiMagenta": "status-purple",
    "terminal.ansiCyan": "code-type", "terminal.ansiBrightBlack": "text-weak",
    "terminal.ansiBrightRed": "color-text-error-hover", "terminal.ansiBrightGreen": "color-text-success-hover",
    "terminal.ansiBrightYellow": "color-text-warning-hover", "terminal.ansiBrightBlue": "color-text-link-hover",
    "terminal.ansiBrightMagenta": "status-pink", "terminal.ansiBrightCyan": "status-info",
  };
  return Object.fromEntries(Object.entries(roles).flatMap(([id, role]) => {
    const value = style.getPropertyValue(`--echo-${role}`).trim();
    if (!value) return [];
    if (/^#[a-f\d]{6}(?:[a-f\d]{2})?$/i.test(value)) return [[id, value]];
    if (!globalThis.CSS?.supports?.("color", value)) return [];
    if (context === undefined) {
      const canvas = document.createElement("canvas");
      canvas.width = canvas.height = 1;
      context = canvas.getContext("2d", { willReadFrequently: true });
    }
    if (!context) return [];
    context.clearRect(0, 0, 1, 1);
    context.fillStyle = value;
    context.fillRect(0, 0, 1, 1);
    const rgba = [...context.getImageData(0, 0, 1, 1).data];
    return [[id, `#${rgba.slice(0, rgba[3] === 255 ? 3 : 4).map(channel => channel.toString(16).padStart(2, "0")).join("")}`]];
  }));
}
