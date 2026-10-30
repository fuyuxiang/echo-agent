import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { CloudStoragePanel } from "../CloudStoragePanel";
import { getStorageProvider, hydrateStorageProviders, listStorageProviders, removeStorageProviderConfig, saveStorageProviderConfig } from "@/lib/cloud-storage";
import { open, save } from "@tauri-apps/plugin-dialog";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(), save: vi.fn() }));
vi.mock("@/lib/cloud-storage", () => ({
  hydrateStorageProviders: vi.fn().mockResolvedValue([{
    id: "dav", label: "团队网盘", kind: "webdav", baseUrl: "https://dav.example.com", username: "alice", password: "••••", enabled: true,
  }]),
  saveStorageProviderConfig: vi.fn().mockResolvedValue(undefined),
  removeStorageProviderConfig: vi.fn(),
  testStorageProviderConfig: vi.fn(),
  createTauriWebDavProvider: vi.fn((config) => ({ ...config })),
  registerStorageProvider: vi.fn(),
  unregisterStorageProvider: vi.fn(),
  listStorageProviders: vi.fn(() => [{ id: "dav", label: "团队网盘" }]),
  getStorageProvider: vi.fn(),
  normalizePath: vi.fn((path: string) => path.replace(/\/+/g, "/") || "/"),
  joinStoragePath: vi.fn((base: string, name: string) => (base === "/" ? "" : base) + "/" + name),
}));

describe("CloudStoragePanel", () => {
  const provider = {
    list: vi.fn().mockResolvedValue([{ path: "/report.pdf", name: "report.pdf", isDir: false, size: 2048 }]),
    readText: vi.fn(), writeText: vi.fn(), delete: vi.fn(), makeDir: vi.fn(),
    uploadFile: vi.fn().mockResolvedValue(10),
    downloadFile: vi.fn().mockResolvedValue(2048),
  };

  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(hydrateStorageProviders).mockReset().mockResolvedValue([{
      id: "dav", label: "团队网盘", kind: "webdav", baseUrl: "https://dav.example.com", username: "alice", password: "••••", enabled: true,
    }]);
    vi.mocked(listStorageProviders).mockReset().mockReturnValue([{ id: "dav", label: "团队网盘" }]);
    vi.mocked(saveStorageProviderConfig).mockReset().mockResolvedValue(undefined);
    provider.list.mockReset().mockResolvedValue([{ path: "/report.pdf", name: "report.pdf", isDir: false, size: 2048 }]);
    provider.readText.mockReset();
    vi.mocked(getStorageProvider).mockReturnValue(provider as never);
  });

  async function selectProvider() {
    render(<CloudStoragePanel />);
    const select = await screen.findByRole("combobox");
    fireEvent.change(select, { target: { value: "dav" } });
    await screen.findByText("report.pdf");
  }

  it("取消存储源编辑需要确认，继续编辑保留草稿", async () => {
    await selectProvider();
    fireEvent.click(screen.getByRole("button", { name: "编辑配置" }));
    const name = screen.getByRole("textbox", { name: "存储源显示名" });
    fireEvent.change(name, { target: { value: "新的团队名称" } });
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    let dialog = await screen.findByRole("alertdialog", { name: "舍弃存储源的修改？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));
    expect(name).toHaveValue("新的团队名称");
    expect(saveStorageProviderConfig).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    dialog = await screen.findByRole("alertdialog", { name: "舍弃存储源的修改？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "舍弃修改" }));
    expect(screen.queryByRole("textbox", { name: "存储源显示名" })).toBeNull();
  });

  it("可编辑已有配置并保留密码掩码", async () => {
    await selectProvider();
    fireEvent.click(screen.getByRole("button", { name: "编辑配置" }));
    expect(screen.getByRole("textbox", { name: "存储源显示名" })).toHaveValue("团队网盘");
    expect(screen.getByDisplayValue("••••")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(saveStorageProviderConfig).toHaveBeenCalledWith(expect.objectContaining({ id: "dav", password: "••••" })));
  });

  it("存储配置保存中防止重复提交", async () => {
    let release!: () => void;
    vi.mocked(saveStorageProviderConfig).mockImplementationOnce(() => new Promise<void>((resolve) => {
      release = resolve;
    }));
    await selectProvider();
    fireEvent.click(screen.getByRole("button", { name: "编辑配置" }));
    const saveButton = screen.getByRole("button", { name: "保存" });
    fireEvent.click(saveButton);
    fireEvent.click(saveButton);

    expect(saveStorageProviderConfig).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "保存中…" })).toBeDisabled();
    release();
    await waitFor(() => expect(screen.queryByRole("button", { name: "保存中…" })).toBeNull());
  });

  it("上传和下载都使用原生文件选择结果", async () => {
    vi.mocked(open).mockResolvedValue("/tmp/upload.txt");
    vi.mocked(save).mockResolvedValue("/tmp/report.pdf");
    await selectProvider();
    fireEvent.click(screen.getByRole("button", { name: "上传文件" }));
    await waitFor(() => expect(provider.uploadFile).toHaveBeenCalledWith("/upload.txt", "/tmp/upload.txt"));
    await waitFor(() => expect(screen.getByRole("button", { name: "上传文件" })).not.toBeDisabled());
    fireEvent.click(screen.getByRole("button", { name: "下载 report.pdf" }));
    await waitFor(() => expect(provider.downloadFile).toHaveBeenCalledWith("/report.pdf", "/tmp/report.pdf"));
  });

  it("同名文件要明确确认，取消不会写入远端", async () => {
    vi.mocked(open).mockResolvedValue("/tmp/report.pdf");
    await selectProvider();
    fireEvent.click(screen.getByRole("button", { name: "上传文件" }));
    const dialog = await screen.findByRole("alertdialog", { name: "覆盖 1 个同名文件？" });
    expect(provider.uploadFile).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));
    expect(provider.uploadFile).not.toHaveBeenCalled();
  });

  it("新建文本在一个应用内表单填写文件名和内容", async () => {
    provider.writeText.mockResolvedValueOnce(true);
    await selectProvider();
    fireEvent.click(screen.getByRole("button", { name: "新建文本" }));
    const dialog = screen.getByRole("dialog", { name: "新建文本文件" });
    fireEvent.change(within(dialog).getByRole("textbox", { name: /文件名/ }), { target: { value: "readme.md" } });
    fireEvent.change(within(dialog).getByRole("textbox", { name: "文件内容" }), { target: { value: "hello" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "创建文件" }));
    await waitFor(() => expect(provider.writeText).toHaveBeenCalledWith("/readme.md", "hello"));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "新建文本文件" })).toBeNull());
  });

  it("存储配置加载失败不伪装成未配置，并可重试", async () => {
    vi.mocked(hydrateStorageProviders).mockRejectedValueOnce(new Error("config locked"));
    render(<CloudStoragePanel />);
    expect(await screen.findByRole("alert")).toHaveTextContent("config locked");
    expect(screen.queryByText("未配置存储源")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByRole("combobox")).toBeInTheDocument();
  });

  it("切换存储源前确认舍弃未保存内容，取消时保留当前文件", async () => {
    vi.mocked(listStorageProviders).mockReturnValueOnce([
      { id: "dav", label: "团队网盘" },
      { id: "other", label: "其他网盘" },
    ]);
    provider.readText.mockResolvedValueOnce("原始内容");
    await selectProvider();
    fireEvent.click(screen.getByRole("button", { name: "读取 report.pdf" }));
    const editor = await screen.findByRole("textbox", { name: "编辑 report.pdf" });
    fireEvent.change(editor, { target: { value: "尚未保存" } });
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "other" } });
    const dialog = await screen.findByRole("alertdialog", { name: "舍弃未保存的文件修改？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));
    expect(screen.getByRole("combobox")).toHaveValue("dav");
    expect(editor).toHaveValue("尚未保存");
  });

  it("保存预览始终写回原存储源，切换后不残留预览", async () => {
    vi.mocked(listStorageProviders).mockReturnValueOnce([
      { id: "dav", label: "团队网盘" },
      { id: "other", label: "其他网盘" },
    ]);
    const otherProvider = { ...provider, list: vi.fn().mockResolvedValue([]), writeText: vi.fn() };
    vi.mocked(getStorageProvider).mockImplementation((id) => (id === "other" ? otherProvider : provider) as never);
    provider.readText.mockResolvedValueOnce("原始内容");
    let release!: (value: boolean) => void;
    provider.writeText.mockImplementationOnce(() => new Promise<boolean>((resolve) => { release = resolve; }));
    await selectProvider();
    fireEvent.click(screen.getByRole("button", { name: "读取 report.pdf" }));
    const editor = await screen.findByRole("textbox", { name: "编辑 report.pdf" });
    fireEvent.change(editor, { target: { value: "修改后的内容" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "other" } });
    fireEvent.click(within(await screen.findByRole("alertdialog", { name: "舍弃未保存的文件修改？" })).getByRole("button", { name: "舍弃修改" }));
    release(true);
    await waitFor(() => expect(screen.getByRole("combobox")).toHaveValue("other"));
    expect(screen.queryByRole("textbox", { name: "编辑 report.pdf" })).toBeNull();
    expect(provider.writeText).toHaveBeenCalledWith("/report.pdf", "修改后的内容");
    expect(otherProvider.writeText).not.toHaveBeenCalled();
  });

  it("文件保存成功就地显示已保存，不弹重复成功提示", async () => {
    provider.readText.mockResolvedValueOnce("原始内容");
    provider.writeText.mockResolvedValueOnce(true);
    const onToast = vi.fn();
    render(<CloudStoragePanel onToast={onToast} />);
    fireEvent.change(await screen.findByRole("combobox"), { target: { value: "dav" } });
    fireEvent.click(await screen.findByRole("button", { name: "读取 report.pdf" }));
    const editor = await screen.findByRole("textbox", { name: "编辑 report.pdf" });
    expect(screen.getByRole("button", { name: "已保存" })).toBeDisabled();
    fireEvent.change(editor, { target: { value: "新内容" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "已保存" })).toBeDisabled());
    expect(provider.writeText).toHaveBeenCalledWith("/report.pdf", "新内容");
    expect(onToast).not.toHaveBeenCalled();
  });

  it("APP-CLOUD02 已禁用配置仍可选择编辑，保存启用后恢复浏览", async () => {
    vi.mocked(hydrateStorageProviders).mockResolvedValue([{
      id: "dav", label: "团队网盘", kind: "webdav", baseUrl: "https://dav.example.com", enabled: false,
    }]);
    vi.mocked(listStorageProviders).mockReturnValueOnce([]);
    const { container } = render(<CloudStoragePanel />);
    const select = await screen.findByRole("combobox");
    expect(screen.getByRole("option", { name: /团队网盘.*已禁用/ })).toHaveValue("dav");
    fireEvent.change(select, { target: { value: "dav" } });
    expect(container.querySelector(".storage-panel__breadcrumb")).toBeNull();
    expect(provider.list).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "编辑配置" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "启用此存储源" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await screen.findByText("report.pdf");
    expect(saveStorageProviderConfig).toHaveBeenCalledWith(expect.objectContaining({ id: "dav", enabled: true }));
    expect(select).toHaveValue("dav");
    expect(provider.list).toHaveBeenCalledTimes(1);
  });

  it("APP-CLOUD02 可移除禁用配置而不调用远端文件操作", async () => {
    vi.mocked(hydrateStorageProviders).mockResolvedValue([{
      id: "dav", label: "团队网盘", kind: "webdav", baseUrl: "https://dav.example.com", enabled: false,
    }]);
    vi.mocked(listStorageProviders).mockReturnValue([]);
    render(<CloudStoragePanel />);
    fireEvent.change(await screen.findByRole("combobox"), { target: { value: "dav" } });
    fireEvent.click(screen.getByRole("button", { name: "移除配置" }));
    const dialog = await screen.findByRole("alertdialog", { name: "移除存储源“团队网盘”？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "移除配置" }));
    await waitFor(() => expect(removeStorageProviderConfig).toHaveBeenCalledWith("dav"));
    await screen.findByText("未配置存储源");
    expect(provider.list).not.toHaveBeenCalled();
    expect(provider.delete).not.toHaveBeenCalled();
  });

  it("APP-CLOUD02 禁用前取消舍弃确认保留草稿和启用配置", async () => {
    provider.readText.mockResolvedValueOnce("原始内容");
    await selectProvider();
    fireEvent.click(screen.getByRole("button", { name: "读取 report.pdf" }));
    const editor = await screen.findByRole("textbox", { name: "编辑 report.pdf" });
    fireEvent.change(editor, { target: { value: "尚未保存" } });
    fireEvent.click(screen.getByRole("button", { name: "编辑配置" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "启用此存储源" }));
    fireEvent.click(within(document.querySelector(".storage-panel__config") as HTMLElement).getByRole("button", { name: "保存" }));
    const dialog = await screen.findByRole("alertdialog", { name: "舍弃未保存的文件修改？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));
    expect(saveStorageProviderConfig).not.toHaveBeenCalled();
    expect(editor).toHaveValue("尚未保存");
    expect(screen.getByRole("combobox")).toHaveValue("dav");
    fireEvent.click(within(document.querySelector(".storage-panel__config") as HTMLElement).getByRole("button", { name: "保存" }));
    const confirmed = await screen.findByRole("alertdialog", { name: "舍弃未保存的文件修改？" });
    fireEvent.click(within(confirmed).getByRole("button", { name: "舍弃修改并禁用" }));
    await waitFor(() => expect(saveStorageProviderConfig).toHaveBeenCalledWith(expect.objectContaining({ id: "dav", enabled: false })));
    expect(screen.queryByRole("region", { name: "文件预览：report.pdf" })).toBeNull();
  });

  it("APP-CLOUD02 禁用后同id重新启用刷新一次，旧目录响应不能覆盖", async () => {
    let release!: (entries: Array<{ path: string; name: string; isDir: boolean }>) => void;
    provider.list.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    render(<CloudStoragePanel />);
    const select = await screen.findByRole("combobox");
    fireEvent.change(select, { target: { value: "dav" } });
    await waitFor(() => expect(provider.list).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "编辑配置" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "启用此存储源" }));
    vi.mocked(listStorageProviders).mockReturnValue([]);
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(screen.queryByText("编辑 WebDAV")).toBeNull());
    expect(screen.getByRole("combobox")).toHaveValue("dav");
    fireEvent.click(screen.getByRole("button", { name: "编辑配置" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "启用此存储源" }));
    vi.mocked(listStorageProviders).mockReturnValue([{ id: "dav", label: "团队网盘" }]);
    provider.list.mockResolvedValueOnce([{ path: "/new.txt", name: "new.txt", isDir: false }]);
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await screen.findByText("new.txt");
    await act(async () => { release([{ path: "/stale.txt", name: "stale.txt", isDir: false }]); });
    expect(screen.queryByText("stale.txt")).toBeNull();
    expect(screen.getByText("new.txt")).toBeInTheDocument();
    expect(provider.list).toHaveBeenCalledTimes(2);
  });

  it("APP-CLOUD02 禁用时失效未完成GET，不恢复文件预览", async () => {
    let release!: (text: string) => void;
    provider.readText.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    await selectProvider();
    fireEvent.click(screen.getByRole("button", { name: "读取 report.pdf" }));
    fireEvent.click(screen.getByRole("button", { name: "编辑配置" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "启用此存储源" }));
    vi.mocked(listStorageProviders).mockReturnValue([]);
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(screen.queryByText("编辑 WebDAV")).toBeNull());
    await act(async () => { release("旧来源迟到内容"); });
    expect(screen.queryByRole("region", { name: "文件预览：report.pdf" })).toBeNull();
  });

  it("APP-CLOUD02 配置保存迟到不刷新已切换的registry-only来源", async () => {
    vi.mocked(listStorageProviders).mockReturnValue([
      { id: "dav", label: "团队网盘" }, { id: "other", label: "临时注册源" },
    ]);
    const other = { ...provider, list: vi.fn().mockResolvedValue([{ path: "/other.txt", name: "other.txt", isDir: false }]) };
    vi.mocked(getStorageProvider).mockImplementation((id) => (id === "other" ? other : provider) as never);
    let release!: () => void;
    vi.mocked(saveStorageProviderConfig).mockImplementationOnce(() => new Promise<void>((resolve) => { release = resolve; }));
    await selectProvider();
    fireEvent.click(screen.getByRole("button", { name: "编辑配置" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "other" } });
    await screen.findByText("other.txt");
    await act(async () => { release(); });
    expect(screen.getByRole("combobox")).toHaveValue("other");
    expect(screen.getByText("other.txt")).toBeInTheDocument();
    expect(provider.list).toHaveBeenCalledTimes(1);
  });
});
