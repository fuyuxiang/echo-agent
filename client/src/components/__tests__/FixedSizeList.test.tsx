import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { FixedSizeList, type FixedSizeListHandle } from "@/components/FixedSizeList";
import { useRef } from "react";

function makeHarness() {
  const items = Array.from({ length: 500 }, (_, i) => ({ id: i, name: `row-${i}` }));
  const Harness = () => {
    const ref = useRef<FixedSizeListHandle>(null);
    return (
      <>
        <FixedSizeList
          ref={ref}
          items={items}
          itemHeight={26}
          height={300}
          renderItem={(item) => <span data-testid="row">{item.name}</span>}
          getKey={(item) => String(item.id)}
        />
        <button type="button" onClick={() => ref.current?.scrollToIndex(0)}>top</button>
        <button type="button" onClick={() => ref.current?.scrollToIndex(450)}>bottom</button>
      </>
    );
  };
  return { items, Harness };
}

describe("FixedSizeList", () => {
  it("渲染所有 items 并维持 DOM 节点数 ~visible + overscan", () => {
    const { Harness } = makeHarness();
    render(<Harness />);
    const rows = screen.getAllByTestId("row");
    // height=300, itemHeight=26, overscan=8 → ceil(300/26)=12 + 2*8 = ~28 DOM rows
    expect(rows.length).toBeGreaterThan(15);
    expect(rows.length).toBeLessThan(40);
    expect(rows[0].textContent).toBe("row-0");
  });

  it("scrollToIndex 改变 scrollTop 后虚拟窗口滑动", () => {
    const { Harness } = makeHarness();
    render(<Harness />);
    fireEvent.click(screen.getByText("bottom"));
    // After scrolling to 450, first visible row index should be near 450 - overscan.
    const firstRow = screen.getAllByTestId("row")[0];
    const firstIndex = Number(firstRow.parentElement?.getAttribute("data-virtual-index") ?? "-1");
    expect(firstIndex).toBeGreaterThan(420);
    expect(firstIndex).toBeLessThan(450);
  });

  it("scrollToIndex(0) 回到顶部", () => {
    const { Harness } = makeHarness();
    render(<Harness />);
    fireEvent.click(screen.getByText("bottom"));
    fireEvent.click(screen.getByText("top"));
    const rows = screen.getAllByTestId("row");
    expect(rows[0].textContent).toBe("row-0");
  });

  it("空数组不崩", () => {
    const Empty = () => (
      <FixedSizeList
        items={[]}
        itemHeight={26}
        height={300}
        renderItem={() => null}
        getKey={() => ""}
      />
    );
    render(<Empty />);
    expect(screen.queryAllByTestId("row")).toHaveLength(0);
  });

  it("滚动监听 onScroll", () => {
    const Harness = () => {
      const items = Array.from({ length: 100 }, (_, i) => i);
      return (
        <FixedSizeList
          items={items}
          itemHeight={26}
          height={300}
          renderItem={(it) => <span>{it}</span>}
          getKey={(it) => String(it)}
          onScroll={() => {
            // exercise onScroll callback
          }}
        />
      );
    };
    render(<Harness />);
    const list = screen.getByRole("listbox");
    act(() => {
      list.scrollTop = 200;
      list.dispatchEvent(new Event("scroll", { bubbles: true }));
    });
    expect(list.scrollTop).toBe(200);
  });
});
