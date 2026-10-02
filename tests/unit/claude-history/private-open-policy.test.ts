import { beforeEach, afterEach, test, expect, mock, spyOn } from 'bun:test';
import fs from 'fs';
import path from 'path';
import os from 'os';
const base = path.resolve(import.meta.dir, '../../..');
let opened = 0, helperCalls = 0;
const privateCanary = 'SYNTHETIC_PRIVATE_TRANSPORT_ERROR';
const transport = path.join(base, 'src/web-server/services/claude-desktop-transport.ts');
const transportModule = await import(transport);
const transportMocks = {
  runClaudeHistoryHelper: async () => { helperCalls++; throw new Error(privateCanary); },
  openClaudeMacLauncher: async () => { opened++; await new Promise(resolve => setTimeout(resolve, 20)); },
  openClaudeWindowsLauncher: async () => { opened++; await new Promise(resolve => setTimeout(resolve, 20)); },
};
const {parseClaudeHistoryPolicy, loadClaudeHistoryPolicy, synchronizeClaudeHistoryBeforeOpen} = await import(path.join(base, 'src/web-server/services/claude-history-sync-service.ts'));
const {openClaudeDesktopProfile} = await import(path.join(base, 'src/web-server/services/claude-desktop-open-service.ts'));
const {listClaudeDesktopProfiles, listClaudeDesktopProfileMetadata} = await import(path.join(base, 'src/web-server/services/claude-desktop-profile-service.ts'));
let directory: string, oldDir: string | undefined;
const clone = (x: any) => JSON.parse(JSON.stringify(x));
const policy = () => ({version: 1, enabled: true, sourcePlatform: 'mac',
  identity: {accountUuid: '00000000-0000-4000-8000-000000000011', organizationUuid: '00000000-0000-4000-8000-000000000012'},
  project: {cwd: '/mnt/Cursor/PuppetMaster', originCwd: '/mnt/Cursor/PuppetMaster', transcriptRoot: '/synthetic/.claude/projects'},
  ssh: {mac: {alias: 'synthetic-mac', hostname: 'synthetic-vm', username: 'synthetic', port: 22}, windows: {alias: 'synthetic-windows', hostname: 'synthetic-vm', username: 'synthetic', port: 22}}});
const profile = (historySync: unknown = policy()) => ({id: 'platyr', email: 'synthetic@example.com',
  mac: {launcherName: 'Synthetic.app', launcherPath: '/Users/synthetic/Applications/Synthetic.app', profilePath: '/Users/synthetic/Library/Application Support/Claude-platyr', sshHost: 'synthetic-mac'},
  windows: {launcherName: 'Synthetic.lnk', launcherPath: 'C:\\Users\\synthetic\\Synthetic.lnk', profilePath: 'C:\\Users\\synthetic\\AppData\\Roaming\\Claude-platyr', sshHost: 'synthetic-windows'}, historySync});
const filename = () => path.join(directory, 'claude-desktop-profiles.json');
const write = (entries: unknown[] = [profile()], mode = 0o600) => fs.writeFileSync(filename(), JSON.stringify({version: 1, profiles: entries}), {mode});
beforeEach(() => { directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-history-hook-')); oldDir = process.env.CCS_DIR; process.env.CCS_DIR = directory; opened = helperCalls = 0;
  spyOn(transportModule, 'runClaudeHistoryHelper').mockImplementation(transportMocks.runClaudeHistoryHelper);
  spyOn(transportModule, 'openClaudeMacLauncher').mockImplementation(transportMocks.openClaudeMacLauncher);
  spyOn(transportModule, 'openClaudeWindowsLauncher').mockImplementation(transportMocks.openClaudeWindowsLauncher);
});
afterEach(() => { mock.restore(); if (oldDir === undefined) delete process.env.CCS_DIR; else process.env.CCS_DIR = oldDir; fs.rmSync(directory, {recursive: true, force: true}); });

test('private policy is accepted from the existing owned managed file without returning it publicly', async () => {
  write(); const parsed = (await listClaudeDesktopProfiles())[0];
  expect(await loadClaudeHistoryPolicy(parsed)).toEqual(policy());
  expect(parsed).not.toHaveProperty('historySync');
  const metadata = await listClaudeDesktopProfileMetadata();
  expect(JSON.stringify(metadata)).not.toContain('organizationUuid');
  expect(JSON.stringify(metadata)).not.toContain('transcriptRoot');
  expect(fs.readFileSync(filename(), 'utf8')).toContain('organizationUuid');
});
test('publisher retained private candidate hardlink is accepted without exposing its policy', async () => {
  write();
  const snapshots=path.join(directory,'owned-snapshot');fs.mkdirSync(snapshots,{mode:0o700});
  fs.linkSync(filename(),path.join(snapshots,'candidate.json'));
  expect(fs.statSync(filename()).nlink).toBe(2);
  const parsed=(await listClaudeDesktopProfiles())[0];
  expect(await loadClaudeHistoryPolicy(parsed)).toEqual(policy());
  expect(JSON.stringify(await listClaudeDesktopProfileMetadata())).not.toContain('accountUuid');
  expect(JSON.stringify(parsed)).not.toContain('historySync');
  expect(fs.statSync(filename()).nlink).toBe(2);
});
test('absent policy preserves the existing Open with no private helper attempt', async () => {
  const row = profile(); delete (row as any).historySync; write([row]); const before = fs.readFileSync(filename());
  await openClaudeDesktopProfile('platyr', 'windows');
  expect(opened).toBe(1); expect(helperCalls).toBe(0); expect(fs.readFileSync(filename())).toEqual(before);
});
test('invalid policy skips copy but ordinary Open still runs exactly once', async () => {
  write([profile({...policy(), identity: {...policy().identity, organizationUuid: 'invalid'}})]);
  await openClaudeDesktopProfile('platyr', 'windows'); expect(opened).toBe(1); expect(helperCalls).toBe(0);
});
test('helper failure cannot expose a private error or block one coalesced Open', async () => {
  write(); const before = fs.readFileSync(filename());
  await Promise.all([openClaudeDesktopProfile('platyr', 'windows'), openClaudeDesktopProfile('platyr', 'windows'), openClaudeDesktopProfile('platyr', 'windows')]);
  expect(opened).toBe(1); expect(helperCalls).toBe(1); expect(fs.readFileSync(filename())).toEqual(before);
  const result = await synchronizeClaudeHistoryBeforeOpen(profile(), 'windows');
  expect(result.status).toBe('skipped'); expect(JSON.stringify(result)).not.toContain(privateCanary);
});
test('active-user protected Me and Party directions skip without attempting helpers', async () => {
  for (const id of ['me', 'party']) {
    const row = {...profile(), id}; write([row]);
    const result = await synchronizeClaudeHistoryBeforeOpen(row, 'mac');
    expect(result.reason).toBe('unsupported_direction');
  }
  expect(helperCalls).toBe(0);
});
test('managed entry launcher/email drift prevents reusing a private policy', async () => {
  const expected = profile(); const changed = profile(); changed.email = 'different@example.com'; write([changed]);
  expect(await loadClaudeHistoryPolicy(expected)).toBeNull();
  changed.email = expected.email; changed.windows.sshHost = 'different-host'; write([changed]);
  expect(await loadClaudeHistoryPolicy(expected)).toBeNull();
});
test('duplicate managed profile entries never enroll an ambiguous identity', async () => { write([profile(), profile()]); expect(await loadClaudeHistoryPolicy(profile())).toBeNull(); });
test('private managed symlink is not followed', async () => {
  const real = path.join(directory, 'other.json'); fs.writeFileSync(real, JSON.stringify({version: 1, profiles: [profile()]}), {mode: 0o600}); fs.symlinkSync(real, filename());
  expect(await loadClaudeHistoryPolicy(profile())).toBeNull();
});
test('public-readable private policy file is refused without changing its permissions', async () => {
  write(); fs.chmodSync(filename(), 0o644); const before = fs.readFileSync(filename());
  expect(await loadClaudeHistoryPolicy(profile())).toBeNull(); expect(fs.statSync(filename()).mode & 0o777).toBe(0o644); expect(fs.readFileSync(filename())).toEqual(before);
});
for (const mutate of [
  (p: any) => {p.extra = privateCanary;},
  (p: any) => {p.identity.accessToken = privateCanary;},
  (p: any) => {p.project.privatePath = privateCanary;},
  (p: any) => {p.ssh.mac.privateKey = privateCanary;},
]) test('unknown private-policy fields never reach a transport request', () => { const p = clone(policy()); mutate(p); expect(parseClaudeHistoryPolicy(p)).toBeNull(); });
test('different remote endpoint cannot be a cross-device alias mapping', () => { const p = clone(policy()); p.ssh.windows.hostname = 'different'; expect(parseClaudeHistoryPolicy(p)).toBeNull(); });
test('a public request ID cannot bypass the current manifest guard', async () => { write(); await expect(openClaudeDesktopProfile('missing', 'windows')).rejects.toThrow(); expect(opened).toBe(0); expect(helperCalls).toBe(0); });
