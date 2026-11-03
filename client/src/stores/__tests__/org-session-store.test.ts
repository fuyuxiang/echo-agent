import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ orgSession: vi.fn() }));

vi.mock("@/lib/org-client", () => ({ orgSession: mocks.orgSession }));

import {
  organizationKnowledgeAvailability,
  resetOrgSessionMirror,
  useOrgSessionStore,
} from "../org-session-store";

describe("org-session-store", () => {
  beforeEach(() => {
    resetOrgSessionMirror();
    mocks.orgSession.mockReset();
  });

  it("只执行一次并发恢复并保存组织用户", async () => {
    mocks.orgSession.mockResolvedValue({
      loggedIn: true,
      serverUrl: "https://memory.example.com",
      user: {
        id: "u1",
        username: "alice",
        displayName: "Alice",
        role: "member",
        clearance: 1,
      },
    });

    const [first, second] = await Promise.all([
      useOrgSessionStore.getState().hydrate(),
      useOrgSessionStore.getState().hydrate(),
    ]);

    expect(first?.user?.displayName).toBe("Alice");
    expect(second?.user?.displayName).toBe("Alice");
    expect(mocks.orgSession).toHaveBeenCalledTimes(1);
    expect(useOrgSessionStore.getState().hydrated).toBe(true);
  });

  it("组织服务不可达不阻塞本地 Shell", async () => {
    mocks.orgSession.mockRejectedValue(new Error("offline"));

    await expect(useOrgSessionStore.getState().hydrate()).resolves.toBeNull();
    expect(useOrgSessionStore.getState().session).toBeNull();
    expect(useOrgSessionStore.getState().hydrated).toBe(true);
  });

  it("退出后忽略启动恢复的迟到账号", async () => {
    let resolve!: (session: { loggedIn: boolean; serverUrl: string }) => void;
    mocks.orgSession.mockReturnValue(new Promise((done) => { resolve = done; }));
    const pending = useOrgSessionStore.getState().hydrate();
    useOrgSessionStore.getState().clearSession();
    resolve({ loggedIn: true, serverUrl: "https://old.example.com" });

    await pending;
    expect(useOrgSessionStore.getState().session?.loggedIn).toBe(false);
  });

  it("切换账号后忽略启动恢复的迟到错误", async () => {
    let reject!: (error: Error) => void;
    mocks.orgSession.mockReturnValue(new Promise((_, fail) => { reject = fail; }));
    const pending = useOrgSessionStore.getState().hydrate();
    const next = { loggedIn: true, serverUrl: "https://new.example.com" };
    useOrgSessionStore.getState().setSession(next);
    reject(new Error("previous server offline"));

    await expect(pending).resolves.toEqual(next);
    expect(useOrgSessionStore.getState().session).toEqual(next);
  });

  it("权限范围变化推进边界代次，同一范围健康刷新保留代次", () => {
    const session = {
      loggedIn: true, serverUrl: "https://memory.example.com",
      bootstrap: {
        apiVersion: 1,
        user: { id: "u1", username: "alice", displayName: "Alice", role: "member" as const, clearance: 1 },
        scopes: [{ id: "team-1", kind: "team" as const, name: "研发团队" }],
        policy: {}, serverTime: 1,
      },
    };
    useOrgSessionStore.getState().setSession(session);
    const revision = useOrgSessionStore.getState().boundaryRevision;
    useOrgSessionStore.getState().setSession({ ...session, organizationMemoryEnabled: false });
    expect(useOrgSessionStore.getState().boundaryRevision).toBe(revision);
    useOrgSessionStore.getState().setSession({ ...session, bootstrap: { ...session.bootstrap, scopes: [] } });
    expect(useOrgSessionStore.getState().boundaryRevision).toBe(revision + 1);
  });

  it("策略续期及字段顺序不改变权限边界，实际限制变化仍隔离旧数据", () => {
    const session = {
      loggedIn: true, serverUrl: "https://memory.example.com",
      bootstrap: {
        apiVersion: 1,
        user: { id: "u1", username: "alice", displayName: "Alice", role: "member" as const, clearance: 1 },
        scopes: [{ id: "team-1", kind: "team" as const, name: "团队" }],
        policy: { version: 1, issuedAt: 10, expiresAt: 20, allowSkillSubmission: true, limits: { a: 1, b: 2 } },
        serverTime: 10,
      },
    };
    useOrgSessionStore.getState().setSession(session);
    const revision = useOrgSessionStore.getState().boundaryRevision;
    useOrgSessionStore.getState().setSession({ ...session, bootstrap: { ...session.bootstrap,
      policy: { limits: { b: 2, a: 1 }, allowSkillSubmission: true, expiresAt: 40, issuedAt: 30, version: 1 },
    } });
    expect(useOrgSessionStore.getState().boundaryRevision).toBe(revision);
    useOrgSessionStore.getState().setSession({ ...session, bootstrap: { ...session.bootstrap,
      policy: { ...session.bootstrap.policy, allowSkillSubmission: false },
    } });
    expect(useOrgSessionStore.getState().boundaryRevision).toBe(revision + 1);
  });

  it("统一判定组织知识能力并绑定服务器与账号身份", () => {
    useOrgSessionStore.setState({
      hydrated: true,
      session: {
        loggedIn: true,
        organizationMemoryEnabled: true,
        serverUrl: "https://memory.example.com",
        username: "alice",
        user: { id: "u1", username: "alice", displayName: "Alice", role: "member", clearance: 1 },
        bootstrap: {
          apiVersion: 1,
          user: { id: "u1", username: "alice", displayName: "Alice", role: "member", clearance: 1 },
          scopes: [{ id: "team-1", kind: "team", name: "研发团队" }],
          policy: {},
          serverTime: 1,
        },
      },
    });

    expect(organizationKnowledgeAvailability()).toEqual({
      available: true,
      reason: "使用组织账号中你有权限的知识",
      identity: JSON.stringify(["https://memory.example.com", "u1"]),
    });
    useOrgSessionStore.getState().clearSession();
    expect(organizationKnowledgeAvailability()).toMatchObject({
      available: false,
      reason: "登录组织后可用",
      identity: "signed-out",
    });
    expect(useOrgSessionStore.getState().session).toMatchObject({
      loggedIn: false,
      serverUrl: "https://memory.example.com",
      username: "alice",
      requiresReauthentication: false,
    });
  });
});
