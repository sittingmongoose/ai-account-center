import * as fs from 'fs';
import * as os from 'os';
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

/**
 * Every existing directory an installer may read the record from: the active CCS directory
 * (which `--config-dir` can move), the one CCS_DIR or CCS_HOME names, and the default ~/.ccs.
 */
export function deployedBuildDirs(
  active: string,
  env: NodeJS.ProcessEnv = process.env,
  home: string = os.homedir()
): string[] {
  const named = env.CCS_DIR
    ? path.resolve(env.CCS_DIR)
    : path.join(path.resolve(env.CCS_HOME || home), '.ccs');
  return [
    ...new Set([active, named, path.join(home, '.ccs')].map((dir) => path.resolve(dir))),
  ].filter((dir) => {
    try {
      return fs.statSync(dir).isDirectory();
    } catch {
      return false;
    }
  });
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
  const temporary = `${file}.${process.pid}.tmp`;
  const record = {
    version: 1,
    commit: build.commit,
    branch: build.branch,
    dirty: build.dirty,
    recordedAt: now.toISOString(),
  };
  try {
    fs.writeFileSync(temporary, `${JSON.stringify(record, null, 2)}\n`, {
      mode: 0o600,
      flag: 'wx',
    });
    fs.renameSync(temporary, file);
  } finally {
    fs.rmSync(temporary, { force: true });
  }
  return true;
}

/** Record the running build everywhere an installer may look; returns the failed directories. */
export function recordRunningBuild(
  active: string,
  build: PackagedBuild | null,
  env: NodeJS.ProcessEnv = process.env
): number {
  if (!build) return 0;
  let failed = 0;
  for (const dir of deployedBuildDirs(active, env)) {
    try {
      recordDeployedBuild(dir, build);
    } catch {
      failed++;
    }
  }
  return failed;
}
