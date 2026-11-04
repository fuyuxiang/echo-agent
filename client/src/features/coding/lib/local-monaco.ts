/// <reference types="vite/client" />
import { loader } from "@monaco-editor/react";
import { readEditorThemeColors, readEditorCommentColor, readEditorSyntaxColors } from "../../../lib/editor-theme";
import EditorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import CssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import HtmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import JsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import TypeScriptWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";

let preparing: Promise<void> | null = null;
let localMonaco: typeof import("monaco-editor") | null = null;

/** Refresh host surfaces and the syntax roles that need stronger contrast. */
export function syncLocalMonacoTheme(): void {
  if (!localMonaco) return;
  const dark = document.documentElement.dataset.theme === "dark";
  const comment = readEditorCommentColor();
  const syntax = readEditorSyntaxColors();
  localMonaco.editor.defineTheme(dark ? "echo-dark" : "echo-light", {
    base: dark ? "vs-dark" : "vs", inherit: true,
    rules: [
      ...(comment ? [{ token: "comment", foreground: comment.slice(1) }] : []),
      ...(syntax.type ? [{ token: "type", foreground: syntax.type.slice(1) }] : []),
      ...(syntax.constant ? [{ token: "constant", foreground: syntax.constant.slice(1) }] : []),
      ...(syntax.number ? ["number", "attribute.value.number", "attribute.value.unit"].map(token => ({ token, foreground: syntax.number.slice(1) })) : []),
    ], colors: readEditorThemeColors(),
  });
}

/** Use the locked, bundled editor and same-origin workers before mounting React. */
export function prepareLocalMonaco(): Promise<void> {
  // Reopening a loaded editor refreshes its colours without replacing the
  // shared initialization promise returned to concurrent callers.
  if (localMonaco) {
    try { syncLocalMonacoTheme(); }
    catch (error) { return Promise.reject(error); }
  }
  if (!preparing) {
    const scope = globalThis as typeof globalThis & {
      MonacoEnvironment?: import("monaco-editor").Environment;
    };
    scope.MonacoEnvironment = {
      ...scope.MonacoEnvironment,
      getWorker: (_moduleId, label) => {
        if (label === "typescript" || label === "javascript") return new TypeScriptWorker();
        if (label === "json") return new JsonWorker();
        if (label === "css" || label === "scss" || label === "less") return new CssWorker();
        if (label === "html" || label === "handlebars" || label === "razor") return new HtmlWorker();
        return new EditorWorker();
      },
    };
    preparing = import("monaco-editor").then(async (monaco) => {
      // Passing the actual ESM instance stops @monaco-editor/loader consulting
      // its remote AMD default, which the desktop's script-src 'self' rejects.
      loader.config({ monaco });
      await loader.init();
      localMonaco = monaco;
      syncLocalMonacoTheme();
    }).catch((error: unknown) => {
      preparing = null;
      localMonaco = null;
      throw error;
    });
  }
  return preparing;
}
