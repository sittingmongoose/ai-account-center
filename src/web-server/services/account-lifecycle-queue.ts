/**
 * In-process queues for account lifecycle writes. Every write that touches an
 * API-key provider's keys or registry entries (add, replace key, remove, the
 * orphan-key sweep) runs in that provider's `key:<provider>` queue, so two of
 * them never interleave.
 */
const queues = new Map<string, Promise<void>>();

export function serialized<T>(name: string, task: () => Promise<T>): Promise<T> {
  const previous = queues.get(name) ?? Promise.resolve();
  const result = previous.then(task);
  const tail = result.then(
    () => undefined,
    () => undefined
  );
  queues.set(name, tail);
  void tail.then(() => {
    if (queues.get(name) === tail) queues.delete(name);
  });
  return result;
}

/** The queue of one provider's keys and registry entries. */
export function keyQueue(provider: string): string {
  return `key:${provider}`;
}
