const pending = new Map<string, Promise<unknown>>();

/** Serialize each asset across sharing surfaces, then let the next writer recheck its durable link. */
export function withProjectAssetSharing<T>(projectId: string, account: string, sharedId: string, assetId: string, work: () => Promise<T>): Promise<T> {
  const key = JSON.stringify([projectId, account, sharedId, assetId]);
  const result = (pending.get(key) ?? Promise.resolve()).catch(() => {}).then(work);
  pending.set(key, result);
  const release = () => { if (pending.get(key) === result) pending.delete(key); };
  void result.then(release, release);
  return result;
}
