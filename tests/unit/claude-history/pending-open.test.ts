import {beforeEach, afterEach, test, expect, mock, spyOn} from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
const base = path.resolve(import.meta.dir, '../../..');
const fx = require('./synthetic-history-fixtures.cjs');
const { MAC_PROFILE } = fx;
const core = require('../../../scripts/claude-history/history-index-sync.cjs');
const seed = fx.bindings('P');
let directory: string, oldDir: string | undefined;
let opened = 0, appendCalls = 0, behavior = 'lost', closed = true;
const canary = 'SYNTHETIC_PRIVATE_LOST_RECEIPT';
function snapshot(platform: string) {
  const records = platform === 'mac' ? [fx.envelope(fx.record(seed))] : [];
  return {profileId:MAC_PROFILE, platform,
    identity:{accountSha256:seed.profiles[MAC_PROFILE].accountSha256, orgSha256:seed.profiles[MAC_PROFILE].orgSha256},
    records:records.map((row: any) => ({name:row.name, sha256:row.sha256, base64:row.bytes.toString('base64')})),
    revision:core.snapshotRevision(records), snapshotStable:true, endpoint:seed.endpoint,
    nativeGuard:{...core.NATIVE[platform], warmGuardVerified:true, autoResumeGuardVerified:true},
    noPendingInput:true, noScheduledWork:true, protectedSnapshotStable:true};
}
const transportModule = await import(path.join(base, 'src/web-server/services/claude-desktop-transport.ts'));
const transportMocks = {
  openClaudeMacLauncher: async () => {opened++;},
  openClaudeWindowsLauncher: async () => {opened++;},
  runClaudeHistoryHelper: async (_launcher: any, platform: string, _id: string, request: any) => {
    let out: any;
    if(request.mode === 'closed-check') out = {closed};
    else if(request.mode === 'collect') out = snapshot(platform);
    else if(request.mode === 'verify-transcripts') out = {verified:true};
    else if(request.mode === 'protected-check') out = {unchanged:true};
    else if(request.mode === 'append') {
      appendCalls++;
      if(behavior === 'lost') throw new Error(canary);
      if(behavior === 'malformed') return Buffer.from('{}');
      out = behavior === 'terminal-refusal'
        ? {status:'refused', createdCount:0, recoveryRequired:true, writerQuiescent:true}
        : {status:'created_metadata', createdCount:1, protectedBytesUnchanged:true, writerQuiescent:true};
    } else throw new Error(canary);
    return Buffer.from(JSON.stringify(out));
  },
};
const {openClaudeDesktopProfile} = await import(path.join(base, 'src/web-server/services/claude-desktop-open-service.ts'));
const {claudeHistoryOpenHeld} = await import(path.join(base, 'src/web-server/services/claude-history-sync-service.ts'));
const policy = {version:1, enabled:true, sourcePlatform:'mac',
  identity:{accountUuid:seed.profiles[MAC_PROFILE].accountUuid, organizationUuid:seed.profiles[MAC_PROFILE].organizationUuid},
  project:{cwd:seed.project, originCwd:seed.project, transcriptRoot:seed.transcriptRoot},
  ssh:Object.fromEntries(['mac','windows'].map(platform => [platform, {alias:seed.aliases[platform], ...seed.plainEndpoint}]))};
function write(policyPresent = true) {
  const row: any = {id:MAC_PROFILE, email:'synthetic@example.com',
    mac:{launcherName:'Synthetic.app',launcherPath:'/Users/synthetic/Synthetic.app',profilePath:'/Users/synthetic/Claude',sshHost:'synthetic-mac'},
    windows:{launcherName:'Synthetic.lnk',launcherPath:'C:\\Synthetic.lnk',profilePath:'C:\\Claude',sshHost:'synthetic-windows'}};
  if(policyPresent) row.historySync = policy;
  fs.writeFileSync(path.join(directory, 'claude-desktop-profiles.json'), JSON.stringify({version:1,profiles:[row]}), {mode:0o600});
}
beforeEach(() => {directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-pending-open-')); fs.chmodSync(directory,0o700); oldDir=process.env.CCS_DIR;process.env.CCS_DIR=directory; opened=appendCalls=0;behavior='lost';closed=true;write();
  spyOn(transportModule, 'runClaudeHistoryHelper').mockImplementation(transportMocks.runClaudeHistoryHelper);
  spyOn(transportModule, 'openClaudeMacLauncher').mockImplementation(transportMocks.openClaudeMacLauncher);
  spyOn(transportModule, 'openClaudeWindowsLauncher').mockImplementation(transportMocks.openClaudeWindowsLauncher);
});
afterEach(() => {mock.restore();if(oldDir === undefined) delete process.env.CCS_DIR;else process.env.CCS_DIR=oldDir;fs.rmSync(directory,{recursive:true,force:true});});

test('lost append blocks ordinary Open and leaves a private durable marker', async () => {
  await expect(openClaudeDesktopProfile(MAC_PROFILE,'windows')).rejects.toThrow('history copy is unconfirmed');
  expect(appendCalls).toBe(1);expect(opened).toBe(0);expect(claudeHistoryOpenHeld(MAC_PROFILE,'windows')).toBe(true);
  expect(fs.readFileSync(path.join(directory,'claude-desktop-profiles.json'),'utf8')).not.toContain(canary);
});
test('second click and policy removal cannot bypass prior lost append', async () => {
  await expect(openClaudeDesktopProfile(MAC_PROFILE,'windows')).rejects.toThrow();write(false);
  await expect(openClaudeDesktopProfile(MAC_PROFILE,'windows')).rejects.toThrow();
  expect(appendCalls).toBe(1);expect(opened).toBe(0);
});
test('malformed successful SSH response keeps hold and does not open', async () => {
  behavior='malformed';await expect(openClaudeDesktopProfile(MAC_PROFILE,'windows')).rejects.toThrow();
  expect(appendCalls).toBe(1);expect(opened).toBe(0);expect(claudeHistoryOpenHeld(MAC_PROFILE,'windows')).toBe(true);
});
test('trusted completed response permits one normal Open and keeps manifest unchanged', async () => {
  behavior='success';const original=fs.readFileSync(path.join(directory,'claude-desktop-profiles.json'));
  await openClaudeDesktopProfile(MAC_PROFILE,'windows');expect(appendCalls).toBe(1);expect(opened).toBe(1);
  expect(claudeHistoryOpenHeld(MAC_PROFILE,'windows')).toBe(false);
  expect(fs.readFileSync(path.join(directory,'claude-desktop-profiles.json'))).toEqual(original);
});
test('trusted terminal failure is quiescent without a zero-write claim and still permits Open', async () => {
  behavior='terminal-refusal';await openClaudeDesktopProfile(MAC_PROFILE,'windows');
  expect(appendCalls).toBe(1);expect(opened).toBe(1);expect(claudeHistoryOpenHeld(MAC_PROFILE,'windows')).toBe(false);
});
test('initial missing policy preserves normal Open and no marker', async () => {
  write(false);await openClaudeDesktopProfile(MAC_PROFILE,'windows');expect(opened).toBe(1);expect(appendCalls).toBe(0);
  expect(fs.existsSync(path.join(directory,'claude-history-pending'))).toBe(false);
});
test('fresh destination-open refusal preserves normal Open without appending', async () => {
  closed=false;await openClaudeDesktopProfile(MAC_PROFILE,'windows');expect(opened).toBe(1);expect(appendCalls).toBe(0);
});
test('unavailable private marker storage skips new copy safely without changing privacy', async () => {
  fs.chmodSync(directory,0o755);await openClaudeDesktopProfile(MAC_PROFILE,'windows');
  expect(opened).toBe(1);expect(appendCalls).toBe(0);expect(fs.statSync(directory).mode & 0o777).toBe(0o755);
});
test('unknown foreign marker refuses Open even with absent policy and preserves bytes', async () => {
  write(false);const root=path.join(directory,'claude-history-pending');fs.mkdirSync(root,{mode:0o700});
  const filename=path.join(root,'foreign');fs.writeFileSync(filename,'FOREIGN',{mode:0o600});
  await expect(openClaudeDesktopProfile(MAC_PROFILE,'windows')).rejects.toThrow();
  expect(opened).toBe(0);expect(appendCalls).toBe(0);expect(fs.readFileSync(filename,'utf8')).toBe('FOREIGN');
});
