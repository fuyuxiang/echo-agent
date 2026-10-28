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

  it("re-focuses the safe action for a second destructive dialog", async () => {
    // Two destructive dialogs in a row share the focus effect's dependencies
    // ([onSettle, options.destructive]), so React re-rendered the same instance
    // and the effect never re-ran: the second dialog kept whatever focus the
    // first left behind. Deleting one job, then another, is exactly the sequence
    // where the destructive default matters most.
    render(
      <ConfirmProvider>
        <Trigger destructive />
      </ConfirmProvider>,
    );

    fireEvent.click(screen.getByText("open"));
    const cancel = await screen.findByRole("button", { name: /cancel|取消/i });
    await waitFor(() => expect(cancel).toHaveFocus());

    // Move focus onto the dangerous action, as a Tab would.
    const confirmButton = screen.getByRole("button", { name: /confirm|确定/i });
    confirmButton.focus();
    expect(confirmButton).toHaveFocus();

    // A second request replaces the first (which resolves as cancelled).
    fireEvent.click(screen.getByText("open"));

    await waitFor(() => {
      expect(screen.getByRole("button", { name: /cancel|取消/i })).toHaveFocus();
    });
  });

  it("focuses confirm when a non-destructive dialog follows a destructive one", async () => {
    // The mirror case: remounting must not make every dialog focus cancel.
    function TwoTriggers() {
      const confirm = useConfirm();
      return (
        <>
          <button onClick={() => confirm({ title: "T", message: "M", destructive: true })}>
            danger
          </button>
          <button onClick={() => confirm({ title: "T", message: "M", destructive: false })}>
            safe
          </button>
        </>
      );
    }
    render(
      <ConfirmProvider>
        <TwoTriggers />
      </ConfirmProvider>,
    );

    fireEvent.click(screen.getByText("danger"));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /cancel|取消/i })).toHaveFocus(),
    );

    fireEvent.click(screen.getByText("safe"));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /confirm|确定/i })).toHaveFocus(),
    );
  });
});
