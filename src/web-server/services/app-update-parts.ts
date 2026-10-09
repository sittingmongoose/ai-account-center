import { record, safeVersion } from './app-update-values';

/** One ACP adapter on a `t3-acp-adapters` row. `inUse` is set only for a part that was behind and skipped
 * because it is in use: it keeps its installed version, while another part may still have updated. */
export interface AdapterPart {
  name: 'muse-acp' | 'zcode-acp-server';
  previousVersion: string | null;
  version: string | null;
  inUse?: true;
}

/** Names each held adapter and says which one updated, so a row with a held part never claims nothing changed. */
export function adapterInUseMessage(parts: AdapterPart[]): string {
  const held = parts.filter((part) => part.inUse === true).map((part) => part.name);
  const updated = parts
    .filter(
      (part) =>
        part.inUse !== true && part.version !== null && part.version !== part.previousVersion
    )
    .map((part) => `${part.name} updated`);
  const names = held.join(' and ');
  const state =
    held.length === 1
      ? `${names} is in use (open, or running in a T3 session), so it stayed at its installed version`
      : `${names} are in use (open, or running in T3 sessions), so they stayed at their installed versions`;
  const closing = held.length === 1 ? 'once it is closed' : 'once they are closed';
  return [state, ...updated, `run Update apps again ${closing}`].join('; ') + '.';
}

const ACP_PARTS = ['muse-acp', 'zcode-acp-server'] as const;
/** The adapter names are a closed set of two, each kept once, so a row has at most two parts. */
export function normalizeParts(value: unknown): AdapterPart[] {
  const parts: AdapterPart[] = [];
  for (const candidate of Array.isArray(value) ? value : []) {
    const part = record(candidate);
    const name = part?.name as (typeof ACP_PARTS)[number] | undefined;
    if (!part || !name || !ACP_PARTS.includes(name) || parts.some((known) => known.name === name))
      continue;
    parts.push({
      name,
      previousVersion: safeVersion(part.previousVersion),
      version: safeVersion(part.version),
      // Only a boolean true marks a held part; any other value is ignored.
      ...(part.inUse === true ? { inUse: true as const } : {}),
    });
  }
  return parts;
}
