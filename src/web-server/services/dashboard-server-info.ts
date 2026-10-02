import { getVersion } from '../../utils/version';
import type { DashboardServerInfo } from './account-dashboard-types';

export type { DashboardServerInfo } from './account-dashboard-types';

let buildCommit: string | null = null;

/** Set once by startServer from dist/ui/ui-build-manifest.json; anything else is null. */
export function setDashboardBuildCommit(commit: string | null): void {
  buildCommit = typeof commit === 'string' && /^[a-f0-9]{7,40}$/.test(commit) ? commit : null;
}

/** No paths, host names or runtime versions; only the package version and a commit. */
export function getDashboardServerInfo(): DashboardServerInfo | undefined {
  const version = getVersion();
  if (!/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]{1,32})?$/.test(version) || version === '0.0.0')
    return undefined;
  return { version, commit: buildCommit };
}
