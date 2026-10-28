import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { Skills } from "./Skills";
import { ConfirmProvider } from "../components/ConfirmDialog";
import * as api from "../lib/api";
import { useCapabilitiesStore } from "../stores/capabilities";

const SKILLS = {
  skills: [
    { name: "web search", description: "搜索网页", enabled: true },
    { name: "shell", description: "", enabled: false },
  ],
};

function mockApi(admin: boolean, overrides: Record<string, unknown> = {}) {
  return vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
    if (path === "/capabilities") return { admin } as never;
    if (path === "/skills") return SKILLS as never;
    if (path in overrides) return overrides[path] as never;
    return {} as never;
  });
}

function renderSkills() {
  return render(
    <ConfirmProvider>
      <Skills />
    </ConfirmProvider>,
  );
}

beforeEach(() => {
  useCapabilitiesStore.getState().reset();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Skills 页", () => {
  it("渲染技能列表,无描述时显示占位文案", async () => {
    mockApi(true);
    renderSkills();

    await waitFor(() => expect(screen.getByText("web search")).toBeInTheDocument());
    expect(screen.getByText("搜索网页")).toBeInTheDocument();
    expect(screen.getByText("无描述")).toBeInTheDocument();
  });

  it("开关反映 enabled,点击后调用 toggle 接口", async () => {
    const spy = mockApi(true);
    renderSkills();

    const toggle = await screen.findByRole("switch", { name: "停用技能 web search" });
    expect(toggle).toHaveAttribute("aria-checked", "true");

    fireEvent.click(toggle);
    // 名称含空格,URL 必须编码,否则请求路径非法。
    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/skills/web%20search/toggle", { method: "POST" }),
    );
  });

  it("删除需要二次确认,取消后不发请求", async () => {
    const spy = mockApi(true);
    renderSkills();

    fireEvent.click(await screen.findByRole("button", { name: "删除技能 shell" }));
    // 确认框必须说明是不可撤销的磁盘删除,而不是一句“确定吗”。
    expect(await screen.findByText(/不可撤销/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    await waitFor(() =>
      expect(spy).not.toHaveBeenCalledWith("/skills/shell", { method: "DELETE" }),
    );
  });

  it("确认后按编码路径发 DELETE", async () => {
    const spy = mockApi(true);
    renderSkills();

    fireEvent.click(await screen.findByRole("button", { name: "删除技能 web search" }));
    fireEvent.click(await screen.findByRole("button", { name: "删除" }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/skills/web%20search", { method: "DELETE" }),
    );
  });

  it("非 admin 令牌下禁用导入、删除与开关,但保留查看", async () => {
    mockApi(false);
    renderSkills();

    await waitFor(() => expect(screen.getByText("web search")).toBeInTheDocument());
    expect(screen.getByRole("button", { name: /导入技能/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "删除技能 shell" })).toBeDisabled();
    // 开关会改变 agent 下一轮能用哪些技能，与安装/删除同一类变更，
    // 已一并收口到 _admin_guard，因此只读令牌下必须禁用。
    expect(screen.getByRole("switch", { name: "停用技能 web search" })).toBeDisabled();
    // 列表与详情仍是只读权限可见的。
    expect(screen.getByText("web search")).toBeInTheDocument();
    expect(screen.getAllByText(/需要管理员令牌/).length).toBeGreaterThan(0);
  });

  it("导入表单提交 path 并刷新列表", async () => {
    const spy = mockApi(true);
    renderSkills();

    fireEvent.click(await screen.findByRole("button", { name: /导入技能/ }));
    fireEvent.change(screen.getByLabelText("技能目录路径"), {
      target: { value: "  /srv/skills/foo  " },
    });
    fireEvent.click(screen.getByRole("button", { name: "导入" }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/skills/import", {
        method: "POST",
        body: JSON.stringify({ path: "/srv/skills/foo" }),
      }),
    );
  });

  it("路径为空时导入按钮不可点", async () => {
    mockApi(true);
    renderSkills();

    fireEvent.click(await screen.findByRole("button", { name: /导入技能/ }));
    expect(screen.getByRole("button", { name: "导入" })).toBeDisabled();
  });

  it("点详情按钮打开抽屉并请求详情与依赖", async () => {
    const spy = mockApi(true, {
      "/skills/shell": { name: "shell", content: "# Shell", files: ["SKILL.md"] },
      "/skills/shell/deps": { requires: [], missing: [], satisfied: true },
    });
    renderSkills();

    fireEvent.click(await screen.findByRole("button", { name: "查看技能 shell 详情" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/skills/shell", expect.anything()));
    expect(spy).toHaveBeenCalledWith("/skills/shell/deps", expect.anything());
    expect(await screen.findByText("# Shell")).toBeInTheDocument();
  });
});
