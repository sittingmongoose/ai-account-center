import fs from 'fs';
import path from 'path';

const MARKER_SEGMENT = /^[A-Za-z0-9._-]{1,64}$/;

/**
 * The hold when the copy core cannot be loaded. Without the core the managed set is unknown, so the hold is read
 * from the marker names alone: an Open is held while a marker named for this profile and platform exists (pending
 * or finished, its state cannot be checked without the core), or while the marker folder cannot be listed. Other
 * launchers keep their ordinary Open.
 */
export function claudeHistoryOpenHeldWithoutCore(
  ccsDir: string,
  profileId: string,
  platform: 'mac' | 'windows'
): boolean {
  if (!MARKER_SEGMENT.test(profileId)) return true;
  let names: string[];
  try {
    names = fs.readdirSync(path.join(ccsDir, 'claude-history-pending'));
  } catch (error) {
    return (error as NodeJS.ErrnoException).code !== 'ENOENT';
  }
  const prefix = `${profileId}-${platform}-`;
  return names.some(
    (name) => name.startsWith(prefix) && /^[0-9a-f]{32}\.json$/.test(name.slice(prefix.length))
  );
}
