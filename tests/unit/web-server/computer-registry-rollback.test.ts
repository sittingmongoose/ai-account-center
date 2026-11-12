import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  defaultDashboardPreferences,
  readDashboardPreferences,
  writeDashboardPreferences,
  type UsageLogSource,
} from '../../../src/web-server/services/dashboard-preferences';
import { DASHBOARD_HOSTS } from '../../../src/web-server/services/dashboard-hosts';
import {
  AppUpdateService,
  UPDATE_APP_LABELS,
} from '../../../src/web-server/services/app-update-service';
import {
  LegacyAppUpdateReader,
  readDashboardPreferences as readLegacyPreferences,
} from '../../fixtures/legacy-computer-readers';

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
});
function directory(): string {
  const result = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-registry-rollback-'));
  directories.push(result);
  return result;
}
const legacyHosts = ['ubuntu', 'mac', 'windows', 'nas1'];
const nas1: UsageLogSource = { id: 'nas1', tool: 'omp', host: 'nas1', path: '/logs/nas1' };
const alexis: UsageLogSource = {
  id: 'alexis',
  tool: 'codex',
  host: 'alexis',
  path: 'C:\\logs\\codex',
};
const preferences = {
  timeZone: 'Europe/London',
  snapshotCleanup: { auto: false },
  usageLogSources: [nas1, alexis],
};
const extraFile = 'dashboard-preferences-extra-sources.json';
const expandedFile = 'dashboard-job-expanded.json';
const rows = Object.keys(UPDATE_APP_LABELS).map((appId) => ({
  appId,
  status: 'current',
  messageCode: 'current',
  previousVersion: '1.2.3',
  version: '1.2.3',
  manager: 'native',
  updateAttempted: false,
  restartedProcesses: 0,
}));
const payload = () => JSON.stringify({ results: rows });
async function complete(service: AppUpdateService): Promise<void> {
  for (let i = 0; i < 50 && service.getStatus().job?.state === 'running'; i++)
    await new Promise((resolve) => setTimeout(resolve, 0));
  expect(service.getStatus().job?.state).toBe('completed');
}
async function savedJob() {
  const root = directory();
  const service = new AppUpdateService({ ccsDir: root, runHost: async () => payload() });
  service.start();
  await complete(service);
  return { root, saved: path.join(root, 'app-updates'), job: service.getStatus().job! };
}

describe('computer registry rollback storage', () => {
  it('new preference writes preserve the old reader settings and all four original computers', () => {
    const root = directory();
    const sources: UsageLogSource[] = legacyHosts.map((host) => ({
      id: host,
      tool: 'omp',
      host: host as UsageLogSource['host'],
      path: '/logs/' + host,
    }));
    const value = { ...preferences, usageLogSources: [...sources, alexis] };
    expect(writeDashboardPreferences(value, root)).toEqual(value);
    expect(readLegacyPreferences(root)).toEqual({ ...value, usageLogSources: sources });
    expect(readDashboardPreferences(root)).toEqual(value);
    expect(JSON.parse(fs.readFileSync(path.join(root, extraFile), 'utf8'))).toEqual({
      usageLogSources: [alexis],
    });
    if (process.platform !== 'win32') {
      for (const file of ['dashboard-preferences.json', extraFile])
        expect(fs.statSync(path.join(root, file)).mode & 0o777).toBe(0o600);
    }
  });

  it('merges old-package preference edits and durably removes extra sources', () => {
    const root = directory();
    writeDashboardPreferences(preferences, root);
    const old = { ...readLegacyPreferences(root), timeZone: 'UTC' };
    fs.writeFileSync(path.join(root, 'dashboard-preferences.json'), JSON.stringify(old));
    expect(readDashboardPreferences(root)).toEqual({ ...preferences, timeZone: 'UTC' });
    const removed = { ...old, usageLogSources: [nas1] };
    expect(writeDashboardPreferences(removed, root)).toEqual(removed);
    expect(readDashboardPreferences(root)).toEqual(removed);
    expect(fs.existsSync(path.join(root, extraFile))).toBe(false);
  });

  it('ignores invalid companions without relaxing source, duplicate or combined-count guards', () => {
    const root = directory();
    const main = { ...preferences, usageLogSources: [nas1] };
    writeDashboardPreferences(main, root);
    const file = path.join(root, extraFile);
    for (const contents of [
      '{broken',
      ' '.repeat(65537),
      JSON.stringify({ usageLogSources: [{ ...alexis, host: 'unknown' }] }),
      JSON.stringify({ usageLogSources: [{ ...alexis, path: 'C:\\logs\\..\\escape' }] }),
      JSON.stringify({ usageLogSources: [nas1] }),
      JSON.stringify({ usageLogSources: [{ ...alexis, id: nas1.id }] }),
      JSON.stringify({
        usageLogSources: Array.from({ length: 64 }, (_, i) => ({
          ...alexis,
          id: `source-${i}`,
        })),
      }),
    ]) {
      fs.writeFileSync(file, contents);
      expect(readDashboardPreferences(root)).toEqual(main);
    }
    fs.unlinkSync(file);
    const target = path.join(root, 'target.json');
    fs.writeFileSync(target, JSON.stringify({ usageLogSources: [alexis] }));
    fs.symlinkSync(target, file);
    expect(readDashboardPreferences(root)).toEqual(main);
  });

  it('still reads an earlier single-file registry preference save and splits its next write', () => {
    const root = directory();
    fs.writeFileSync(path.join(root, 'dashboard-preferences.json'), JSON.stringify(preferences));
    expect(readDashboardPreferences(root)).toEqual(preferences);
    writeDashboardPreferences(readDashboardPreferences(root), root);
    expect(readLegacyPreferences(root)).toEqual({ ...preferences, usageLogSources: [nas1] });
    expect(readDashboardPreferences(root)).toEqual(preferences);
    expect(readDashboardPreferences(directory())).toEqual(defaultDashboardPreferences());
  });

  it('new completed update writes restore 40 rows in the old reader and 50 in the new reader', async () => {
    const { root, saved, job } = await savedJob();
    const old = new LegacyAppUpdateReader(root).getStatus().job!;
    expect(old).not.toBeNull();
    expect(old.id).toBe(job.id);
    expect(old.state).toBe('completed');
    expect(old.expectedResults).toBe(40);
    expect(old.results).toEqual(job.results.filter((row) => legacyHosts.includes(row.platform)));
    expect(old.results).toHaveLength(40);
    expect(Object.keys(old.hosts!).sort()).toEqual([...legacyHosts].sort());
    expect(new AppUpdateService({ ccsDir: root }).getStatus().job).toEqual(job);
    expect(job.results).toHaveLength(DASHBOARD_HOSTS.length * rows.length);
    if (process.platform !== 'win32') {
      for (const file of ['dashboard-job.json', expandedFile])
        expect(fs.statSync(path.join(saved, file)).mode & 0o777).toBe(0o600);
    }
  });

  it('keeps a running job readable by the old package after more than 40 streamed rows', async () => {
    const root = directory();
    let release!: (value: string) => void;
    const blocked = new Promise<string>((resolve) => {
      release = resolve;
    });
    const service = new AppUpdateService({
      ccsDir: root,
      runHost: async (_host, control) => {
        for (const row of rows) control.onEvent({ event: 'result', result: row });
        return blocked;
      },
    });
    service.start();
    try {
      for (let i = 0; i < 20 && service.getStatus().job!.results.length < 50; i++)
        await new Promise((resolve) => setTimeout(resolve, 0));
      expect(service.getStatus().job!.results).toHaveLength(50);
      const old = new LegacyAppUpdateReader(root).getStatus().job!;
      expect(old.state).toBe('running');
      expect(old.expectedResults).toBe(40);
      expect(old.results).toHaveLength(40);
      expect(old.results.every((row) => legacyHosts.includes(row.platform))).toBe(true);
      expect(new AppUpdateService({ ccsDir: root }).getStatus().job!.results).toHaveLength(50);
    } finally {
      release(payload());
      await complete(service);
    }
  });

  it('still accepts legacy-only saved jobs', async () => {
    const { root, saved } = await savedJob();
    fs.unlinkSync(path.join(saved, expandedFile));
    const job = new AppUpdateService({ ccsDir: root }).getStatus().job!;
    expect(job.results).toHaveLength(40);
    expect(job.expectedResults).toBe(40);
    expect(job.hosts!.alexis).toEqual({ state: 'done', currentApp: null, phase: null });
  });

  it('falls back to the legacy job when the expanded file is invalid or exceeds its guards', async () => {
    const { root, saved } = await savedJob();
    const file = path.join(saved, expandedFile);
    const expanded = JSON.parse(fs.readFileSync(file, 'utf8'));
    for (const value of [
      '{broken',
      ' '.repeat(65537),
      JSON.stringify({ ...expanded, job: { ...expanded.job, id: 'bad-id' } }),
      JSON.stringify({
        ...expanded,
        job: { ...expanded.job, results: Array(51).fill(expanded.job.results[0]) },
      }),
    ]) {
      fs.writeFileSync(file, value);
      expect(new AppUpdateService({ ccsDir: root }).getStatus().job!.results).toHaveLength(40);
    }
  });

  it('accepts a later old-package job instead of a stale expanded companion', async () => {
    const { root, saved } = await savedJob();
    const file = path.join(saved, 'dashboard-job.json');
    const legacy = JSON.parse(fs.readFileSync(file, 'utf8'));
    legacy.job.id = '22222222-2222-4222-8222-222222222222';
    fs.writeFileSync(file, JSON.stringify(legacy));
    const job = new AppUpdateService({ ccsDir: root }).getStatus().job!;
    expect(job.id).toBe(legacy.job.id);
    expect(job.results).toHaveLength(40);
  });
});
