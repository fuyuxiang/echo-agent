import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ConfirmProvider, useConfirm } from "./ConfirmDialog";

function Trigger({ destructive }: { destructive: boolean }) {
  const confirm = useConfirm();
  return (
    <button onClick={() => confirm({ title: "T", message: "M", destructive })}>open</button>
  );
}

describe("ConfirmDialog initial focus", () => {
  it("focuses cancel for destructive actions", async () => {
    // A destructive dialog that pre-focuses its confirm button turns a stray
    // Enter or Space into the destructive action itself.
    render(
      <ConfirmProvider>
        <Trigger destructive />
      </ConfirmProvider>,
    );
    fireEvent.click(screen.getByText("open"));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: /cancel|取消/i })).toHaveFocus();
    });
  });

  it("focuses confirm for non-destructive actions", async () => {
    render(
      <ConfirmProvider>
        <Trigger destructive={false} />
      </ConfirmProvider>,
    );
    fireEvent.click(screen.getByText("open"));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: /confirm|确定/i })).toHaveFocus();
    });
  });

  it("still closes on Escape", async () => {
    render(
      <ConfirmProvider>
        <Trigger destructive />
      </ConfirmProvider>,
    );
    fireEvent.click(screen.getByText("open"));
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
  });
});
