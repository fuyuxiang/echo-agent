import ReactDOM from "react-dom/client";
import { restoreUiBeforeBootstrap } from "./lib/backup-client";
import { AppErrorBoundary } from "./components/AppErrorBoundary";
import { GlobalTooltip } from "./components/GlobalTooltip";
import { initializeTheme } from "./components/ThemeProvider";
import { applyFontSize, readFontSize } from "./lib/font-size";
import { startRendererHealth } from "./lib/renderer-recovery";

import "./styles/index.css";

async function bootstrap() {
  const restored = await restoreUiBeforeBootstrap();
  initializeTheme();
  applyFontSize(readFontSize());
  const { default: App } = await import("./App");
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <AppErrorBoundary>
      <App uiRestoreDeferred={restored === "deferred"} />
      <GlobalTooltip />
    </AppErrorBoundary>
  );
  const stopHealth = startRendererHealth();
  if (import.meta.hot) import.meta.hot.dispose(stopHealth);
}
void bootstrap().catch((error) => {
  const root = document.getElementById("root")!;
  const panel = document.createElement("main");
  panel.style.cssText = "padding:48px;max-width:720px;margin:auto;font:16px system-ui";
  const heading = document.createElement("h1"); heading.textContent = "启动未完成";
  const detail = document.createElement("p"); detail.textContent = `${String(error)}。请检查本机存储空间后重试；未完成的数据恢复会在下次启动时继续。`;
  const retry = document.createElement("button"); retry.textContent = "重试"; retry.className = "echo-button echo-button--primary"; retry.onclick = () => location.reload();
  panel.append(heading, detail, retry); root.replaceChildren(panel);
});
