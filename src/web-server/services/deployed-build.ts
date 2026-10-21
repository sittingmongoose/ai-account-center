import * as fs from 'fs';
import * as path from 'path';

/**
 * The build an installed dashboard runs, recorded so the next installation can refuse a
 * package that would remove it (scripts/deploy-lineage.js). Source checkouts record nothing.
 */
export interface PackagedBuild {
  commit: string;
  branch: string | null;
  dirty: boolean;
}

const COMMIT = /^[a-f0-9]{40}$/;
const LEDGER = 'deployed-build.json';
const PACKAGE_ROOT = path.resolve(__dirname, '../../..');

function safeBranch(value: unknown): string | null {
  return typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}$/.test(value)
    ? value
    : null;
}

/** `dist/build-info.json` of the running package; null for a source checkout or older build. */
export function readPackagedBuild(packageRoot = PACKAGE_ROOT): PackagedBuild | null {
  if (
    fs.existsSync(path.join(packageRoot, '.git')) ||
    fs.existsSync(path.join(packageRoot, 'src', 'ccs.ts'))
  )
    return null;
  try {
    const file = path.join(packageRoot, 'dist', 'build-info.json');
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.size > 8 * 1024 * 1024) return null;
    const value: unknown = JSON.parse(fs.readFileSync(file, 'utf8'));
    if (typeof value !== 'object' || value === null) return null;
    const info = value as Record<string, unknown>;
    if (
      info.version !== 1 ||
      typeof info.commit !== 'string' ||
      !COMMIT.test(info.commit) ||
      typeof info.dirty !== 'boolean'
    )
      return null;
    return { commit: info.commit, branch: safeBranch(info.branch), dirty: info.dirty };
  } catch {
    return null;
  }
}

/** Write `<ccsDir>/deployed-build.json` atomically; an unchanged record is left alone. */
export function recordDeployedBuild(
  ccsDir: string,
  build: PackagedBuild | null,
  now: Date = new Date()
): boolean {
  if (!build) return false;
  const file = path.join(ccsDir, LEDGER);
  try {
    const existing: unknown = JSON.parse(fs.readFileSync(file, 'utf8'));
    const previous = existing as Record<string, unknown>;
    if (
      previous?.commit === build.commit &&
      previous.dirty === build.dirty &&
      previous.branch === build.branch
    )
      return true;
  } catch {
    // Missing or unreadable records are replaced below.
  }
  fs.mkdirSync(ccsDir, { recursive: true, mode: 0o700 });
  const temporary = `${file}.${process.pid}.tmp`;
  const record = {
    version: 1,
    commit: build.commit,
    branch: build.branch,
    dirty: build.dirty,
    recordedAt: now.toISOString(),
  };
  try {
    fs.writeFileSync(temporary, `${JSON.stringify(record, null, 2)}\n`, { mode: 0o600 });
    fs.renameSync(temporary, file);
  } finally {
    fs.rmSync(temporary, { force: true });
  }
  return true;
}
