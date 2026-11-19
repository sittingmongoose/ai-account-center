/**
 * M4b claude-host parity table (lane F). Generates `claude-host-table.json` from the REAL
 * reference builders: `windowsHostScript` for every operation and value shape, the Open
 * commands (`macOpenCommand`, `windowsOpenCommand`), the account-list script, the fixed
 * Windows bootstrap and the TS-issued purge permits. The Rust port
 * (`crates/aac-agent/src/claude_host*`) compares byte for byte against it.
 *
 *   bun tests/spec/agent/claude-host-table.ts          # rewrite the checked-in table
 *   bun tests/spec/agent/claude-host-table.ts --check  # fail when it drifted
 *
 * Nothing runs on a host here: only the pure builders and the permit HMAC.
 */
import fs from 'fs';
import path from 'path';
import {
  windowsHostScript,
  type ClaudeHostRunner,
  SshClaudeHostTransport,
} from '../../../src/web-server/services/claude-host-transport';
import {
  macOpenCommand,
  windowsAccountListScript,
  windowsOpenCommand,
} from '../../../src/web-server/services/claude-desktop-transport';
import { claudeHostPurgePermit } from '../../../src/web-server/services/agent-verbs';
import type { ClaudeHostLauncher } from '../../../src/web-server/services/claude-account-stores';

const base: ClaudeHostLauncher = {
  launcherName: 'Claude (party)',
  launcherPath: 'C:\\Users\\x\\Desktop\\Claude (party@example.com).lnk',
  startMenuPath:
    'C:\\Users\\x\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Claude (party).lnk',
  profilePath: 'C:\\Users\\x\\AppData\\Roaming\\Claude-party',
  sshHost: 'windows-host',
};
const wideUser = '\u5f20'.repeat(200);
const wide: ClaudeHostLauncher = {
  ...base,
  launcherPath: `C:\\Users\\${wideUser}\\Desktop\\Claude (party).lnk`,
  startMenuPath: `C:\\Users\\${wideUser}\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Claude (party).lnk`,
  profilePath: `C:\\Users\\${wideUser}\\AppData\\Roaming\\Claude-party`,
};
const jose: ClaudeHostLauncher = {
  ...base,
  profilePath: 'C:\\Users\\José\\AppData\\Roaming\\Claude-party',
};
const noStartMenu: ClaudeHostLauncher = { ...base };
delete noStartMenu.startMenuPath;

const trashName = 'party-20261002T080000Z';
type Op = 'create' | 'undo' | 'state' | 'session' | 'trash' | 'restore' | 'purge';

function attempt(fn: () => string): string | { error: true } {
  try {
    return fn();
  } catch {
    return { error: true };
  }
}

const scripts: unknown[] = [];
for (const [label, launcher] of [
  ['base', base],
  ['wide', wide],
  ['jose', jose],
  ['noStartMenu', noStartMenu],
] as const) {
  const valuesByOp: Record<Op, { profileId?: string; trashName?: string; launcher?: ClaudeHostLauncher }> = {
    create: { profileId: 'party' },
    undo: { profileId: 'party', launcher },
    state: { launcher },
    session: { launcher },
    trash: { profileId: 'party', trashName, launcher },
    restore: { profileId: 'party', trashName, launcher },
    purge: { trashName },
  };
  for (const [op, values] of Object.entries(valuesByOp)) {
    scripts.push({
      name: `${op}/${label}`,
      op,
      values: JSON.parse(JSON.stringify(values)),
      expected: attempt(() => windowsHostScript(op as Op, values)),
    });
  }
}
// The spec's refusal cases: quotes, variables, typographic quotes, controls, bad names.
const unsafePaths = [
  "C:\\x\\Claude-a'; Remove-Item C:\\",
  'C:\\x\\$env:TEMP',
  'C:\\x\\`n',
  'C:\\x\\Claude-a\u2018; Remove-Item C:\\',
  'C:\\x\\Claude-a\u2019; Remove-Item C:\\',
  'C:\\x\\Claude-a\u201a; exit',
  'C:\\x\\Claude-a\u201b; exit',
  'C:\\x\\Claude-a\u201c\u201d',
  'C:\\x\\Claude\r\nexit',
];
for (const [i, profilePath] of unsafePaths.entries()) {
  scripts.push({
    name: `unsafe-path/${i}`,
    op: 'state',
    values: { launcher: { ...base, profilePath } },
    expected: attempt(() => windowsHostScript('state', { launcher: { ...base, profilePath } })),
  });
}
scripts.push(
  {
    name: 'bad-profile-id',
    op: 'create',
    values: { profileId: "x'; exit" },
    expected: attempt(() => windowsHostScript('create', { profileId: "x'; exit" })),
  },
  {
    name: 'bad-trash-name',
    op: 'purge',
    values: { trashName: '..\\..\\Windows' },
    expected: attempt(() => windowsHostScript('purge', { trashName: '..\\..\\Windows' })),
  },
  {
    name: 'short-trash-stamp',
    op: 'trash',
    values: { profileId: 'party', trashName: 'party-20261002T08000Z', launcher: base },
    expected: attempt(() =>
      windowsHostScript('trash', { profileId: 'party', trashName: 'party-20261002T08000Z', launcher: base })
    ),
  }
);

const longIds = Array.from({ length: 64 }, (_, i) => `profile${i}`.padEnd(63, 'x'));
longIds[0] = 'party';
const open: unknown[] = [
  { name: 'mac/plain', kind: 'mac', launcherPath: '/Users/u/Applications/Claude (work2).app', expected: attempt(() => macOpenCommand('/Users/u/Applications/Claude (work2).app')) },
  { name: 'mac/quote', kind: 'mac', launcherPath: "/Users/u/Applications/Claude (it's).app", expected: attempt(() => macOpenCommand("/Users/u/Applications/Claude (it's).app")) },
  { name: 'mac/relative', kind: 'mac', launcherPath: 'relative.app', expected: attempt(() => macOpenCommand('relative.app')) },
  { name: 'mac/not-app', kind: 'mac', launcherPath: '/Users/u/Applications/Claude.appx', expected: attempt(() => macOpenCommand('/Users/u/Applications/Claude.appx')) },
  { name: 'mac/control', kind: 'mac', launcherPath: '/x/\u0001.app', expected: attempt(() => macOpenCommand('/x/\u0001.app')) },
  { name: 'mac/undefined', kind: 'mac', launcherPath: null, expected: attempt(() => macOpenCommand(undefined)) },
  { name: 'windows/plain', kind: 'windows', profileId: 'party', accountList: null, expected: attempt(() => windowsOpenCommand('party')) },
  { name: 'windows/list-first', kind: 'windows', profileId: 'party', accountList: ['store', 'party'], expected: attempt(() => windowsOpenCommand('party', ['store', 'party'])) },
  { name: 'windows/list-reordered', kind: 'windows', profileId: 'party', accountList: ['party', 'store'], expected: attempt(() => windowsOpenCommand('party', ['party', 'store'])) },
  { name: 'windows/list-without-profile', kind: 'windows', profileId: 'party', accountList: ['store'], expected: attempt(() => windowsOpenCommand('party', ['store'])) },
  { name: 'windows/list-invalid', kind: 'windows', profileId: 'party', accountList: ['bad id', 'party'], expected: attempt(() => windowsOpenCommand('party', ['bad id', 'party'])) },
  { name: 'windows/list-too-long', kind: 'windows', profileId: 'party', accountList: longIds, expected: attempt(() => windowsOpenCommand('party', longIds)) },
  { name: 'windows/bad-id', kind: 'windows', profileId: 'bad id', accountList: null, expected: attempt(() => windowsOpenCommand('bad id')) },
];

const accountList: unknown[] = [
  { name: 'two', ids: ['store', 'work2'], expected: attempt(() => windowsAccountListScript(['store', 'work2'])) },
  { name: 'empty', ids: [], expected: attempt(() => windowsAccountListScript([])) },
  { name: 'duplicate', ids: ['a', 'a'], expected: attempt(() => windowsAccountListScript(['a', 'a'])) },
  { name: 'bad-id', ids: ['bad id'], expected: attempt(() => windowsAccountListScript(['bad id'])) },
  { name: 'too-many', ids: Array.from({ length: 65 }, (_, i) => `p${i}`), expected: attempt(() => windowsAccountListScript(Array.from({ length: 65 }, (_, i) => `p${i}`))) },
];

const now = 1_700_000_000_000;
const permits = [
  { host: 'windows', trashName, now, token: claudeHostPurgePermit('windows', trashName, now) },
  { host: 'mac', trashName, now, token: claudeHostPurgePermit('mac', trashName, now) },
  { host: 'windows', trashName: 'other-20261002T080000Z', now, token: claudeHostPurgePermit('windows', 'other-20261002T080000Z', now) },
];

// The production transport's fixed command lines, captured through a fake runner (the
// verbs stay off here, so the agent attempt delegates and the bootstrap is what runs).
const calls: Array<{ command: string; input: string }> = [];
const fakeRunner: ClaudeHostRunner = async (_host, command, input) => {
  calls.push({ command, input });
  return '{"running":false}';
};
await new SshClaudeHostTransport(fakeRunner).appState('windows', { launcher: base });
await new SshClaudeHostTransport(fakeRunner).appState('mac', { launcher: base });

const table = {
  note: 'Generated by tests/spec/agent/claude-host-table.ts from the reference builders. Do not edit.',
  bootstrap: {
    windows: calls[0].command,
    captured: calls.map((c) => c.command),
  },
  scripts,
  open,
  accountList,
  permits,
};

/** The SAFE_PATH L/M/N ranges, from the same engine that defines the TS allowlist. */
function categoryRanges(re: RegExp): Array<[number, number]> {
  const out: Array<[number, number]> = [];
  let start = -1;
  for (let cp = 0; cp <= 0x10ffff; cp++) {
    const hit = re.test(String.fromCodePoint(cp));
    if (hit && start < 0) start = cp;
    if (!hit && start >= 0) {
      out.push([start, cp - 1]);
      start = -1;
    }
  }
  if (start >= 0) out.push([start, 0x10ffff]);
  return out;
}

function categoriesSource(): string {
  const fmt = (name: string, ranges: Array<[number, number]>) =>
    `pub(crate) const ${name}: &[(u32, u32)] = &[\n` +
    ranges.map(([a, b]) => `    (0x${a.toString(16)}, 0x${b.toString(16)}),\n`).join('') +
    '];\n';
  return (
    '//! Unicode L/M/N category ranges for the SAFE_PATH port, generated from Node (the engine\n' +
    '//! that defines the TS spec). Regenerate: bun tests/spec/agent/claude-host-table.ts.\n' +
    fmt('LETTER', categoryRanges(/\p{L}/u)) +
    fmt('MARK', categoryRanges(/\p{M}/u)) +
    fmt('NUMBER', categoryRanges(/\p{N}/u))
  );
}

const categoriesTarget = path.join(
  import.meta.dir,
  '../../../crates/aac-agent/src/claude_host/categories.rs'
);
const target = path.join(import.meta.dir, 'claude-host-table.json');
const text = JSON.stringify(table, null, 2) + '\n';
const categoriesText = categoriesSource();
if (process.argv.includes('--check')) {
  if (fs.readFileSync(target, 'utf8') !== text) {
    console.error('claude-host-table.json drifted; run: bun tests/spec/agent/claude-host-table.ts');
    process.exit(1);
  }
  if (fs.readFileSync(categoriesTarget, 'utf8') !== categoriesText) {
    console.error('categories.rs drifted; run: bun tests/spec/agent/claude-host-table.ts');
    process.exit(1);
  }
  console.log(`claude-host-table: ${scripts.length} scripts, ${open.length} open commands current`);
} else {
  fs.writeFileSync(target, text);
  fs.writeFileSync(categoriesTarget, categoriesText);
  console.log(`claude-host-table: wrote ${scripts.length} scripts, ${open.length} open commands`);
}
