/**
 * The Claude SSH-mirror owners file: which host owns each orphan mirror (`claudeName`), saved
 * beside the per-host caches so a mirror's ownership survives its owner's cache file going
 * missing. Without it, a missing file re-elects the mirror to the next host that holds a copy,
 * and hours that settle on the new owner before the old one returns are counted twice
 * (history-L5). The table is AAC's own state under its cache folder, written atomically and
 * privately like the caches; a missing, damaged or future-shaped file reads as empty, and the
 * election then runs fresh over what the hosts hold now.
 */
import * as fs from 'fs';
import * as path from 'path';
import { REMOTE_ANALYTICS_HOSTS, type AnalyticsScanHost } from './analytics-remote-transport';

const MIRROR_OWNERS_VERSION = 1;
const MAX_MIRROR_OWNERS_BYTES = 1024 * 1024;

/** `analytics-remote-v1/claude-mirror-owners.json` under the cache dir. */
export function mirrorOwnersFile(cacheDir: string): string {
  return path.join(cacheDir, 'analytics-remote-v1', 'claude-mirror-owners.json');
}

/** The saved owner of each mirror name; a missing or damaged file reads as none. */
export function loadClaudeMirrorOwners(file: string): Map<string, AnalyticsScanHost> {
  const owners = new Map<string, AnalyticsScanHost>();
  try {
    if (fs.statSync(file).size > MAX_MIRROR_OWNERS_BYTES) return owners;
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as {
      version?: unknown;
      owners?: unknown;
    };
    if (value.version !== MIRROR_OWNERS_VERSION) return owners;
    if (typeof value.owners !== 'object' || value.owners === null) return owners;
    for (const [name, host] of Object.entries(value.owners as Record<string, unknown>))
      if (
        name &&
        typeof host === 'string' &&
        (REMOTE_ANALYTICS_HOSTS as readonly string[]).includes(host)
      )
        owners.set(name, host as AnalyticsScanHost);
  } catch {
    /* A damaged table elects fresh; the election rewrites it. */
  }
  return owners;
}

/** Saves the table atomically and privately, best-effort: a failed save only re-elects next time. */
export function saveClaudeMirrorOwners(
  file: string,
  owners: ReadonlyMap<string, AnalyticsScanHost>
): boolean {
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    const body = JSON.stringify({
      version: MIRROR_OWNERS_VERSION,
      owners: Object.fromEntries(owners),
    });
    const temporary = `${file}.${process.pid}.tmp`;
    fs.writeFileSync(temporary, body, { mode: 0o600 });
    fs.renameSync(temporary, file);
    return true;
  } catch {
    return false;
  }
}
