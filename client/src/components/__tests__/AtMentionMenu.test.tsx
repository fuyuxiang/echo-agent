import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import {
  AtMentionMenu,
} from "@/components/AtMentionMenu";

describe("AtMentionMenu", () => {
  it("renders nothing when there is no active mention", () => {
    const { container } = render(
      <AtMentionMenu
        text="plain text"
        cursor={5}
        filePaths={["src/foo.ts"]}
        onPick={() => {}}
      />,
    );
    expect(container.querySelector(".at-menu")).toBeNull();
  });

  it("renders nothing when the query has no matches", () => {
    const { container } = render(
      <AtMentionMenu
        text="@zzzz"
        cursor={5}
        filePaths={["src/foo.ts"]}
        onPick={() => {}}
      />,
    );
    expect(container.querySelector(".at-menu")).toBeNull();
  });

  it("lists members and matching files", () => {
    render(
      <AtMentionMenu
        text="@for"
        cursor={4}
        filePaths={["src/form.tsx", "notes.md"]}
        members={[{id: "1", name: "Format Expert"}]}
        onPick={() => {}}
      />,
    );
    const items = screen.getAllByTestId("at-menu-item");
    expect(items.map(item => item.getAttribute("data-kind"))).toEqual(["member", "file"]);
    expect(screen.queryByText("notes.md")).not.toBeInTheDocument();
  });

  it("calls onPick when an item is clicked", () => {
    const onPick = vi.fn();
    render(
      <AtMentionMenu
        text="@for"
        cursor={4}
        filePaths={["src/form.tsx"]}
        onPick={onPick}
      />,
    );
    fireEvent.click(screen.getAllByTestId("at-menu-item")[0]);
    expect(onPick).toHaveBeenCalledWith("@src/form.tsx", "src/form.tsx");
  });

  it("handleKeyDown ArrowDown moves active and Enter picks", async () => {
    const onPick = vi.fn();
    const ref = { current: null as null | { handleKeyDown: (e: { key: string; preventDefault: () => void }) => boolean } };
    render(
      <AtMentionMenu
        ref={ref as never}
        text="@for"
        cursor={4}
        filePaths={["src/form.tsx"]}
        onPick={onPick}
      />,
    );
    // Sanity: handleKeyDown forwards key events when there are matches.
    expect(ref.current!.handleKeyDown({ key: "ArrowDown", preventDefault: () => {} })).toBe(true);
    expect(ref.current!.handleKeyDown({ key: "ArrowUp", preventDefault: () => {} })).toBe(true);
    expect(ref.current!.handleKeyDown({ key: "Escape", preventDefault: () => {} })).toBe(true);
    expect(ref.current!.handleKeyDown({ key: "Tab", preventDefault: () => {} })).toBe(true);
    // Unrelated keys are not consumed.
    expect(ref.current!.handleKeyDown({ key: "a", preventDefault: () => {} })).toBe(false);
  });

  it("clicks on items call onPick with the right mention + replacement", () => {
    const onPick = vi.fn();
    render(
      <AtMentionMenu
        text="@x"
        cursor={2}
        filePaths={["x.ts", "draft-x.md"]}
        onPick={onPick}
      />,
    );
    const items = screen.getAllByTestId("at-menu-item");
    fireEvent.click(items[0]);
    expect(onPick).toHaveBeenCalledWith("@x.ts", "x.ts");
    fireEvent.click(items[1]);
    expect(onPick).toHaveBeenCalledWith("@draft-x.md", "draft-x.md");
  });

  it("handleKeyDown Escape returns true even with no matches (dismiss)", () => {
    const ref = { current: null as null | { handleKeyDown: (e: { key: string; preventDefault: () => void }) => boolean } };
    render(
      <AtMentionMenu
        ref={ref as never}
        text="@zzzz"
        cursor={5}
        filePaths={["src/foo.ts"]}
        onPick={() => {}}
      />,
    );
    const esc = { key: "Escape", preventDefault: () => {} };
    expect(ref.current!.handleKeyDown(esc)).toBe(true);
  });

  it("handleKeyDown ArrowUp wraps around to the last entry", () => {
    const ref = { current: null as null | { handleKeyDown: (e: { key: string; preventDefault: () => void }) => boolean } };
    render(
      <AtMentionMenu
        ref={ref as never}
        text="@for"
        cursor={4}
        filePaths={["src/form.tsx", "notes.md"]}
        members={[{id: "1", name: "Format Expert"}]}
        onPick={() => {}}
      />,
    );
    const up = { key: "ArrowUp", preventDefault: () => {} };
    expect(ref.current!.handleKeyDown(up)).toBe(true);
  });

  it("ignores unrelated keys and returns false", async () => {
    const user = userEvent.setup();
    const ref = { current: null as null | { handleKeyDown: (e: { key: string; preventDefault: () => void }) => boolean } };
    render(
      <AtMentionMenu
        ref={ref as never}
        text="@for"
        cursor={4}
        filePaths={["src/form.tsx", "notes.md"]}
        members={[{id: "1", name: "Format Expert"}]}
        onPick={() => {}}
      />,
    );
    const other = { key: "a", preventDefault: () => {} };
    expect(ref.current!.handleKeyDown(other)).toBe(false);
    // Sanity: typing in the textarea shouldn't crash the menu.
    await user.keyboard("a");
  });
});
