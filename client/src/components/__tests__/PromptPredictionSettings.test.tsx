import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PromptPredictionSettings } from "../PromptPredictionSettings";
import { PROMPT_PREDICTION_KEY, usePromptPredictionStore } from "@/stores/prompt-prediction-store";
import { readDurable } from "@/lib/durable-ui-state";
import { searchSettings } from "@/lib/settings-search";

vi.mock("@/lib/agent-client", async original => ({ ...await original<object>(),
  providersList: vi.fn(async () => ({ providers: [], models: [{ modelId: "small", name: "轻量模型", providerId: "p" }] })),
}));
beforeEach(() => { localStorage.clear(); usePromptPredictionStore.setState({ enabled: true, model: "" }); });
afterEach(cleanup);

it("persists the switch and selected model, restoring the preference on reload", async () => {
  render(<PromptPredictionSettings />);
  fireEvent.focus(screen.getByLabelText("预测模型"));
  const option = await screen.findByRole("option", { name: "轻量模型" });
  expect(option).toHaveValue("small");
  fireEvent.change(screen.getByLabelText("预测模型"), { target: { value: "small" } });
  fireEvent.click(screen.getByRole("switch", { name: "显示输入建议" }));
  expect(usePromptPredictionStore.getState()).toMatchObject({ enabled: false, model: "small" });
  expect(screen.getByLabelText("预测模型")).toBeDisabled();
  expect(readDurable(PROMPT_PREDICTION_KEY, { enabled: true, model: "" }, (value): value is { enabled: boolean; model: string } => !!value)).toEqual({ enabled: false, model: "small" });
  expect(searchSettings("预测")).toEqual(expect.arrayContaining([expect.objectContaining({ target: "composer-predictions" })]));
});

it("retains the active preference and explains a persistence failure", async () => {
  render(<PromptPredictionSettings />);
  fireEvent.focus(screen.getByLabelText("预测模型"));
  await screen.findByRole("option", { name: "轻量模型" });
  const storage = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("quota"); });
  try {
    fireEvent.click(screen.getByRole("switch", { name: "显示输入建议" }));
    expect(screen.getByRole("switch")).toBeChecked();
    expect(screen.getByRole("alert")).toHaveTextContent("预测设置保存失败");
  } finally { storage.mockRestore(); }
});

it("applies a preference changed in another desktop window", () => {
  render(<PromptPredictionSettings />);
  localStorage.setItem(PROMPT_PREDICTION_KEY, JSON.stringify({ version: 1, data: { enabled: false, model: "shared-small" } }));
  act(() => window.dispatchEvent(new StorageEvent("storage", { key: PROMPT_PREDICTION_KEY })));
  expect(screen.getByRole("switch")).not.toBeChecked();
  expect(screen.getByLabelText("预测模型")).toHaveValue("shared-small");
});
