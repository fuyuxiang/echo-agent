import { describe, it, expect, vi, beforeEach, afterEach, type MockInstance } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { Cron } from "./Cron";
import { ConfirmProvider } from "../components/ConfirmDialog";
import * as api from "../lib/api";
import { useAuthStore } from "../stores/auth";

const AUTHORIZE_LABEL = "允许无人值守执行写入/命令类工具";

function job(overrides: Record<string, unknown> = {}) {
  return {
    id: "j1",
    name: "nightly",
    cron_expr: "0 9 * * *",
    enabled: true,
    status: "active",
    last_status: "",
    next_run_ms: null,
    config_valid: true,
    payload: { command: "echo hi", deliver_channel: "telegram", deliver_chat_id: "1" },
    authorization: null,
    authorization_valid: false,
    ...overrides,
  };
}

function renderCron() {
  return render(
    <MemoryRouter>
      <ConfirmProvider>
        <Cron />
      </ConfirmProvider>
    </MemoryRouter>,
  );
}

/** Fill the three fields a create needs; submit stays disabled without command. */
function fillForm() {
  fireEvent.change(screen.getByLabelText("任务名称"), { target: { value: "nightly" } });
  fireEvent.change(screen.getByLabelText("cron 表达式"), { target: { value: "0 9 * * *" } });
  fireEvent.change(screen.getByLabelText(/任务内容/), { target: { value: "echo hi" } });
}

// apiFetch 是泛型函数,ReturnType<typeof vi.spyOn> 会退化成 (...args: unknown[]) 的
// 宽签名,反而装不下它。这里直接用 apiFetch 自己的类型来标注 spy。
type ApiFetchSpy = MockInstance<typeof api.apiFetch>;

function postBody(spy: ApiFetchSpy) {
  const call = spy.mock.calls.find(
    ([, init]) => (init as RequestInit | undefined)?.method === "POST",
  );
  expect(call).toBeDefined();
  return JSON.parse((call![1] as RequestInit).body as string);
}

beforeEach(() => {
  // useWsSubscribe 只在有 token 时才连接;默认不设,避免测试里开真实 socket。
  useAuthStore.setState({ token: "" });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Cron 无人值守授权", () => {
  it("按任务显示已授权/需要重新授权/未授权三态", async () => {
    vi.spyOn(api, "apiFetch").mockResolvedValue({
      jobs: [
        job({
          id: "j1",
          name: "已授权任务",
          authorization: { operator: "alice", source: "cli", granted_at_ms: 1, summary: "echo hi" },
          authorization_valid: true,
        }),
        job({
          id: "j2",
          name: "改过内容的任务",
          authorization: { operator: "bob", source: "cli", granted_at_ms: 1, summary: "echo hi" },
          authorization_valid: false,
        }),
        job({ id: "j3", name: "从未授权的任务", authorization: null, authorization_valid: false }),
      ],
    } as never);

    renderCron();

    expect(await screen.findByText("已授权")).toBeInTheDocument();
    // 有授权记录但指纹已失效,必须与"从未授权"区分开:前者需要人重新确认,后者
    // 从来没有人确认过。
    expect(screen.getByText("需要重新授权")).toBeInTheDocument();
    expect(screen.getByText("未授权")).toBeInTheDocument();
    expect(screen.getByTitle(/由 alice 于 .* 授权/)).toBeInTheDocument();
  });

  it("不勾选授权时提交不带 authorize_unattended 字段", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockResolvedValue({ jobs: [] } as never);
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: /新建/ }));
    fillForm();
    expect(screen.getByLabelText(AUTHORIZE_LABEL)).not.toBeChecked();
    fireEvent.click(screen.getByRole("button", { name: "创建" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron", expect.anything()));
    // 缺失即非同意:不发这个字段,而不是发 false,让协议层面看得见这一点。
    expect(postBody(spy)).not.toHaveProperty("authorize_unattended");
  });

  it("勾选授权先弹危险确认,确认后才发 authorize_unattended", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockResolvedValue({ jobs: [] } as never);
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: /新建/ }));
    fillForm();
    fireEvent.change(screen.getByLabelText(/投递渠道/), { target: { value: "telegram" } });
    fireEvent.change(screen.getByLabelText(/会话\/群 ID/), { target: { value: "1" } });
    fireEvent.click(screen.getByLabelText(AUTHORIZE_LABEL));

    // 确认框要让人看清自己在授权什么:指令全文、频率、投递目标都得在场。
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("echo hi");
    expect(dialog).toHaveTextContent("0 9 * * *");
    expect(dialog).toHaveTextContent("telegram:1");

    fireEvent.click(screen.getByRole("button", { name: "我已确认，授权" }));
    await waitFor(() => expect(screen.getByLabelText(AUTHORIZE_LABEL)).toBeChecked());
    fireEvent.click(screen.getByRole("button", { name: "创建" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron", expect.anything()));
    expect(postBody(spy).authorize_unattended).toBe(true);
  });

  it("拒绝确认则开关保持关闭,提交仍不带授权", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockResolvedValue({ jobs: [] } as never);
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: /新建/ }));
    fillForm();
    fireEvent.click(screen.getByLabelText(AUTHORIZE_LABEL));
    // 表单自己也有一个"取消",按钮要从对话框里取,否则会连带点到关表单的那个。
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(screen.getByLabelText(AUTHORIZE_LABEL)).not.toBeChecked();

    fireEvent.click(screen.getByRole("button", { name: "创建" }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron", expect.anything()));
    expect(postBody(spy)).not.toHaveProperty("authorize_unattended");
  });

  it("编辑已授权任务时开关重新回到关闭,一次改名不会静默续授权", async () => {
    const granted = job({
      name: "每日汇报",
      authorization: { operator: "alice", source: "cli", granted_at_ms: 1, summary: "echo hi" },
      authorization_valid: true,
    });
    const spy = vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/cron") return { jobs: [granted] } as never;
      return {} as never;
    });
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: "编辑定时任务 每日汇报" }));
    // 授权是每次提交单独表达的意图,不是表单的持久状态。
    expect(screen.getByLabelText(AUTHORIZE_LABEL)).not.toBeChecked();

    fireEvent.change(screen.getByLabelText("任务名称"), { target: { value: "改个名" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron/j1", expect.anything()));
    const [, init] = spy.mock.calls.find(([p]) => p === "/cron/j1")!;
    const sent = JSON.parse((init as { body: string }).body);
    expect(sent).not.toHaveProperty("authorize_unattended");
  });
});
