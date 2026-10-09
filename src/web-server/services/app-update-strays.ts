/**
 * Other Codex or Claude copies beside a managed install. The helper names each one
 * with a fixed place word and a bounded version; nothing here reads a file or runs one.
 */
export type AppUpdateStrayLocation = 'usr-local' | 'homebrew' | 'bun' | 'user-npm' | 'other';
/** A copy beside its managed install; the helper sends only these fixed fields. */
export interface AppUpdateStray {
  location: AppUpdateStrayLocation;
  /** The copy's own version, or null when its read-only probe could not read one. */
  version: string | null;
  /** True when this copy comes before the managed install on the computer's PATH. */
  shadows: boolean;
}
export const STRAY_LOCATIONS: readonly AppUpdateStrayLocation[] = [
  'usr-local',
  'homebrew',
  'bun',
  'user-npm',
  'other',
];
/** At most this many copies are kept from one row. */
export const MAX_STRAYS = 4;
/** Fixed words for where a stray copy sits: the helper never sends a folder. */
const STRAY_PLACES: Record<AppUpdateStrayLocation, string> = {
  'usr-local': '/usr/local/bin',
  homebrew: 'the Homebrew bin folder',
  bun: 'the Bun bin folder',
  'user-npm': 'a user npm folder',
  other: 'another PATH folder',
};
/** A dotted version is compared over at most this many numbers (the helper's versions have at most six). */
const VERSION_PARTS = 6;

/** The first copy that shadows the managed install and is older than `version`, if any. */
export function olderShadow(
  strays: AppUpdateStray[],
  version: string | null
): (AppUpdateStray & { version: string }) | undefined {
  if (version === null) return undefined;
  return strays.find(
    (stray): stray is AppUpdateStray & { version: string } =>
      stray.shadows && stray.version !== null && isOlderVersion(stray.version, version)
  );
}

/** The one fixed sentence an older shadowing copy earns, after the row's own words. */
export function strayNote(label: string, stray: AppUpdateStray & { version: string }): string {
  return `An older ${label} copy (${stray.version}) in ${STRAY_PLACES[stray.location]} comes first on some PATHs; remove it so it never runs instead.`;
}

/** True when `value` is a lower dotted version than `than`; a version with a non-numeric part is never older. */
function isOlderVersion(value: string, than: string): boolean {
  const left = numericParts(value);
  const right = numericParts(than);
  if (!left || !right) return false;
  for (let index = 0; index < VERSION_PARTS; index += 1) {
    if (left[index] !== right[index]) return left[index] < right[index];
  }
  return false;
}

/** The numbers of a version's core (0.153.4 from 0.153.4-beta), padded to VERSION_PARTS; null when one is not a number. */
function numericParts(version: string): number[] | null {
  const parts = version.split(/[-+]/)[0].split('.').map(Number);
  if (!parts.every((part) => Number.isInteger(part))) return null;
  return [...parts, ...Array<number>(VERSION_PARTS).fill(0)].slice(0, VERSION_PARTS);
}
