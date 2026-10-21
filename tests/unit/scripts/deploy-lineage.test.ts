import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { spawnSync } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  readPackagedBuild,
  recordDeployedBuild,
} from '../../../src/web-server/services/deployed-build';

interface LineageCheck {
  ok: boolean;
  message: string | null;
}
const SCRIPTS = path.resolve(__dirname, '../../../scripts');
const lineage = require(path.join(SCRIPTS, 'deploy-lineage.js')) as {
  checkDeployLineage: (options: {
    packageRoot: string;
    env?: Record<string, string | undefined>;
  }) => LineageCheck;
  isSourceCheckout: (root: string) => boolean;
  parseLedger: (value: unknown) => { commit: string; branch: string | null } | null;
  readSourceBuild: (root: string) => {
    commit: string;
    branch: string | null;
    dirty: boolean;
    lineage: string[];
  } | null;
  recordBuild: (root: string, outDir?: string) => { commit: string | null };
};

const A = 'a'.repeat(40);
const B = 'b'.repeat(40);
const C = 'c'.repeat(40);
let scope = '';
let ccsHome = '';
let packageRoot = '';

beforeEach(() => {
  scope = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-deploy-lineage-'));
  ccsHome = path.join(scope, 'home');
  packageRoot = path.join(scope, 'package');
  fs.mkdirSync(path.join(ccsHome, '.ccs'), { recursive: true });
  fs.mkdirSync(path.join(packageRoot, 'dist'), { recursive: true });
});
afterEach(() => fs.rmSync(scope, { recursive: true, force: true }));

function env(extra: Record<string, string> = {}): Record<string, string | undefined> {
  return { CCS_HOME: ccsHome, CCS_DIR: '', ...extra };
}

function packaged(info: Record<string, unknown> | null): void {
  const file = path.join(packageRoot, 'dist', 'build-info.json');
  if (info === null) fs.rmSync(file, { force: true });
  else
    fs.writeFileSync(
      file,
      JSON.stringify({ version: 1, branch: 'main', builtAt: '2026-10-21T00:00:00.000Z', ...info })
    );
}

function deployed(commit: string, extra: Record<string, unknown> = {}): void {
  fs.writeFileSync(
    path.join(ccsHome, '.ccs', 'deployed-build.json'),
    JSON.stringify({ version: 1, commit, branch: 'fix/muse-usage-refresh', dirty: false, ...extra })
  );
}

function check(extra: Record<string, string> = {}): LineageCheck {
  return lineage.checkDeployLineage({ packageRoot, env: env(extra) });
}

function git(root: string, ...args: string[]): string {
  const result = spawnSync(
    'git',
    ['-C', root, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.test', ...args],
    { encoding: 'utf8' }
  );
  if (result.status !== 0) throw new Error(result.stderr);
  return result.stdout.trim();
}

describe('deployment lineage guard', () => {
  it('allows the first guarded install and any package containing the running build', () => {
    packaged({ commit: C, dirty: false, lineage: [C, B] });
    expect(check()).toEqual({ ok: true, message: null });
    deployed(B);
    expect(check()).toEqual({ ok: true, message: null });
    deployed(C);
    expect(check()).toEqual({ ok: true, message: null });
  });

  it('refuses a package that would remove the running branch-only fix', () => {
    packaged({ commit: C, dirty: false, lineage: [C, A] });
    deployed(B);
    const result = check();
    expect(result.ok).toBe(false);
    expect(result.message).toContain('does not contain bbbbbbbb (fix/muse-usage-refresh)');
    expect(result.message).toContain('Merge fix/muse-usage-refresh into main');
    expect(result.message).toContain('AAC_ALLOW_DEPLOY_REGRESSION=1');
  });

  it('allows an intentional rollback only with the override and still explains it', () => {
    packaged({ commit: C, dirty: false, lineage: [C, A] });
    deployed(B);
    const result = check({ AAC_ALLOW_DEPLOY_REGRESSION: '1' });
    expect(result.ok).toBe(true);
    expect(result.message).toContain('Installing anyway');
  });

  it('refuses uncommitted builds and packages without lineage once a build is recorded', () => {
    packaged({ commit: C, dirty: true, lineage: [C] });
    expect(check().ok).toBe(false);
    expect(check().message).toContain('uncommitted changes');
    packaged(null);
    expect(check()).toEqual({ ok: true, message: null });
    deployed(B);
    expect(check().ok).toBe(false);
    expect(check().message).toContain('does not record the commits');
    packaged({ commit: C, dirty: false, lineage: [C, B] });
    deployed(B, { dirty: true });
    expect(check().ok).toBe(false);
    expect(check().message).toContain('with uncommitted changes');
  });

  it('honors CCS_DIR for the running build record', () => {
    const custom = path.join(scope, 'custom');
    fs.mkdirSync(custom);
    fs.writeFileSync(
      path.join(custom, 'deployed-build.json'),
      JSON.stringify({ version: 1, commit: B, branch: null, dirty: false })
    );
    packaged({ commit: C, dirty: false, lineage: [C] });
    expect(check().ok).toBe(true);
    expect(check({ CCS_DIR: custom }).ok).toBe(false);
  });

  it('makes postinstall fail before touching storage when it would regress', () => {
    fs.mkdirSync(path.join(packageRoot, 'scripts'));
    for (const name of ['postinstall.js', 'deploy-lineage.js'])
      fs.copyFileSync(path.join(SCRIPTS, name), path.join(packageRoot, 'scripts', name));
    packaged({ commit: C, dirty: false, lineage: [C, A] });
    deployed(B);
    const run = () =>
      spawnSync('node', [path.join(packageRoot, 'scripts', 'postinstall.js')], {
        encoding: 'utf8',
        env: { ...process.env, ...env() } as NodeJS.ProcessEnv,
      });
    const refused = run();
    expect(refused.status).toBe(1);
    expect(refused.stderr).toContain('would remove deployed fixes');
    packaged({ commit: C, dirty: false, lineage: [C, B, A] });
    const accepted = run();
    expect(accepted.status).toBe(0);
    expect(accepted.stdout).toContain('storage is ready');
  });

  it('skips source checkouts so dependency installs in a worktree are unaffected', () => {
    expect(lineage.isSourceCheckout(path.resolve(__dirname, '../../..'))).toBe(true);
    expect(lineage.isSourceCheckout(packageRoot)).toBe(false);
  });

  it('records the full lineage and uncommitted package inputs of a checkout', () => {
    const repo = path.join(scope, 'repo');
    fs.mkdirSync(path.join(repo, 'src'), { recursive: true });
    git(repo, 'init', '-q', '-b', 'main');
    fs.writeFileSync(path.join(repo, 'src', 'a.ts'), 'export const a = 1;\n');
    git(repo, 'add', '.');
    git(repo, 'commit', '-q', '-m', 'first');
    const first = git(repo, 'rev-parse', 'HEAD');
    git(repo, 'checkout', '-q', '-b', 'fix/branch');
    fs.writeFileSync(path.join(repo, 'src', 'b.ts'), 'export const b = 2;\n');
    git(repo, 'add', '.');
    git(repo, 'commit', '-q', '-m', 'second');
    const second = git(repo, 'rev-parse', 'HEAD');
    fs.writeFileSync(path.join(repo, 'notes.txt'), 'scratch\n');
    expect(lineage.readSourceBuild(repo)).toMatchObject({
      commit: second,
      branch: 'fix/branch',
      dirty: false,
      lineage: [second, first],
    });
    fs.writeFileSync(path.join(repo, 'src', 'c.ts'), 'export const c = 3;\n');
    expect(lineage.readSourceBuild(repo)?.dirty).toBe(true);
    expect(lineage.readSourceBuild(path.join(repo, 'src'))).toBeNull();
  });
});

describe('running build record', () => {
  it('reads an installed package build and ignores source checkouts', () => {
    packaged({ commit: C, dirty: false, lineage: [C] });
    expect(readPackagedBuild(packageRoot)).toEqual({ commit: C, branch: 'main', dirty: false });
    expect(readPackagedBuild(path.resolve(__dirname, '../../..'))).toBeNull();
    packaged({ commit: null, dirty: null, lineage: [] });
    expect(readPackagedBuild(packageRoot)).toBeNull();
  });

  it('writes a private record the install guard reads, and leaves an unchanged one alone', () => {
    const ccsDir = path.join(ccsHome, '.ccs');
    const build = { commit: B, branch: 'main', dirty: false };
    expect(recordDeployedBuild(ccsDir, build, new Date('2026-10-21T01:00:00.000Z'))).toBe(true);
    const file = path.join(ccsDir, 'deployed-build.json');
    const first = fs.readFileSync(file, 'utf8');
    expect(lineage.parseLedger(JSON.parse(first))).toEqual({
      commit: B,
      branch: 'main',
      dirty: false,
    });
    if (process.platform !== 'win32') expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    recordDeployedBuild(ccsDir, build, new Date('2026-10-21T02:00:00.000Z'));
    expect(fs.readFileSync(file, 'utf8')).toBe(first);
    expect(fs.readdirSync(ccsDir)).toEqual(['deployed-build.json']);
    expect(recordDeployedBuild(ccsDir, null)).toBe(false);
    packaged({ commit: C, dirty: false, lineage: [C, A] });
    expect(check().ok).toBe(false);
  });
});
