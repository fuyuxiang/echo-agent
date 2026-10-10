import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn(async (command: string, _args?: unknown): Promise<unknown> => {
  if (command === "coding_task_list") return [];
  if (command === "coding_verification_detect") return [];
  return null;
});
const eventListeners = new Map<string, (event: { payload: unknown }) => void>();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args: unknown) => invoke(command, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, callback: (event: { payload: unknown }) => void) => {
    eventListeners.set(name, callback);
    return () => {
      if (eventListeners.get(name) === callback) eventListeners.delete(name);
    };
  }),
}));
vi.mock("@/components/workspace-panel/FileTreeView", () => ({
  FileTreeView: ({
    rootPath,
    onFileSelect,
  }: {
    rootPath: string;
    onFileSelect: (path: string) => void;
  }) => (
    <div data-testid="file-tree">
      <button type="button" onClick={() => onFileSelect(`${rootPath}/src/a.ts`)}>
        打开源文件
      </button>
    </div>
  ),
}));
const readDocument = vi.fn(async (_root: string, path: string) => ({
  path,
  relativePath: path.replace("/repo/", ""),
  content: "disk content",
  hash: "h1",
  size: 12,
  modifiedAt: 0,
  language: "typescript",
  lineEnding: "LF" as const,
}));
const writeDocument = vi.fn(async (_root: string, path: string, content: string, _hash?: string) => ({
  path,
  relativePath: path.replace("/repo/", ""),
  content,
  hash: "h2",
  size: content.length,
  modifiedAt: 0,
  language: "typescript",
  lineEnding: "LF" as const,
}));

vi.mock("@/lib/agent-client", () => ({
  filesystemPickDirectory: vi.fn(async () => "/picked"),
  listDir: vi.fn(async () => []),
  codingReadDocument: (root: string, path: string) => readDocument(root, path),
  codingWriteDocument: (root: string, path: string, content: string, hash: string) =>
    writeDocument(root, path, content, hash),
}));

// These shared components reach for session/permission plumbing that is out of
// scope here; the workbench only needs to render them.
vi.mock("@/components/PermissionPicker", () => ({
  PermissionPicker: () => <div data-testid="permission-picker" />,
}));
vi.mock("@/components/ModelSelector", () => ({
  ModelSelector: () => <div data-testid="model-selector" />,
}));
vi.mock("@/components/PermissionDialog", () => ({
  PermissionInlineCard: () => <div data-testid="permission-card" />,
}));
vi.mock("@/components/QuestionInlineCard", () => ({
  QuestionInlineCard: () => <div data-testid="question-card" />,
}));
vi.mock("@/components/ExecutionProcess", () => ({
  ExecutionProcess: () => <div data-testid="execution" />,
}));
vi.mock("@/components/Markdown", () => ({
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));

import { CodingWorkbench } from "../CodingWorkbench";
import { codingTaskContextDraftKey, codingTaskDraftKey } from "../lib/task-draft-key";
import type { CodingTask } from "../lib/types";
import { useAiDraftStore } from "../store/ai-draft-store";
import { useTaskStore } from "../store/task-store";
import { SidebarLayoutContext, useAppSidebarLayout } from "@/lib/sidebar-layout";

function verificationTask(overrides: Partial<CodingTask> = {}): CodingTask {
  return {
    schemaVersion: 2,
    id: "verification-task",
    name: "验证布局",
    requirement: "验证工作台布局",
    phase: "verifying",
    acceptanceCriteria: [],
    taskNodes: [],
    planIssues: [],
    globalConstraints: [],
    createdAt: "2026-09-13T00:00:00Z",
    updatedAt: "2026-09-13T00:00:01Z",
    ...overrides,
  };
}

describe("Theia workbench", () => {
  const bridgeToken = (frame: HTMLIFrameElement): string =>
    (JSON.parse(frame.name.slice("echo-embed:".length)) as { bridgeToken: string }).bridgeToken;
  beforeEach(() => {
    window.history.replaceState({}, "", "/");
    window.localStorage.removeItem("echo-agent-panel-width");
    invoke.mockReset();
    invoke.mockImplementation(async (command: string): Promise<unknown> => {
      if (command === "coding_task_list" || command === "coding_verification_detect") return [];
      if (command === "coding_theia_start") {
        return { url: "http://127.0.0.1:41773/", embedToken: "test-embed-token" };
      }
      return null;
    });
    useTaskStore.setState({
      root: "",
      summaries: [],
      task: null,
      changeSet: null,
      verifications: [],
      problems: [],
      orchestrator: null,
      loading: false,
      error: null,
    });
    useAiDraftStore.getState().clear();
  });

  it("restores the global sidebar from the coding header without leaving the workbench", () => {
    function Shell() {
      const layout = useAppSidebarLayout();
      return <SidebarLayoutContext.Provider value={layout.context}>
        <button onClick={() => layout.setCollapsed(true)}>收起全局导航</button>
        <CodingWorkbench cwd="" models={[]} />
      </SidebarLayoutContext.Provider>;
    }
    render(<Shell />);
    fireEvent.click(screen.getByText("收起全局导航"));
    fireEvent.click(screen.getByRole("button", { name: "展开侧边栏" }));
    expect(screen.queryByRole("button", { name: "展开侧边栏" })).not.toBeInTheDocument();
    expect(screen.getByText("打开代码库开始开发")).toBeInTheDocument();
  });

  it("keeps the Agent pane beside Theia and allows resizing independently", async () => {
    const { container } = render(<CodingWorkbench cwd="/repo" models={[]} />);

    expect(await screen.findByTitle("Echo Code IDE")).toHaveAttribute(
      "name",
      expect.stringContaining("test-embed-token"),
    );
    expect((screen.getByTitle("Echo Code IDE") as HTMLIFrameElement).src).not.toContain("echoEmbedToken");
    expect(invoke).toHaveBeenCalledWith("coding_theia_start", { root: "/repo" });
    const agent = container.querySelector(".coding-workbench > .echo-theia-agent") as HTMLElement;
    expect(agent).toBeInTheDocument();
    expect(container.querySelector(".echo-theia-workspace > .echo-theia-agent")).not.toBeInTheDocument();
    const splitter = screen.getByRole("separator", { name: "调整 Agent 面板宽度" });
    expect(splitter).toHaveAttribute("aria-valuenow", "410");
    expect(container.querySelector(".coding-workbench--agent-closed")).not.toBeInTheDocument();
    fireEvent.keyDown(splitter, { key: "ArrowLeft" });
    expect(splitter).toHaveAttribute("aria-valuenow", "430");
    const workbench = container.querySelector(".coding-workbench") as HTMLElement;
    vi.spyOn(workbench, "getBoundingClientRect").mockReturnValue({
      width: 1200, right: 1200, left: 0, top: 0, bottom: 800, height: 800, x: 0, y: 0,
      toJSON: () => ({}),
    });
    fireEvent.pointerDown(splitter);
    fireEvent.pointerMove(window, { clientX: 700 });
    fireEvent.pointerUp(window);
    expect(splitter).toHaveAttribute("aria-valuenow", "500");
    expect(window.localStorage.getItem("echo-agent-panel-width")).toBe("500");
    vi.spyOn(workbench, "getBoundingClientRect").mockReturnValue({
      width: 700, right: 700, left: 0, top: 0, bottom: 800, height: 800, x: 0, y: 0,
      toJSON: () => ({}),
    });
    fireEvent.doubleClick(splitter);
    expect(splitter).toHaveAttribute("aria-valuemax", "380");
    expect(splitter).toHaveAttribute("aria-valuenow", "380");
    expect(screen.queryByRole("tablist", { name: "开发任务面板" })).not.toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "任务描述" })).toBeInTheDocument();
    expect(agent.lastElementChild).toHaveClass("echo-theia-agent__composer");
    fireEvent.click(screen.getByRole("button", { name: "收起 Agent 面板" }));
    expect(container.querySelector(".coding-workbench--agent-closed")).toBeInTheDocument();
  });

  it("blocks an empty folder during verification without opening a follow-up round", async () => {
    render(<CodingWorkbench cwd="/repo" models={[]} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    const url = new URL(frame.src);
    act(() => {
      useTaskStore.setState({ root: "/repo", task: verificationTask() });
    });
    await waitFor(() => {
      act(() => {
        window.dispatchEvent(new MessageEvent("message", {
          origin: url.origin,
          source: frame.contentWindow,
          data: {
            type: "echo/before-mutation", id: "folder", operation: "createFolder",
            paths: ["/repo/new-folder"], token: bridgeToken(frame),
          },
        }));
      });
      expect(postMessage).toHaveBeenCalledWith(
        expect.objectContaining({ id: "folder", ok: false }), url.origin,
      );
    });
    expect(invoke).not.toHaveBeenCalledWith("coding_task_begin_followup", expect.anything());
  });

  it("keeps the task composer available while reviewing changes and verification", async () => {
    const user = userEvent.setup();
    useTaskStore.setState({ root: "/repo", task: verificationTask({ phase: "delivered", sessionId: "session-1" }) });
    const { container } = render(<CodingWorkbench cwd="/repo" sessionId="session-1" models={[{ id: "model-1" }]} defaultModelId="model-1" />);
    await screen.findByTitle("Echo Code IDE");
    const composer = container.querySelector(".echo-theia-agent__composer");
    const textbox = await screen.findByRole("textbox", { name: "给 Agent 的补充要求" });
    fireEvent.change(textbox, { target: { value: "请处理边界情况" } });
    await user.click(screen.getByRole("tab", { name: "任务变更" }));
    expect(composer).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "给 Agent 的补充要求" })).toHaveValue("请处理边界情况");
    await user.click(screen.getByRole("tab", { name: "验证" }));
    expect(screen.getByRole("textbox", { name: "给 Agent 的补充要求" })).toHaveValue("请处理边界情况");
  });

  it("opens the delivery report from a named action in the default IDE", async () => {
    useTaskStore.setState({ root: "/repo", task: verificationTask({ phase: "delivered" }) });
    render(<CodingWorkbench cwd="/repo" models={[]} />);
    await screen.findByTitle("Echo Code IDE");
    await userEvent.click(screen.getByRole("button", { name: "打开交付报告" }));
    expect(screen.getByRole("region", { name: "交付报告" })).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("coding_delivery_report", { root: "/repo", taskId: "verification-task" });
  });

  it("opens another folder while a coding task continues in the background", async () => {
    const onSelectWorkspace = vi.fn();
    const onToast = vi.fn();
    useTaskStore.setState({ root: "/repo", task: verificationTask({ phase: "implementing" }) });
    render(<CodingWorkbench cwd="/repo" models={[]} onSelectWorkspace={onSelectWorkspace} onToast={onToast} />);
    await screen.findByTitle("Echo Code IDE");
    await userEvent.click(screen.getByRole("button", { name: "切换项目" }));
    await userEvent.click(screen.getByRole("menuitem", { name: "打开其他文件夹…" }));
    expect(onSelectWorkspace).toHaveBeenCalledWith("/picked");
    expect(onToast).not.toHaveBeenCalledWith(expect.stringContaining("请先停止任务"));
  });

  it("creates an isolated Git worktree for a parallel task in the same project", async () => {
    const onSelectWorkspace = vi.fn();
    invoke.mockImplementation(async (command: string): Promise<unknown> => {
      if (command === "coding_task_list") return [{ id: "running-task", name: "正在开发", phase: "implementing", updatedAt: "now" }];
      if (command === "coding_verification_detect") return [];
      if (command === "coding_theia_start") return { url: "http://127.0.0.1:41773/", embedToken: "test-embed-token" };
      if (command === "coding_isolation_create") return { root: "/managed/parallel", sourceRoot: "/repo", baseHead: "abc123" };
      return null;
    });
    const onStartRun = vi.fn();
    const { rerender } = render(<CodingWorkbench
      cwd="/repo"
      models={[{ id: "model-1" }]}
      defaultModelId="model-1"
      apiReady
      onStartRun={onStartRun}
      onSelectWorkspace={onSelectWorkspace}
    />);
    await screen.findByTitle("Echo Code IDE");
    expect(await screen.findByText("当前项目有任务正在执行；提交新任务时会创建隔离工作树。")).toBeInTheDocument();
    act(() => useAiDraftStore.getState().setDraft({ prompt: "", contextPaths: ["src/orders.ts"], source: "context-menu" }));
    expect(await screen.findByText("orders.ts")).toBeInTheDocument();
    fireEvent.change(screen.getByRole("textbox", { name: "任务描述" }), { target: { value: "实现新的订单流程" } });
    await userEvent.click(screen.getByRole("button", { name: "开始 Agent 任务" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("coding_isolation_create", { root: "/repo" }));
    await waitFor(() => expect(onSelectWorkspace).toHaveBeenCalledWith("/managed/parallel"));
    expect(window.localStorage.getItem(codingTaskDraftKey("/managed/parallel"))).toBe("实现新的订单流程");
    expect(window.localStorage.getItem(codingTaskContextDraftKey("/managed/parallel"))).toBe('["src/orders.ts"]');
    expect(invoke).not.toHaveBeenCalledWith("coding_task_create", expect.anything());

    rerender(<CodingWorkbench
      cwd="/managed/parallel"
      models={[{ id: "model-1" }]}
      defaultModelId="model-1"
      apiReady
      onStartRun={onStartRun}
      onSelectWorkspace={onSelectWorkspace}
    />);
    expect(await screen.findByRole("textbox", { name: "任务描述" })).toHaveValue("实现新的订单流程");
    expect(await screen.findByText("orders.ts")).toBeInTheDocument();
    expect(window.localStorage.getItem(codingTaskContextDraftKey("/managed/parallel"))).toBeNull();
  });

  it("saves dirty Theia editors before switching projects", async () => {
    const onSelectWorkspace = vi.fn();
    render(<CodingWorkbench cwd="/repo" codingWorkspaces={[{ cwd: "/next" }]} models={[]} onSelectWorkspace={onSelectWorkspace} />);
    const frame = await screen.findByTitle("Echo Code IDE") as HTMLIFrameElement;
    const src = new URL(frame.src);
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    const send = (data: Record<string, unknown>) => {
      act(() => {
        window.dispatchEvent(new MessageEvent("message", {
          origin: src.origin,
          source: frame.contentWindow,
          data: { ...data, token: bridgeToken(frame) },
        }));
      });
    };
    send({ type: "echo/ready" });
    send({ type: "echo/dirty-state", count: 2 });
    expect(screen.getByLabelText("2 个未保存文件")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "切换项目" }));
    await userEvent.click(screen.getByRole("menuitem", { name: "切换到项目 next" }));
    await waitFor(() => expect(postMessage).toHaveBeenCalledWith(expect.objectContaining({ type: "echo/get-dirty" }), src.origin));
    expect(onSelectWorkspace).not.toHaveBeenCalled();
    const dirtyRequest = postMessage.mock.calls.find(([message]) => (message as { type?: string }).type === "echo/get-dirty")![0] as { id: string };
    send({ type: "echo/response", id: dirtyRequest.id, ok: true, value: 2 });

    await userEvent.click(await screen.findByRole("button", { name: "保存并继续" }));
    await waitFor(() => expect(postMessage).toHaveBeenCalledWith(expect.objectContaining({ type: "echo/save-all" }), src.origin));
    expect(onSelectWorkspace).not.toHaveBeenCalled();
    const saveRequest = postMessage.mock.calls.find(([message]) => (message as { type?: string }).type === "echo/save-all")![0] as { id: string };
    send({ type: "echo/response", id: saveRequest.id, ok: true, value: 0 });
    await waitFor(() => expect(onSelectWorkspace).toHaveBeenCalledWith("/next"));
  });

  it("keeps preview controls compact and lets the editor reclaim the Agent space", async () => {
    const user = userEvent.setup();
    const { container } = render(<CodingWorkbench cwd="/repo" models={[]} />);
    await screen.findByTitle("Echo Code IDE");

    expect(screen.queryByRole("textbox", { name: "网页预览地址" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "网页预览" }));
    expect(screen.getByRole("textbox", { name: "网页预览地址" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("textbox", { name: "网页预览地址" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "收起 Agent 面板" }));
    expect(container.querySelector(".coding-workbench--agent-closed")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "展开 Agent 面板" }));
    expect(container.querySelector(".coding-workbench--agent-closed")).not.toBeInTheDocument();
  });
});
