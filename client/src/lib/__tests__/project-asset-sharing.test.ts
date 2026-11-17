import { expect, it, vi } from "vitest";
import { withProjectAssetSharing } from "../project-asset-sharing";

it("lets concurrent sharing surfaces reuse the first persisted document instead of uploading twice", async () => {
  let finish!: () => void, documentId: string | undefined;
  const upload = vi.fn(async () => { await new Promise<void>(resolve => { finish = resolve; }); return "doc"; });
  const share = () => withProjectAssetSharing("p", "account", "shared", "asset", async () => {
    if (documentId) return documentId;
    documentId = await upload(); return documentId;
  });
  const first = share(), second = share();
  await vi.waitFor(() => expect(upload).toHaveBeenCalledOnce());
  finish(); expect(await Promise.all([first, second])).toEqual(["doc", "doc"]); expect(upload).toHaveBeenCalledOnce();
});
it("releases a failed attempt so a queued retry can proceed, without blocking another account", async () => {
  let fail!: (error: Error) => void;
  const first = withProjectAssetSharing("p", "a", "shared", "asset", () => new Promise<void>((_resolve, reject) => { fail = reject; }));
  const failure = expect(first).rejects.toThrow("offline");
  const retry = withProjectAssetSharing("p", "a", "shared", "asset", async () => "retried");
  expect(await withProjectAssetSharing("p", "b", "shared", "asset", async () => "independent")).toBe("independent");
  fail(new Error("offline")); await failure; expect(await retry).toBe("retried");
});
