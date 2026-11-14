/** Keep partial failures visible; only successful items may leave the UI. */
export async function runBatchOperation(paths: readonly string[], action: (path: string) => Promise<unknown>) {
  const unique = [...new Set(paths)];
  const results = await Promise.allSettled(unique.map(path => Promise.resolve().then(() => action(path))));
  const succeeded = new Set<string>();
  const failed: { path: string; error: unknown }[] = [];
  results.forEach((result, index) => {
    if (result.status === 'fulfilled') succeeded.add(unique[index]);
    else failed.push({ path: unique[index], error: result.reason });
  });
  return { succeeded, failed };
}
