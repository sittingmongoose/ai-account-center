import {
  getCodexProfileRenewalStatus,
  type CodexProfileRenewalProfileStatus,
} from '../../codex-auth/codex-profile-renewal';

/** Renewal entries keyed by saved profile name. */
export type CodexRenewalEntries = ReadonlyMap<string, CodexProfileRenewalProfileStatus>;

/**
 * The renewal status of every saved Codex login, read from local files only.
 * A read that throws yields no entries, so every caller keeps today's output.
 * The timer is Rust's now; this reads the same status store its cycles write.
 */
export async function readCodexRenewalEntries(
  read: () => Promise<{ profiles: CodexProfileRenewalProfileStatus[] }> = async () =>
    getCodexProfileRenewalStatus()
): Promise<CodexRenewalEntries> {
  try {
    const status = await read();
    return new Map(status.profiles.map((entry) => [entry.name, entry]));
  } catch {
    return new Map();
  }
}
