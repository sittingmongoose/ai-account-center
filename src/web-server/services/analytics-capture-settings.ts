/**
 * What Analytics captures beyond usage numbers (DESIGN 11.1a): `cwd`, each session's working
 * folder (home written as ~), kept in the local AAC database only, never published. On by default;
 * AAC runs for other people too, so it can be turned off. Its own file, not a dashboard preference:
 * an older package rejects unknown preference keys and would reset the others after a rollback.
 */
import * as fs from 'fs';
import * as path from 'path';
import { getCcsDir } from '../../utils/config-manager';
import { assertTsWritesFile, withTsPrivateFileLeases } from '../../shared/private-files';

export interface AnalyticsCaptureSettings {
  cwd: boolean;
}

export const ANALYTICS_CAPTURE_FILE = 'analytics-capture.json';

export function readAnalyticsCaptureSettings(ccsDir = getCcsDir()): AnalyticsCaptureSettings {
  try {
    const file = path.join(ccsDir, ANALYTICS_CAPTURE_FILE);
    const stat = fs.lstatSync(file);
    if (stat.isSymbolicLink() || !stat.isFile() || stat.size > 4096) return { cwd: true };
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as Record<string, unknown>;
    return { cwd: value?.cwd !== false };
  } catch {
    return { cwd: true };
  }
}

export function writeAnalyticsCaptureSettings(
  settings: AnalyticsCaptureSettings,
  ccsDir = getCcsDir()
): void {
  const file = path.join(ccsDir, ANALYTICS_CAPTURE_FILE);
  assertTsWritesFile(file, ccsDir);
  return withTsPrivateFileLeases(ccsDir, ['analytics-capture'], () => {
    const temporary = `${file}.${process.pid}.tmp`;
    fs.mkdirSync(ccsDir, { recursive: true, mode: 0o700 });
    fs.writeFileSync(temporary, `${JSON.stringify({ cwd: settings.cwd === true })}\n`, {
      mode: 0o600,
    });
    fs.renameSync(temporary, file);
  });
}
