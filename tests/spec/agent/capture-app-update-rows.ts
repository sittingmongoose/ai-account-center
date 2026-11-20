/**
 * Capture the Update apps row contract from the legacy dashboard before the cut.
 * The fixture is independent of the TS implementation after it is captured.
 */
import { normalizeAppUpdateRow, type UpdateAppId, type UpdatePlatform } from '../../../src/web-server/services/app-update-contract';
import { HOST_OS } from '../../../src/web-server/services/dashboard-hosts';

const cases: [UpdatePlatform, UpdateAppId, Record<string, unknown>][] = [
  ['ubuntu', 'antigravity-cli', { status:'held', messageCode:'held_for_review', heldVersion:'1.5.9', version:'1.5.8', manager:'native' }],
  ['windows', 't3-acp-adapters', { status:'action_required', messageCode:'in_use', manager:'npm', parts:[
    { name:'muse-acp', previousVersion:'0.21.0', version:'0.21.0', inUse:true },
    { name:'zcode-acp-server', previousVersion:'0.84.2', version:'0.84.3' },
  ] }],
  ['mac', 'claude-code', { status:'action_required', messageCode:'older_copy_first', version:'2.4.1', manager:'native', strays:[
    { location:'homebrew', version:'2.3.9', shadows:true },
    { location:'bun', version:'2.1.0', shadows:false },
  ] }],
  ['nas1', 't3-code', { status:'updated', messageCode:'t3_restart_scheduled', version:'0.1.10-nightly.20261113.4616', manager:'native', restartTargets:[
    { kind:'systemd', service:'t3code.service', delaySeconds:30 },
  ] }],
  ['alexis', 'cursor-cli', { status:'action_required', messageCode:'policy_blocked', manager:'unsupported', diagnostics:{
    cause:'policy_blocked', reason:'execution_policy', tool:'powershell', exitCode:-1,
    phases:{ checking:123 }, owner:{ manager:'native', executable:'~/.local/bin/cursor-agent', copies:2, onPath:true },
    notes:['policy_version_from_folder'], tail:'fixture-scrub', command:'fixture-scrub',
  } }],
  ['windows', 'codex-desktop', { status:'updated', messageCode:'desktop_reopened', manager:'msix', previousVersion:'26.1101.9.0',
    version:'26.1113.36452.0', updateAttempted:true, restartedProcesses:1, forcedStops:1, restartTargets:[{ kind:'desktop' }] }],
  ['mac', 'claude-desktop', { status:'skipped', messageCode:'skipped_cancelled' }],
  ['nas1', 'omp', { status:'unknown', messageCode:'host_timeout' }],
  ['ubuntu', 'antigravity-acp', { status:'action_required', messageCode:'antigravity_acp_signin_hold', heldVersion:'1.5.3' }],
  ['alexis', 'claude-code', { status:'updated', messageCode:'unlisted', version:'2.4.1', manager:'native' }],
  ['ubuntu', 'muse-code', {
    status: 'updated', messageCode: 'muse_quota_exhausted', previousVersion: '1.4.2',
    version: '1.5.0', manager: 'native', updateAttempted: true, quotaReset: '2026-11-20 05:00 UTC',
  }],
  ['mac', 'muse-code', {
    status: 'current', messageCode: 'muse_quota_current', previousVersion: '1.4.2',
    version: '1.4.2', manager: 'native', updateAttempted: true, quotaReset: '2026-11-20 05:00 UTC',
  }],
  ['windows', 'muse-code', {
    status: 'updated', messageCode: 'muse_quota_exhausted', version: '1.5.0', manager: 'native',
  }],
  ['nas1', 'muse-code', {
    status: 'updated', messageCode: 'muse_quota_exhausted', version: '1.5.0', manager: 'native',
    quotaReset: '5 pm\nquota',
  }],
  ['odroid', 't3-code', {
    status: 'updated', messageCode: 't3_restart_scheduled', version: '0.1.10-nightly.20261113.4616',
    manager: 'native', restartTargets: [{ kind: 'systemd', service: 't3code.service', delaySeconds: 30 }],
  }],
  ['ubuntu', 'antigravity-acp', {
    status: 'updated', messageCode: 'antigravity_acp_qualified', version: '1.4.6', manager: 'official-download',
  }],
  ['mac', 'antigravity-acp', {
    status: 'updated', messageCode: 'antigravity_acp_isolation_unverified', version: '1.4.6',
    manager: 'official-download', wrapperVersion: '1.4.5', qualifyReason: 'bwrap is missing',
  }],
  ['windows', 'antigravity-acp', {
    status: 'updated', messageCode: 'antigravity_acp_isolation_unverified', version: '1.4.6',
    manager: 'official-download', wrapperVersion: null,
  }],
  ['nas1', 'antigravity-acp', {
    status: 'updated', messageCode: 'antigravity_acp_isolation_unverified', version: '1.4.6',
    manager: 'official-download', wrapperVersion: 'invalid version',
  }],
];
const golden = cases.map(([host, app, facts]) => {
  const os = HOST_OS[host];
  const row = { appId:app, platform: os === 'linux' ? 'ubuntu' : os, ...facts };
  return { host, app, row, expected: normalizeAppUpdateRow(row, app, host) };
});
const bytes = JSON.stringify(golden, null, 2) + '\n';
const output = new URL('./app-update-rows.json', import.meta.url);
if (process.argv.includes('--regenerate')) await Bun.write(output, bytes);
else if (bytes !== await Bun.file(output).text()) throw new Error('App update row fixture drift');
console.log(golden.length + ' app-update reference rows');
