import type { CodexProfileRenewalProfileStatus } from '../../codex-auth/codex-profile-renewal';
import { getCodexProfileRenewalService } from './codex-profile-renewal-service';

/** Renewal entries keyed by saved profile name. */
export type CodexRenewalEntries = ReadonlyMap<string, CodexProfileRenewalProfileStatus>;

/**
 * The renewal status of every saved Codex login, read from local files only.
 * A read that throws yields no entries, so every caller keeps today's output.
 */
export async function readCodexRenewalEntries(
  read: () => Promise<{ profiles: CodexProfileRenewalProfileStatus[] }> = () =>
    getCodexProfileRenewalService().getStatus()
): Promise<CodexRenewalEntries> {
  try {
    const status = await read();
    return new Map(status.profiles.map((entry) => [entry.name, entry]));
  } catch {
    return new Map();
  }
}
