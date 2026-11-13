/** Canonical path checks for the shared Codex plugin cache. */
import * as fs from 'fs';
import * as path from 'path';

export function resolveCanonicalPath(targetPath: string): string {
  try {
    return fs.realpathSync.native(targetPath);
  } catch {
    return path.resolve(targetPath);
  }
}

export function symlinkPointsTo(linkPath: string, expectedTarget: string): boolean {
  try {
    const currentTarget = fs.readlinkSync(linkPath);
    const resolvedCurrentTarget = path.resolve(path.dirname(linkPath), currentTarget);
    return resolveCanonicalPath(resolvedCurrentTarget) === resolveCanonicalPath(expectedTarget);
  } catch {
    return false;
  }
}
