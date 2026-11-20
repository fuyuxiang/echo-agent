import { describe, it, expect, beforeEach } from "vitest";
import {
  todayKey,
  monthKey,
  estimateCost,
  recordUsage,
  recordCumulativeUsage,
  recordTurnUsage,
  summarizeUsage,
  checkQuota,
  consumeQuotaAlert,
  isQuotaBlocking,
  loadQuotaConfig,
  saveQuotaConfig,
  clearUsage,
  serializeUsageCsv,
  usageRecordCost,
  type UsageRecord,
  type QuotaConfig,
} from "../usage-quota";

const rates = { "gpt-4": { prompt: 0.03, completion: 0.06 } };
const config: QuotaConfig = { period: "daily", tokenLimit: 10000, rates };

describe("estimateCost", () => {
  it("按费率表计算", () => {
    expect(estimateCost("gpt-4", 1000, 500, rates)).toBeCloseTo(0.03 + 0.03, 5);
  });
  it("无费率 → undefined", () => {
    expect(estimateCost("unknown", 100, 50, rates)).toBeUndefined();
    expect(estimateCost("gpt-4", 100, 50)).toBeUndefined();
  });
});

describe("recordUsage", () => {
  beforeEach(() => {
    window.localStorage.removeItem("echoagent.usage");
    window.localStorage.removeItem("echoagent.quota");
    window.localStorage.removeItem("echoagent.usage-snapshots.v1");
  });

  it("追加记录并持久化", () => {
    const records = recordUsage([], { modelId: "gpt-4", promptTokens: 100, completionTokens: 50 }, config);
    expect(records).toHaveLength(1);
    expect(records[0].modelId).toBe("gpt-4");
    expect(records[0].promptTokens).toBe(100);
    expect(records[0].cost).toBeDefined();
    // localStorage 持久化。
    const raw = window.localStorage.getItem("echoagent.usage");
    expect(raw).not.toBeNull();
    expect(JSON.parse(raw!)).toHaveLength(1);
  });

  it("无 config → cost 为 undefined", () => {
    const records = recordUsage([], { modelId: "x", promptTokens: 10, completionTokens: 5 });
    expect(records[0].cost).toBeUndefined();
  });
});

describe("recordCumulativeUsage", () => {
  beforeEach(() => {
    window.localStorage.removeItem("echoagent.usage");
    window.localStorage.removeItem("echoagent.usage-snapshots.v1");
  });

  it("对会话累计值只记录每轮增量", () => {
    const first = recordCumulativeUsage([], {
      sessionId: "s1", modelId: "gpt-4", inputTokens: 100, outputTokens: 20,
    });
    const second = recordCumulativeUsage(first, {
      sessionId: "s1", modelId: "gpt-4", inputTokens: 160, outputTokens: 35,
    });
    expect(second).toHaveLength(2);
    expect(second[1]).toMatchObject({
      sessionId: "s1", modelId: "gpt-4", promptTokens: 60, completionTokens: 15,
    });
  });

  it("同一累计快照重复上报时不重复记账", () => {
    const first = recordCumulativeUsage([], {
      sessionId: "s1", modelId: "gpt-4", inputTokens: 100, outputTokens: 20,
    });
    const second = recordCumulativeUsage(first, {
      sessionId: "s1", modelId: "gpt-4", inputTokens: 100, outputTokens: 20,
    });
    expect(second).toHaveLength(1);
  });
});

describe("recordTurnUsage", () => {
  beforeEach(() => clearUsage());

  it("按 session + prompt 幂等记录持久化轮次用量", () => {
    const entry = {
      sessionId: "s1",
      promptId: "p1",
      occurredAt: new Date(2026, 7, 12, 9, 30).getTime(),
      usage: {
        inputTokens: 800,
        outputTokens: 200,
        totalTokens: 1000,
        cachedReadTokens: 300,
        reasoningTokens: 50,
        modelCalls: 3,
        numTurns: 2,
        modelUsage: {
          "gpt-4": {
            inputTokens: 800,
            outputTokens: 200,
            totalTokens: 1000,
            cachedReadTokens: 300,
            modelCalls: 3,
          },
        },
      },
    };
    const first = recordTurnUsage([], { ...entry, projectId: "project-a", turnOrigin: "scheduled", fallbackModelId: "current-model" });
    const replay = recordTurnUsage(first, { ...entry, occurredAt: undefined });
    expect(replay).toHaveLength(1);
    expect(replay[0]).toMatchObject({
      id: "s1:p1",
      date: "2026-08-12",
      promptTokens: 800,
      completionTokens: 200,
      cachedReadTokens: 300,
      reasoningTokens: 50,
      modelCalls: 3,
      agentTurns: 2,
      source: "turn",
      projectId: "project-a",
      modelId: "gpt-4",
      turnOrigin: "scheduled",
    });
    expect(summarizeUsage(replay).byProject["project-a"]).toMatchObject({ tokens: 1000, count: 3, requests: 1 });
  });

  it("优先使用可信的运行时成本，不完整账本改用本地费率", () => {
    const base = {
      sessionId: "s1",
      promptId: "p1",
      usage: {
        inputTokens: 1000,
        outputTokens: 500,
        totalTokens: 1500,
        cachedReadTokens: 0,
        modelCalls: 1,
        costUsdTicks: 500_000_000,
      },
    };
    const trusted = recordTurnUsage([], base, config)[0];
    expect(usageRecordCost(trusted, config)).toBeCloseTo(0.05);

    const incomplete = recordTurnUsage([], {
      ...base,
      promptId: "p2",
      fallbackModelId: "gpt-4",
      usage: { ...base.usage, usageIsIncomplete: true },
    }, config)[0];
    expect(incomplete.providerCost).toBeUndefined();
    expect(usageRecordCost(incomplete, config)).toBeCloseTo(0.06);
  });
});

describe("summarizeUsage", () => {
  const records: UsageRecord[] = [
    { date: "2026-07-30", modelId: "gpt-4", promptTokens: 100, completionTokens: 50, cost: 0.06, ts: 1 },
    { date: "2026-07-30", modelId: "gpt-4", promptTokens: 200, completionTokens: 100, cost: 0.12, ts: 2 },
    { date: "2026-07-29", modelId: "claude", promptTokens: 50, completionTokens: 30, ts: 3 },
  ];

  it("全量汇总", () => {
    const s = summarizeUsage(records);
    expect(s.totalTokens).toBe(530); // 150+300+80
    expect(s.totalPrompt).toBe(350);
    expect(s.totalCompletion).toBe(180);
    expect(s.totalCost).toBeCloseTo(0.18, 5);
    expect(s.count).toBe(3);
    expect(s.byModel["gpt-4"].tokens).toBe(450);
    expect(s.byModel["gpt-4"].count).toBe(2);
    expect(s.byDate["2026-07-30"].tokens).toBe(450);
  });

  it("按日期范围过滤", () => {
    const s = summarizeUsage(records, { from: "2026-07-30", to: "2026-07-30" });
    expect(s.count).toBe(2);
    expect(s.totalTokens).toBe(450);
  });
});

describe("checkQuota", () => {
  it("未超限", () => {
    const records: UsageRecord[] = [
      { date: todayKey(), modelId: "x", promptTokens: 1000, completionTokens: 500, ts: 1 },
    ];
    const q = checkQuota(records, { period: "daily", tokenLimit: 10000 });
    expect(q.used).toBe(1500);
    expect(q.pct).toBe(15);
    expect(q.exceeded).toBe(false);
    expect(q.nearLimit).toBe(false);
  });
  it("接近上限(≥80%)", () => {
    const records: UsageRecord[] = [
      { date: todayKey(), modelId: "x", promptTokens: 4000, completionTokens: 4000, ts: 1 },
    ];
    const q = checkQuota(records, { period: "daily", tokenLimit: 10000 });
    expect(q.pct).toBe(80);
    expect(q.nearLimit).toBe(true);
  });
  it("超限", () => {
    const records: UsageRecord[] = [
      { date: todayKey(), modelId: "x", promptTokens: 6000, completionTokens: 5000, ts: 1 },
    ];
    const q = checkQuota(records, { period: "daily", tokenLimit: 10000 });
    expect(q.exceeded).toBe(true);
  });
  it("monthly 周期", () => {
    const records: UsageRecord[] = [
      { date: monthKey() + "-01", modelId: "x", promptTokens: 100, completionTokens: 50, ts: 1 },
      { date: monthKey() + "-15", modelId: "x", promptTokens: 200, completionTokens: 100, ts: 2 },
    ];
    const q = checkQuota(records, { period: "monthly", tokenLimit: 1000 });
    expect(q.used).toBe(450);
  });
  it("0 / 空白上限表示不限，不会被判定超限", () => {
    const records: UsageRecord[] = [
      { date: todayKey(), modelId: "x", promptTokens: 1, completionTokens: 1, ts: 1 },
    ];
    expect(checkQuota(records, { period: "daily", tokenLimit: 0 })).toMatchObject({
      pct: 0, exceeded: false, nearLimit: false,
    });
  });
  it("只有 block 策略在超限后暂停发送", () => {
    const records: UsageRecord[] = [
      { date: todayKey(), modelId: "x", promptTokens: 100, completionTokens: 1, ts: 1 },
    ];
    expect(isQuotaBlocking(records, { period: "daily", tokenLimit: 100, enforcement: "warn" })).toBe(false);
    expect(isQuotaBlocking(records, { period: "daily", tokenLimit: 100, enforcement: "block" })).toBe(true);
  });
  it("接近与超限阈值各通知一次", () => {
    window.localStorage.removeItem("echoagent.quota-alert.v1");
    const near = [{ date: todayKey(), modelId: "x", promptTokens: 80, completionTokens: 0, ts: 1 }];
    const exceeded = [{ ...near[0], promptTokens: 101 }];
    const cfg = { period: "daily", tokenLimit: 100 } as const;
    expect(consumeQuotaAlert(near, cfg)).toBe("near");
    expect(consumeQuotaAlert(near, cfg)).toBeNull();
    expect(consumeQuotaAlert(exceeded, cfg)).toBe("exceeded");
    expect(consumeQuotaAlert(exceeded, cfg)).toBeNull();
  });
});

describe("本地时区日期键", () => {
  it("使用本地日历日而不是 UTC 日历日", () => {
    const local = new Date(2026, 8, 1, 0, 30, 0);
    expect(todayKey(local)).toBe("2026-09-01");
    expect(monthKey(local)).toBe("2026-09");
  });
});

describe("quota config 持久化", () => {
  beforeEach(() => {
    window.localStorage.removeItem("echoagent.quota");
  });

  it("save/load", () => {
    saveQuotaConfig(config);
    const loaded = loadQuotaConfig();
    expect(loaded?.period).toBe("daily");
    expect(loaded?.tokenLimit).toBe(10000);
  });
  it("未配置 → null", () => {
    expect(loadQuotaConfig()).toBeNull();
  });
});

describe("clearUsage", () => {
  it("清空", () => {
    recordUsage([], { modelId: "x", promptTokens: 1, completionTokens: 1 });
    clearUsage();
    const raw = window.localStorage.getItem("echoagent.usage");
    expect(JSON.parse(raw!)).toEqual([]);
  });
});

describe("用量导出", () => {
  it("CSV 包含审计字段与数据", () => {
    const records = recordUsage([], {
      modelId: "gpt-4",
      sessionId: "s1",
      promptId: "p1",
      promptTokens: 100,
      completionTokens: 20,
    });
    const csv = serializeUsageCsv(records, config);
    expect(csv).toContain("session_id,prompt_id,model,input_tokens");
    expect(csv).toContain("s1,p1,gpt-4,100,20,120");
  });
});
