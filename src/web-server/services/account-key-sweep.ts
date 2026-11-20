import { KEY_PROVIDERS, LocalKeyStore } from './account-key-store';
import { readAdditionalEntries } from './account-lifecycle-accounts';
import { keyQueue, serialized } from './account-lifecycle-queue';

/** Older than this, a key file no account names is not an Add or a Replace key in flight. */
export const ORPHAN_KEY_AGE_MS = 60 * 60_000;

/**
 * Startup and daily maintenance: delete this host's key files that no registry
 * entry names and that are older than an hour, such as a Replace key trial
 * key or an Add that a crash interrupted. Each provider is swept inside its
 * key queue, so an Add, Replace key or Remove never races it. Nothing is
 * deleted unless the version 2 registry reads cleanly. Returns the count.
 */
export async function sweepOrphanKeys(
  ccsDir: string,
  now: () => number = Date.now,
  store: LocalKeyStore = new LocalKeyStore(ccsDir)
): Promise<number> {
  let removed = 0;
  for (const provider of KEY_PROVIDERS) {
    removed += await serialized(ccsDir, keyQueue(provider), async () => {
      let named: Set<string>;
      try {
        const { mode, entries } = await readAdditionalEntries(ccsDir);
        if (mode !== 'v2') return 0;
        named = new Set(
          entries.flatMap((entry) =>
            entry.provider === provider && entry.credential.kind === 'aac-key'
              ? [entry.credential.keyId]
              : []
          )
        );
      } catch {
        return 0;
      }
      const native = await store.sweepOrphans(provider, named, now).catch(() => 0);
      if (native !== undefined) return native;
      const files = await store.keyFiles(provider);
      const checkedAt = now();
      let count = 0;
      for (const file of files) {
        if (named.has(file.keyId) || checkedAt - file.modifiedAt < ORPHAN_KEY_AGE_MS) continue;
        try {
          await store.delete(provider, file.keyId);
          count += 1;
        } catch {
          /* Retried at the next sweep. */
        }
      }
      return count;
    });
  }
  return removed;
}
