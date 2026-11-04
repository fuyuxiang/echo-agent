// Each route mounts the real scenario fixture in a fresh document. A single
// Vite optimizer avoids six concurrent copies of the same dependency graph.
const route = location.pathname.split("/").at(-1);
switch (route) {
  case "ui-surfaces": await import("./ui-surfaces"); break;
  case "meeting-workbench-review": await import("./meeting-workbench-review"); break;
  case "group-review": await import("./group-review"); break;
  case "scheduled-tasks-review": await import("./scheduled-tasks-review"); break;
  case "document-workspace-review": await import("./document-workspace-review"); break;
  default: await import("./enterprise-theme-review");
}
export {};
