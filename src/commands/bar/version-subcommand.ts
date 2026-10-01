import * as fs from 'fs';
import * as path from 'path';
import { getVersion } from '../../utils/version';
import { getCcsDir } from '../../config/config-loader-facade';
import { getMacAppPaths, resolveOwnedMacApp } from './native-app-paths';

export interface VersionDeps {
  getCcsDir: () => string;
  getVersion: () => string;
  getAppsDir: () => string;
}

function readRecordedVersion(ccsDir: string): string | null {
  try {
    const file = path.join(ccsDir, 'bar', '.version');
    if (!fs.lstatSync(file).isFile()) return null;
    const value = fs.readFileSync(file, 'utf8').trim();
    return value.length < 100 && /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(value)
      ? value
      : null;
  } catch {
    return null;
  }
}

/** Read metadata only; neither this command nor the native resolver changes private state. */
export async function handleBarVersion(deps: Partial<VersionDeps> = {}): Promise<void> {
  console.log(`[i] AI Account Center CLI v${(deps.getVersion ?? getVersion)()}`);
  try {
    const appsDir = (deps.getAppsDir ?? (() => getMacAppPaths().appsDir))();
    const installed = resolveOwnedMacApp(appsDir);
    if (installed) {
      console.log(`[i] AI Account Center macOS app: v${installed.version ?? 'unknown'}`);
      return;
    }
    console.log(
      '[i] AI Account Center macOS app: not installed (run `ai-account-center bar install`)'
    );
    const recorded = readRecordedVersion((deps.getCcsDir ?? getCcsDir)());
    if (recorded) console.log(`[i] Last recorded bar version: v${recorded}`);
  } catch (error) {
    console.log(
      `[!] Native app metadata unavailable: ${error instanceof Error ? error.message : String(error)}`
    );
  }
}
