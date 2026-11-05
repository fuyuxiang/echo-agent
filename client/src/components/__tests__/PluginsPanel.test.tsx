import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { PluginsPanel } from "../PluginsPanel";
import { pluginsAction, pluginsList } from "@/lib/agent-client";
import { ensureSession } from "@/lib/ensure-session";
vi.mock("@/lib/ensure-session", () => ({ensureSession:vi.fn()}));

vi.mock("@/lib/agent-client", () => ({
  pluginsList: vi.fn(),
  pluginsAction: vi.fn(),
}));

beforeEach(() => vi.resetAllMocks());

describe("PluginsPanel load states", () => {
  it("keeps a failed load distinct from a successful empty list and allows retry", async () => {
    vi.mocked(pluginsList)
      .mockRejectedValueOnce(new Error("目录不可用"))
      .mockResolvedValueOnce({ plugins: [] });
    render(<PluginsPanel />);
    expect(await screen.findByRole("alert")).toHaveTextContent("目录不可用");
    expect(screen.getByText("插件列表未加载")).toBeInTheDocument();
    expect(screen.queryByText("暂无插件。")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText("暂无插件。")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("keeps the last loaded plugins when refresh fails", async () => {
    vi.mocked(pluginsList)
      .mockResolvedValueOnce({ plugins: [{ name: "评审插件", enabled: true, trusted: true, scope: "user" }] })
      .mockRejectedValueOnce(new Error("暂时无法刷新"));
    render(<PluginsPanel />);
    expect(await screen.findByText("评审插件")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "刷新" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("下方保留上次加载的结果");
    expect(screen.getByText("评审插件")).toBeInTheDocument();
    expect(screen.queryByText("暂无插件。")).toBeNull();
  });

  it("discards a previous project's delayed plugin list after switching sessions", async () => {
    let resolveOld!: (value: { plugins: { name: string; enabled: boolean; trusted: boolean }[] }) => void;
    vi.mocked(pluginsList)
      .mockReturnValueOnce(new Promise((resolve) => { resolveOld = resolve; }))
      .mockResolvedValueOnce({ plugins: [{ name: "当前项目插件", enabled: true, trusted: true }] });
    const { rerender } = render(<PluginsPanel sessionId="old-session" />);
    rerender(<PluginsPanel sessionId="new-session" />);
    expect(await screen.findByText("当前项目插件")).toBeInTheDocument();
    await act(async () => resolveOld({ plugins: [{ name: "旧项目插件", enabled: true, trusted: true }] }));
    expect(screen.queryByText("旧项目插件")).toBeNull();
    expect(screen.getByText("当前项目插件")).toBeInTheDocument();
  });
  it("refresh reloads local plugin definitions before displaying the new snapshot", async () => {
    vi.mocked(pluginsList).mockResolvedValueOnce({ plugins: [] })
      .mockResolvedValueOnce({ plugins: [{ id: "user/local", name: "新加入的本地插件", enabled: false }] });
    vi.mocked(pluginsAction).mockResolvedValueOnce({ status: "success" });
    render(<PluginsPanel sessionId="local-session" />);
    expect(await screen.findByText("暂无插件。")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "浏览市场" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "刷新" }));
    expect(await screen.findByText("新加入的本地插件")).toBeInTheDocument();
    expect(pluginsAction).toHaveBeenLastCalledWith("local-session", { type: "reload" });
    expect(screen.getByRole("button", { name: "启用插件 新加入的本地插件" })).toBeEnabled();
  });

  it("a failed local reload keeps the previous plugins available and reports the error", async () => {
    vi.mocked(pluginsList).mockResolvedValueOnce({ plugins: [{ id: "user/kept", name: "保留插件", enabled: true }] });
    vi.mocked(pluginsAction).mockRejectedValueOnce(new Error("重新扫描失败"));
    render(<PluginsPanel sessionId="local-session" />);
    await screen.findByText("保留插件");
    fireEvent.click(screen.getByRole("button", { name: "刷新" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("重新扫描失败");
    expect(screen.getByText("保留插件")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "禁用插件 保留插件" })).toBeEnabled();
  });

});

it("managed package removal lists affected plugins and retries refresh without uninstalling twice", async () => {
  vi.clearAllMocks();
  const plugin={id:"user/managed/review",name:"代码评审",enabled:true,management:{uninstallable:true,removable:false,packagePlugins:["代码评审","文档检查"]}};
  vi.mocked(pluginsList).mockResolvedValueOnce({plugins:[plugin]}).mockResolvedValue({plugins:[]});
  vi.mocked(pluginsAction).mockResolvedValueOnce({status:"success"}).mockRejectedValueOnce(new Error("重新加载失败")).mockResolvedValue({status:"success"});
  render(<PluginsPanel sessionId="managed-session" />);
  fireEvent.click(await screen.findByRole("button",{name:"插件 代码评审更多操作"}));
  fireEvent.click(screen.getByRole("menuitem",{name:"卸载插件 代码评审"}));
  const dialog=screen.getByRole("alertdialog");
  expect(dialog).toHaveTextContent("文档检查");
  fireEvent.click(screen.getByRole("button",{name:"卸载插件"}));
  expect(await screen.findByRole("alert")).toHaveTextContent("重新加载失败");
  fireEvent.click(screen.getByRole("button",{name:"卸载插件"}));
  expect(await screen.findByText("暂无插件。")).toBeInTheDocument();
  expect(vi.mocked(pluginsAction).mock.calls.filter(([,action])=>(action as {type:string}).type==="uninstall")).toHaveLength(1);
});

it("卸载后的迟到列表不能覆盖切换后的会话，包括切换回同一会话", async () => {
  const plugin={id:"user/old/plugin",name:"旧插件",enabled:true,management:{uninstallable:true,removable:false,packagePlugins:["旧插件"]}};
  let finish!: (value: {plugins:typeof plugin[]})=>void;
  vi.mocked(pluginsList).mockResolvedValueOnce({plugins:[plugin]})
    .mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;}))
    .mockResolvedValueOnce({plugins:[{id:"new",name:"新项目插件",enabled:true}]})
    .mockResolvedValueOnce({plugins:[{id:"current",name:"当前列表",enabled:true}]});
  vi.mocked(pluginsAction).mockResolvedValue({status:"success"});
  const {rerender}=render(<PluginsPanel sessionId="old" />);
  fireEvent.click(await screen.findByRole("button",{name:"插件 旧插件更多操作"}));
  fireEvent.click(screen.getByRole("menuitem",{name:"卸载插件 旧插件"}));
  fireEvent.click(screen.getByRole("button",{name:"卸载插件"}));
  await waitFor(()=>expect(finish).toBeTypeOf("function"));
  rerender(<PluginsPanel sessionId="new" />);
  await screen.findByText("新项目插件");
  rerender(<PluginsPanel sessionId="old" />);
  await screen.findByText("当前列表");
  await act(async()=>finish({plugins:[plugin]}));
  expect(screen.getByText("当前列表")).toBeInTheDocument();
  expect(screen.queryByText("旧插件")).toBeNull();
});

it("从无任务开始卸载时，自动创建会话不会关闭失败重试弹窗", async () => {
  const plugin={id:"user/auto/plugin",name:"自动会话插件",enabled:true,management:{uninstallable:true,removable:false,packagePlugins:["自动会话插件"]}};
  vi.mocked(pluginsList).mockResolvedValue({plugins:[plugin]});
  let reject!: (reason:Error)=>void;
  vi.mocked(pluginsAction).mockImplementationOnce(()=>new Promise((_,fail)=>{reject=fail;}));
  const {rerender}=render(<PluginsPanel />);
  vi.mocked(ensureSession).mockImplementation(async()=>{rerender(<PluginsPanel sessionId="auto-session" />);return "auto-session";});
  fireEvent.click(await screen.findByRole("button",{name:"插件 自动会话插件更多操作"}));
  fireEvent.click(screen.getByRole("menuitem",{name:"卸载插件 自动会话插件"}));
  fireEvent.click(screen.getByRole("button",{name:"卸载插件"}));
  await waitFor(()=>expect(reject).toBeTypeOf("function"));
  await act(async()=>reject(new Error("安装目录暂时不可写")));
  expect(screen.getByRole("alertdialog")).toHaveTextContent("安装目录暂时不可写");
  expect(screen.getByRole("button",{name:"卸载插件"})).toBeEnabled();
});
