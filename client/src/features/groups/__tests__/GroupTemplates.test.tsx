import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { agentsList } from "@/lib/agent-client";
import type { AgentEntry } from "@/lib/types";
import { GroupTeams, TemplatePicker } from "../GroupManagement";
import { GroupWorkspace } from "../GroupWorkspace";
import { groupCreate, groupDeleteTeam, groupTemplates, type TeamTemplate } from "../group-client";
import { useGroupStore } from "../group-store";
import { readGroupCreationDraft } from "../group-creation-draft";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []) }));
vi.mock("@/lib/agent-client", async original => ({ ...await original<typeof import("@/lib/agent-client")>(), agentsList: vi.fn() }));
vi.mock("../group-client", async original => ({ ...await original<typeof import("../group-client")>(), groupTemplates: vi.fn(), groupDeleteTeam: vi.fn(), groupCreate: vi.fn() }));

const template: TeamTemplate = {
  id: "saved", name: "交付模板", description: "分析需求并交付", source: "常用团队",
  members: [{ name: "代码专家", role: "评审代码", modelId: "member-model" }],
  coordinatorName: "代码专家", coordinatorRole: "组织评审", defaultModelId: "template-model",
};
const experts = ["代码专家", "文档助手"].map(name => ({ name, scope: "user", description: "本地专家", raw: "", path: `/agents/${name}.md`, modelTags: [] })) as AgentEntry[];
beforeEach(() => {
  vi.resetAllMocks(); localStorage.clear();
  vi.mocked(groupTemplates).mockResolvedValue([]);
  vi.mocked(agentsList).mockResolvedValue(experts);
  vi.mocked(groupDeleteTeam).mockResolvedValue(undefined);
  vi.mocked(groupCreate).mockRejectedValue(new Error("验收中保留表单"));
  useGroupStore.setState({ groups: [], current: null, currentId: null, createTemplate: null, createFrom: null, loading: false, error: null, drafts: {}, outbox: {} });
});
afterEach(cleanup);
const mountWorkspace = () => render(<GroupWorkspace cwd="/work" modelId="default-model" models={[
  { id: "default-model", label: "默认模型" }, { id: "template-model", label: "模板模型" }, { id: "member-model", label: "成员模型" },
]} workspaces={[]} onToast={vi.fn()} onOpenSettings={vi.fn()} />);

it("没有保存模板时展示说明，不出现含历史团队的下拉框；模板页可直接开始创建", async () => {
  const picker = render(<TemplatePicker onPick={vi.fn()} />);
  expect(await screen.findByText("还没有保存的专家团模板，可直接选择下方专家创建。")).toBeInTheDocument();
  expect(screen.queryByRole("combobox")).toBeNull();
  picker.unmount();
  const onCreate = vi.fn();
  render(<GroupTeams onBack={vi.fn()} onUse={vi.fn()} onCreate={onCreate} />);
  await screen.findByText("把合适的协作组合留下来");
  fireEvent.click(screen.getByRole("button", { name: "创建专家团" }));
  expect(onCreate).toHaveBeenCalledOnce();
});

it("读取失败显示原因并可重试，失败状态不会伪装成空模板库", async () => {
  vi.mocked(groupTemplates).mockRejectedValueOnce(new Error("数据库暂不可读")).mockResolvedValueOnce([template]);
  render(<TemplatePicker onPick={vi.fn()} />);
  expect(await screen.findByRole("alert")).toHaveTextContent("数据库暂不可读");
  expect(screen.queryByText(/还没有保存的专家团模板/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "刷新模板" }));
  const select = await screen.findByRole("combobox", { name: "使用专家团模板" });
  expect(within(select).getByRole("option", { name: "交付模板 · 1 位专家" })).toBeInTheDocument();
  expect(screen.queryByRole("alert")).toBeNull();
});

it("刷新后模板被删除会取消模板关联，但保留用户已填写的创建配置", async () => {
  const onPick = vi.fn();
  vi.mocked(groupTemplates).mockResolvedValueOnce([template]).mockResolvedValueOnce([]);
  render(<TemplatePicker onPick={onPick} initialTemplateId={template.id} />);
  expect(await screen.findByRole("combobox")).toHaveValue(template.id);
  fireEvent.click(screen.getByRole("button", { name: "刷新模板" }));
  expect(await screen.findByText("所选模板已删除，已保留当前填写的配置，可继续调整并创建专家团。")).toBeInTheDocument();
  expect(onPick).not.toHaveBeenCalled();
  expect(screen.queryByRole("combobox")).toBeNull();
});

it("模板复用准确填充；切回自行选择会清空名称目标成员职责模型及协调员设置", async () => {
  vi.mocked(groupTemplates).mockResolvedValue([template]);
  useGroupStore.setState({ createTemplate: template });
  mountWorkspace();
  fireEvent.click(screen.getByRole("button", { name: "创建专家团" }));
  const select = await screen.findByRole("combobox", { name: "使用专家团模板" });
  expect(select).toHaveValue(template.id);
  expect(screen.getByRole("textbox", { name: "专家团名称" })).toHaveValue(template.name);
  expect(screen.getByRole("textbox", { name: "协作目标" })).toHaveValue(template.description);
  expect(screen.getByRole("checkbox", { name: /代码专家/ })).toBeChecked();
  expect(screen.getByRole("combobox", { name: "执行模型" })).toHaveValue("template-model");
  fireEvent.change(select, { target: { value: "" } });
  expect(screen.getByRole("textbox", { name: "专家团名称" })).toHaveValue("");
  expect(screen.getByRole("textbox", { name: "协作目标" })).toHaveValue("");
  expect(screen.getByRole("checkbox", { name: /代码专家/ })).not.toBeChecked();
  expect(screen.getByRole("combobox", { name: "执行模型" })).toHaveValue("default-model");
  fireEvent.change(screen.getByRole("textbox", { name: "专家团名称" }), { target: { value: "手动编组" } });
  fireEvent.click(screen.getByRole("checkbox", { name: /文档助手/ }));
  fireEvent.click(screen.getByRole("button", { name: "创建并进入专家团" }));
  await waitFor(() => expect(groupCreate).toHaveBeenCalledWith(expect.objectContaining({ name: "手动编组", goal: "", members: ["文档助手"], modelId: "default-model", roles: {}, models: {}, coordinatorName: undefined, coordinatorRole: undefined })));
});

it("切换到没有默认模型的模板时恢复默认执行模型，不沿用上一模板", async () => {
  const second = { ...template, id: "second", name: "另一个模板", defaultModelId: null };
  vi.mocked(groupTemplates).mockResolvedValue([template, second]);
  mountWorkspace(); fireEvent.click(screen.getByRole("button", { name: "创建专家团" }));
  const select = await screen.findByRole("combobox", { name: "使用专家团模板" });
  fireEvent.change(select, { target: { value: template.id } });
  expect(screen.getByRole("combobox", { name: "执行模型" })).toHaveValue("template-model");
  fireEvent.change(select, { target: { value: second.id } });
  expect(screen.getByRole("combobox", { name: "执行模型" })).toHaveValue("default-model");
});

it("失效模板保留在库内并标明缺失专家，创建时可移除并改选有效成员", async () => {
  const stale = { ...template, members: [{ name: "已删除专家", role: "旧职责" }] };
  vi.mocked(groupTemplates).mockResolvedValue([stale]);
  mountWorkspace(); fireEvent.click(screen.getByRole("button", { name: "专家团模板" }));
  expect(await screen.findByText(/缺少本地专家：已删除专家/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "用此模板创建专家团" }));
  await screen.findByRole("checkbox", { name: /代码专家/ });
  expect(screen.getByRole("button", { name: "创建并进入专家团" })).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "移除失效选择" }));
  fireEvent.click(screen.getByRole("checkbox", { name: /代码专家/ }));
  expect(screen.getByRole("button", { name: "创建并进入专家团" })).toBeEnabled();
});

it("模板删除失败可重试，成功后同页进入空状态", async () => {
  vi.mocked(groupTemplates).mockResolvedValueOnce([template]).mockResolvedValueOnce([]);
  vi.mocked(groupDeleteTeam).mockRejectedValueOnce(new Error("磁盘暂不可写")).mockResolvedValueOnce(undefined);
  render(<GroupTeams onBack={vi.fn()} onUse={vi.fn()} onCreate={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: "删除模板" }));
  const dialog = screen.getByRole("alertdialog");
  fireEvent.click(within(dialog).getByRole("button", { name: "删除模板" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent("磁盘暂不可写");
  expect(screen.getByText(template.name)).toBeInTheDocument();
  fireEvent.click(within(dialog).getByRole("button", { name: "删除模板" }));
  expect(await screen.findByText("把合适的协作组合留下来")).toBeInTheDocument();
});

it("从会话团队创建的入口保持独立，并提交准确的来源和成员职责", async () => {
  useGroupStore.setState({ createFrom: {
    sessionId: "source-session", team: { teamId: "本会话团队", members: ["代码专家"], roles: { "代码专家": "检查边界" }, description: "会话中的协作目标", createdAt: 1, sessionId: "source-session", promptIds: [] },
  } });
  mountWorkspace();
  expect(await screen.findByRole("textbox", { name: "专家团名称" })).toHaveValue("本会话团队");
  await screen.findByRole("checkbox", { name: /代码专家/ });
  expect(screen.queryByRole("combobox", { name: "使用专家团模板" })).toBeNull();
  expect(groupTemplates).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "创建并进入专家团" }));
  await waitFor(() => expect(groupCreate).toHaveBeenCalledWith(expect.objectContaining({ sourceSessionId: "source-session", sourceTeamId: "本会话团队", members: ["代码专家"], roles: { "代码专家": "检查边界" } })));
});

it("不可用的执行模型明确显示原因，重新选择可用模型后才能创建", async () => {
  const stale = { ...template, defaultModelId: "removed-model" };
  vi.mocked(groupTemplates).mockResolvedValue([stale]);
  useGroupStore.setState({ createTemplate: stale });
  mountWorkspace(); fireEvent.click(screen.getByRole("button", { name: "创建专家团" }));
  await screen.findByRole("checkbox", { name: /代码专家/ });
  expect(screen.getByRole("option", { name: "不可用：removed-model，请重新选择" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "创建并进入专家团" })).toBeDisabled();
  fireEvent.change(screen.getByRole("combobox", { name: "执行模型" }), { target: { value: "default-model" } });
  expect(screen.getByRole("button", { name: "创建并进入专家团" })).toBeEnabled();
});

it("专家读取失败不会误判模板成员失效，重读成功后保留选择并恢复创建", async () => {
  vi.mocked(groupTemplates).mockResolvedValue([template]);
  vi.mocked(agentsList).mockRejectedValueOnce(new Error("专家目录暂不可读")).mockResolvedValueOnce(experts);
  useGroupStore.setState({ createTemplate: template });
  mountWorkspace(); fireEvent.click(screen.getByRole("button", { name: "创建专家团" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("专家目录暂不可读");
  expect(screen.queryByRole("button", { name: "移除失效选择" })).toBeNull();
  expect(screen.queryByText(/还没有本地专家/)).toBeNull();
  expect(screen.getByRole("button", { name: "创建并进入专家团" })).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "重新读取专家" }));
  expect(await screen.findByRole("checkbox", { name: /代码专家/ })).toBeChecked();
  expect(screen.getByRole("button", { name: "创建并进入专家团" })).toBeEnabled();
});

it("ignores a late creation response after leaving and reuses the request identity on retry", async () => {
  let reject!: (reason: Error) => void;
  vi.mocked(groupCreate).mockImplementationOnce(() => new Promise((_resolve, fail) => { reject = fail; }));
  mountWorkspace(); fireEvent.click(screen.getByRole("button", {name: "创建专家团"}));
  await screen.findByRole("checkbox", {name: /代码专家/});
  fireEvent.change(screen.getByRole("textbox", {name: "专家团名称"}), {target:{value:"retry"}});
  fireEvent.click(screen.getByRole("checkbox", {name: /代码专家/}));
  fireEvent.click(screen.getByRole("button", {name: "创建并进入专家团"}));
  await waitFor(() => expect(groupCreate).toHaveBeenCalledTimes(1));
  const requestId = vi.mocked(groupCreate).mock.calls[0][0].requestId;
  expect(requestId).toBeTruthy();
  await act(async () => { reject(new Error("response lost")); });
  let resolve!: (value: Awaited<ReturnType<typeof groupCreate>>) => void;
  vi.mocked(groupCreate).mockImplementationOnce(() => new Promise(done => { resolve = done; }));
  fireEvent.click(screen.getByRole("button", {name: "创建并进入专家团"}));
  await waitFor(() => expect(groupCreate).toHaveBeenCalledTimes(2));
  expect(vi.mocked(groupCreate).mock.calls[1][0].requestId).toBe(requestId);
  fireEvent.click(screen.getByRole("button", {name: "返回专家团"}));
  await act(async () => { resolve({id:"late"} as Awaited<ReturnType<typeof groupCreate>>); });
  expect(useGroupStore.getState().currentId).not.toBe("late");
});

it("名称可以自动建议，创建草稿在离开后恢复，取消会清除草稿", async () => {
  let view = mountWorkspace();
  fireEvent.click(screen.getByRole("button",{name:"创建专家团"}));
  await screen.findByRole("checkbox",{name:/代码专家/});
  fireEvent.click(screen.getByRole("checkbox",{name:/代码专家/}));
  fireEvent.change(screen.getByRole("textbox",{name:"协作目标"}),{target:{value:"保留创建中的目标"}});
  fireEvent.click(screen.getByRole("button",{name:"创建并进入专家团"}));
  await waitFor(()=>expect(groupCreate).toHaveBeenCalledWith(expect.objectContaining({name:"代码专家协作组",goal:"保留创建中的目标"})));
  view.unmount();
  view = mountWorkspace();
  expect(screen.getByRole("textbox",{name:"协作目标"})).toHaveValue("保留创建中的目标");
  expect(await screen.findByRole("checkbox",{name:/代码专家/})).toBeChecked();
  fireEvent.click(screen.getByRole("button",{name:"取消"}));
  expect(useGroupStore.getState().drafts["create:pending"]).toBeUndefined();
  expect(screen.getByRole("button",{name:"创建专家团"})).toBeInTheDocument();
});

it("模板创建草稿恢复用户修改，重试沿用已持久化的创建请求标识", async () => {
  vi.mocked(groupTemplates).mockResolvedValue([template]);
  useGroupStore.setState({ createTemplate: template });
  let view = mountWorkspace();
  fireEvent.click(screen.getByRole("button",{name:"创建专家团"}));
  await screen.findByRole("checkbox",{name:/代码专家/});
  fireEvent.change(screen.getByRole("textbox",{name:"协作目标"}),{target:{value:"修改后的模板目标"}});
  fireEvent.click(screen.getByRole("button",{name:"创建并进入专家团"}));
  await waitFor(()=>expect(groupCreate).toHaveBeenCalledOnce());
  const requestId = vi.mocked(groupCreate).mock.calls[0][0].requestId;
  view.unmount();
  view = mountWorkspace();
  expect(screen.getByRole("textbox",{name:"协作目标"})).toHaveValue("修改后的模板目标");
  await screen.findByRole("checkbox",{name:/代码专家/});
  fireEvent.click(screen.getByRole("button",{name:"创建并进入专家团"}));
  await waitFor(()=>expect(groupCreate).toHaveBeenCalledTimes(2));
  expect(vi.mocked(groupCreate).mock.calls[1][0].requestId).toBe(requestId);
});

it("来源创建在重启后恢复引用正文与原请求身份，迟到成功不会重复建群", async () => {
  useGroupStore.setState({ createFrom: { sessionId: "source-session", team: { teamId: "来源团队", description: "来源目标", members: ["代码专家"], roles: { "代码专家": "检查边界" } }, referenceText: "必须支持离线登录" } });
  const first = mountWorkspace();
  await screen.findByRole("checkbox", { name: /代码专家/ });
  fireEvent.click(screen.getByText("引用来源对话", { exact: true }));
  fireEvent.click(screen.getByRole("checkbox", { name: "把下列内容共享给成员" }));
  fireEvent.click(screen.getByRole("button", { name: "创建并进入专家团" }));
  await screen.findByText("验收中保留表单");
  const input = vi.mocked(groupCreate).mock.calls[0][0];
  first.unmount();
  // Simulate a fresh renderer: the durable draft survives, transient source state does not.
  useGroupStore.setState({ createFrom: null, createTemplate: null });
  mountWorkspace();
  await screen.findByRole("checkbox", { name: /代码专家/ });
  expect(screen.getByRole("checkbox", { name: "把下列内容共享给成员" })).toBeChecked();
  expect(screen.getByText("必须支持离线登录")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "创建并进入专家团" }));
  await waitFor(() => expect(groupCreate).toHaveBeenCalledTimes(2));
  expect(vi.mocked(groupCreate).mock.calls[1][0]).toEqual(input);
});

it("旧版来源草稿从已提交输入恢复引用，缺失正文时要求重新引用或明确取消", async () => {
  const input = { sourceSessionId: "old-session", sourceTeamId: "old-team", referenceText: "旧版引用正文" };
  const draft = { scope: "source:old-session:old-team", name: "旧团队", goal: "目标", directory: "/work", model: "default-model", selected: ["代码专家"], reference: true, requestKey: JSON.stringify(input) };
  useGroupStore.setState({ drafts: { "create:pending": JSON.stringify(draft) } });
  const view = mountWorkspace();
  await screen.findByRole("checkbox", { name: /代码专家/ });
  expect(screen.getByText("旧版引用正文")).toBeInTheDocument();
  view.unmount();
  useGroupStore.setState({ drafts: { "create:pending": JSON.stringify({ ...draft, requestKey: "" }) } });
  mountWorkspace();
  await screen.findByRole("checkbox", { name: /代码专家/ });
  fireEvent.click(screen.getByRole("button", { name: "创建并进入专家团" }));
  expect(groupCreate).not.toHaveBeenCalled();
  expect(screen.getByRole("alert")).toHaveTextContent("此前草稿未保存引用正文");
  fireEvent.click(screen.getByRole("checkbox", { name: "把下列内容共享给成员" }));
  fireEvent.click(screen.getByRole("button", { name: "创建并进入专家团" }));
  await waitFor(() => expect(groupCreate).toHaveBeenCalledOnce());
  expect(vi.mocked(groupCreate).mock.calls[0][0]).toMatchObject({ sourceSessionId: "old-session", sourceTeamId: "old-team" });
  expect(vi.mocked(groupCreate).mock.calls[0][0].referenceText).toBeUndefined();
});

it("来源团队省略可选说明和职责时，未提交草稿仍能完整恢复引用", () => {
  const draft = { scope: "source:s:t", name: "团队", goal: "", directory: "/work", model: "default-model", selected: ["代码专家"], reference: true,
    source: { sessionId: "s", team: { teamId: "t", members: ["代码专家"] }, referenceText: "不可丢失的来源正文" } };
  const saved = readGroupCreationDraft(JSON.stringify(draft));
  expect(saved?.source?.referenceText).toBe("不可丢失的来源正文");
  expect(saved?.source?.team).toMatchObject({ description: "", roles: {} });
});
