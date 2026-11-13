/**
 * Owner decision U13: installer output and recorded commands are never saved. Every witness the
 * H1 rechecks found (rounds 1-4, including the H1-R6 ANSI title witnesses) runs through the real
 * Python command() and Diagnostics.export(), the dashboard's normalizeAppUpdateRow, a service job,
 * both saved job files and a fresh service's restoration; no output or command text may reach any
 * of them. The cause is still classified from the output in memory, and the tool, exit code and
 * cause are kept.
 */
import { afterEach, expect, it } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {
  normalizeAppUpdateRow,
  UPDATE_APP_LABELS,
} from '../../../src/web-server/services/app-update-contract';
import {
  APP_UPDATE_TOOLS,
  CREDENTIAL_FLAGS,
  CREDENTIAL_MARKERS,
  normalizeDiagnostics,
  scrubText,
} from '../../../src/web-server/services/app-update-diagnostics';
import { AppUpdateService } from '../../../src/web-server/services/app-update-service';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

function tempRoot(prefix: string): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  roots.push(root);
  return root;
}

const AFTER = 'update step finished';
const REMOVED = '<line removed: may contain a credential>';
const REST = '<rest of output removed: may contain a credential>';
const ESC = '\x1b';
const BEL = '\x07';
/** The only fields a row's diagnostics may keep (owner decision U13). */
const KEPT_FIELDS = ['cause', 'reason', 'tool', 'exitCode', 'phases', 'owner', 'notes'];
const WITNESSES = [
  'horse',
  'battery',
  'staple',
  'correctHorseWitness',
  'continuationWitness',
  'zebraWitness',
  'aaaaaaaaaa',
  'fail.py',
  'dXNlcjpmaXh0dXJl',
  'fixtureCookieOne',
  'fixtureCookieTwo',
  'second third',
  'foldedWitness',
  'lateMarkerWitness',
  'lastWitness',
  'xxxxxxxx',
];

const H1_FLAGS = [
  '--password',
  '--passwd',
  '--pass',
  '--token',
  '--api-key',
  '--apikey',
  '--auth',
  '--auth-token',
  '--authtoken',
  '--secret',
  '--access-token',
  '--accesstoken',
  '--otp',
];

// C1 (max review 2): credential carriers and their continuations.
const C1_SAMPLES = [
  '{"//registry.example.test/:_auth":"dXNlcjpmaXh0dXJl"}',
  'Cookie: SID=fixtureCookieOne;\n\tHSID=fixtureCookieTwo',
  'installer failed --password "first second third"',
  'token=' + 'x'.repeat(70000) + '\n    continuationWitness',
  'x'.repeat(700) + ' api_key=lateMarkerWitness',
];

/** H1: every flag the token pass redacts, both dash forms, quoted and unquoted, '=' and space. */
function aliasSamples(): string[] {
  const samples: string[] = [];
  for (const flag of [...H1_FLAGS, ...H1_FLAGS.map((flag) => flag.slice(1))])
    for (const value of [
      '"correct horse battery staple"',
      "'correct horse battery staple'",
      'correctHorseWitness',
    ])
      samples.push(`installer ${flag} ${value}`, `installer ${flag}=${value}`);
  return [
    ...samples,
    'installer -p "correct horse battery staple"',
    'installer -p="correct horse battery staple"',
    "installer -p'correct horse battery staple'",
    'installer --pass \\\n  continuationWitness',
  ];
}

// RECHECK-H1: multi-line values after a flag.
const H1_MULTILINE = [
  `installer --pass "correct\nhorse battery staple"\n${AFTER}`,
  `installer --pass 'correct\nhorse battery staple'\n${AFTER}`,
  `installer -OTP 'correct\nhorse battery\nstaple'\n${AFTER}`,
  `installer -OTP "correct\nhorse battery staple"\n${AFTER}`,
  `installer --password "correct\nhorse battery staple"\n${AFTER}`,
  `installer --password 'correct\nhorse battery staple'\n${AFTER}`,
  `installer --pass "correct\\"\nhorse battery staple"\n${AFTER}`,
  `installer --pass "correct\nhorse battery staple\n${AFTER}`,
  `installer -OTP 'correct\nhorse\nbattery staple\n${AFTER}`,
  'installer --password "correct\nhorse battery staple',
];

// RECHECK-H1-2: 24 output carriers.
const H1_PROBE: Array<[string, string]> = [
  ['R1 dq', `installer --pass "correct\nhorse battery staple"\n${AFTER}`],
  ['R1 sq OTP', `installer -OTP 'correct\nhorse battery\nstaple'\n${AFTER}`],
  ['R1 never closed', `installer --password "correct\nhorse battery staple\n${AFTER}`],
  ['R1 escaped dq', `installer --pass "correct\\"\nhorse battery staple"\n${AFTER}`],
  ['CRLF dq', `installer --pass "correct\r\nhorse battery staple"\r\n${AFTER}\r\n`],
  ['CRLF sq eq', `installer --pass='correct\r\nhorse battery staple'\r\n${AFTER}\r\n`],
  ['lone CR', `installer --pass "correct\rhorse battery staple"\r${AFTER}`],
  ['mixed apostrophe in dq', `installer --pass "it's correct\nhorse battery staple"\n${AFTER}`],
  ['mixed dq in sq', `installer --pass 'say "hi\nhorse battery staple'\n${AFTER}`],
  ['mixed concat', `installer --pass "correct"'x\nhorse battery staple'\n${AFTER}`],
  ['eq dq', `installer --pass="correct\nhorse battery staple"\n${AFTER}`],
  ['space sq', `installer --token 'correct\nhorse battery staple'\n${AFTER}`],
  [
    'close later + more text',
    `installer --pass "correct\nhorse battery staple" --verbose ok\n${AFTER}`,
  ],
  ['close later 3 lines', `installer --secret 'a\nb\nhorse battery staple' then\n${AFTER}`],
  ['quote before flag dq', `installer "--pass=correct\nhorse battery staple"\n${AFTER}`],
  ['quote before flag sq', `installer '--password=correct\nhorse battery staple'\n${AFTER}`],
  ['argv list', `Running ["installer", "--pass", "correct\nhorse battery staple"]\n${AFTER}`],
  [
    'earlier quoted marker',
    `installer: "auth" step: --pass "correct\nhorse battery staple"\n${AFTER}`,
  ],
  [
    'earlier quoted session',
    `error: "session expired"; retry --otp "correct\nhorse battery staple"\n${AFTER}`,
  ],
  ['posix escaped sq outside', `installer --pass it\\'s' correct\nhorse battery staple'\n${AFTER}`],
  ['bash sq idiom', `installer --pass 'it'\\''s correct\nhorse battery staple'\n${AFTER}`],
  ['powershell backtick', `installer --pass "correct\`" x\nhorse battery staple"\n${AFTER}`],
  ['unquoted newline (dash -x)', `+ installer --pass correct\nhorse battery staple\n${AFTER}`],
  ['marker password colon', `password: "correct\nhorse battery staple"\n${AFTER}`],
];

// RECHECK-H1-4 (H1-R6): an ANSI title (OSC) that holds the flag, and a coloured flag.
const H1_OSC: Array<[string, string]> = [
  ['osc newline', `${ESC}]0;installer --pass "correct\nhorse battery staple"${BEL}\n${AFTER}`],
  ['osc crlf otp', `${ESC}]0;installer --otp "correct\r\nhorse battery staple"${BEL}\r\n${AFTER}`],
  [
    'osc over 512',
    `${ESC}]0;installer --pass ${'a'.repeat(520)}horse battery staple${BEL}\n${AFTER}`,
  ],
  [
    'sgr coloured flag',
    `installer ${ESC}[31m--pass${ESC}[0m "correct horse battery staple"\n${AFTER}`,
  ],
];

// RECHECK-H1-2 and -3: argv values (line breaks, plain, long slash '=' values).
const ARGV_CASES: string[][] = [
  ['--pass', 'correct\nhorse battery staple'],
  ['--pass', 'correct\r\nhorse battery staple'],
  ['--pass', 'zebraWitness'],
  ['--pass=/' + 'horse battery staple/'.repeat(12)],
  ['--password=/' + 'horse battery staple/'.repeat(12)],
  ['--verbose', 'correctHorseWitness'],
];

interface HelperRun {
  row: Record<string, unknown>;
  /** Whether the raw output was still in memory for classification. */
  inMemory: boolean;
}

/** Real Python command() and Diagnostics.export() for each sample, as stderr or as argv. */
function helperRows(root: string, samples: unknown[], mode: 'out' | 'argv'): HelperRun[] {
  const script = `import json,pathlib,sys
from unittest import mock
sys.path.insert(0, sys.argv[1])
import app_update_common as c
runs=[]
child = pathlib.Path(sys.argv[2]) / 'fail.py'
child.write_text('import sys; sys.exit(1)')
with mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(sys.argv[2])):
    for sample in json.load(sys.stdin):
        output = ''
        with c.row_diagnostics() as record:
            try:
                if sys.argv[3] == 'argv':
                    c.command([sys.executable, str(child), *sample], errors=True)
                else:
                    c.command([sys.executable, '-c', 'import sys; sys.stderr.write('+repr(sample)+'); sys.exit(1)'],
                              errors=True, label='fixture updater')
            except c.UpdateFailure as error:
                output = error.output or ''
        row=c.result('t3-acp-adapters','mac','failed',code='update_failed')
        row['diagnostics']=record.export(row)
        probe = sample if isinstance(sample, str) else ''
        runs.append({'row': row, 'inMemory': sys.argv[3] == 'argv' or probe[:20] in output})
print(json.dumps(runs))`;
  const child = Bun.spawnSync(
    ['python3', '-c', script, path.join(process.cwd(), 'scripts/app-updates'), root, mode],
    { stdin: Buffer.from(JSON.stringify(samples)), env: { ...process.env, CCS_HOME: root } }
  );
  if (child.exitCode !== 0) console.log(child.stderr.toString());
  expect(child.exitCode).toBe(0);
  return JSON.parse(child.stdout.toString()) as HelperRun[];
}

/** Runs one job whose Mac adapter row is `raw`; every place it lands, as text, plus the restored row. */
async function savedPaths(storage: string, raw: Record<string, unknown>) {
  const service = new AppUpdateService({
    ccsDir: storage,
    sync: async () => {},
    runHost: async (platform) =>
      JSON.stringify({
        results: Object.keys(UPDATE_APP_LABELS).map((appId) =>
          appId === 't3-acp-adapters' && platform === 'mac'
            ? raw
            : {
                appId,
                status: 'not_installed',
                messageCode: 'not_installed',
                previousVersion: null,
                version: null,
                manager: null,
                updateAttempted: false,
                restartedProcesses: 0,
              }
        ),
      }),
  });
  service.start();
  for (let tries = 0; tries < 100 && service.getStatus().job?.state === 'running'; tries++)
    await new Promise((resolve) => setTimeout(resolve, 0));
  expect(service.getStatus().job?.state).toBe('failed');
  const job = JSON.stringify(service.getStatus().job);
  const saved = new AppUpdateService({ ccsDir: storage }).getStatus().job!;
  const files = ['dashboard-job.json', 'dashboard-job-expanded.json'].map((file) =>
    fs.readFileSync(path.join(storage, 'app-updates', file), 'utf8')
  );
  const restored = saved.results.find(
    (row) => row.appId === 't3-acp-adapters' && row.platform === 'mac'
  )!;
  return { texts: [job, ...files, JSON.stringify(saved)], restored };
}

/** No output or command text in any of these; only the fixed fields remain. */
function expectNothingKept(name: string, texts: string[], diagnostics: unknown, tool: string) {
  const kept = diagnostics as Record<string, unknown>;
  expect([name, Object.keys(kept).filter((key) => !KEPT_FIELDS.includes(key))]).toEqual([name, []]);
  expect([name, kept.cause, kept.tool, kept.exitCode]).toEqual([name, 'installer_failed', tool, 1]);
  for (const text of texts)
    for (const witness of WITNESSES)
      expect([name, witness, text.includes(witness)]).toEqual([name, witness, false]);
  for (const text of texts)
    expect([name, /"(?:tail|command)"\s*:/.test(text)]).toEqual([name, false]);
}

it('H1 the credential and tool definitions match the Python helper', () => {
  const child = Bun.spawnSync([
    'python3',
    '-c',
    'import json,sys; sys.path.insert(0, sys.argv[1]); import app_update_common as c; ' +
      'print(json.dumps([list(c.CREDENTIAL_FLAGS), list(c.CREDENTIAL_MARKERS), list(c.TOOLS)]))',
    path.join(process.cwd(), 'scripts/app-updates'),
  ]);
  expect(child.exitCode).toBe(0);
  expect(JSON.parse(child.stdout.toString())).toEqual([
    [...CREDENTIAL_FLAGS],
    [...CREDENTIAL_MARKERS],
    APP_UPDATE_TOOLS.filter((tool) => tool !== 'unlisted'),
  ]);
  const flag = new RegExp(`^--?(?:${CREDENTIAL_FLAGS.join('|')})$`, 'i');
  for (const alias of H1_FLAGS) expect(flag.test(alias)).toBe(true);
  expect(CREDENTIAL_MARKERS).toContain(`--?(?:${CREDENTIAL_FLAGS.join('|')})\\b`);
});

it('U13 no output from any H1 probe reaches command, export, normalization, the job, either file or restoration', async () => {
  const root = tempRoot('aac-u13-output-');
  const named: Array<[string, string]> = [
    ...C1_SAMPLES.map((sample, index): [string, string] => [`C1 ${index}`, sample]),
    ...aliasSamples().map((sample): [string, string] => [`alias ${sample}`, sample]),
    ...H1_MULTILINE.map((sample, index): [string, string] => [`multiline ${index}`, sample]),
    ...H1_PROBE,
    ...H1_OSC,
  ];
  const runs = helperRows(
    root,
    named.map(([, sample]) => sample),
    'out'
  );
  expect(runs.length).toBe(named.length);
  for (const [index, { row, inMemory }] of runs.entries()) {
    const [name] = named[index];
    // Classified from the output in memory, then discarded.
    expect([name, inMemory]).toEqual([name, true]);
    const normalized = normalizeAppUpdateRow(row, 't3-acp-adapters', 'mac');
    const { texts, restored } = await savedPaths(path.join(root, String(index)), row);
    expectNothingKept(name, [JSON.stringify(row)], row.diagnostics, 'unlisted');
    expectNothingKept(
      name,
      [JSON.stringify(normalized), ...texts],
      restored.diagnostics,
      'unlisted'
    );
  }
}, 180_000);

it('U13 no argument reaches any path; only the tool word stays', async () => {
  const root = tempRoot('aac-u13-argv-');
  const runs = helperRows(root, ARGV_CASES, 'argv');
  for (const [index, { row }] of runs.entries()) {
    const name = `argv ${ARGV_CASES[index][0].slice(0, 12)}`;
    const normalized = normalizeAppUpdateRow(row, 't3-acp-adapters', 'mac');
    const { texts, restored } = await savedPaths(path.join(root, String(index)), row);
    expectNothingKept(name, [JSON.stringify(row)], row.diagnostics, 'python');
    expectNothingKept(name, [JSON.stringify(normalized), ...texts], restored.diagnostics, 'python');
  }
}, 60_000);

it('U13 an older helper row with a command and tail keeps neither, on read and through restoration', async () => {
  const root = tempRoot('aac-u13-older-');
  const older = {
    appId: 't3-acp-adapters',
    status: 'failed',
    messageCode: 'update_failed',
    previousVersion: null,
    version: null,
    manager: 'npm',
    updateAttempted: true,
    restartedProcesses: 0,
    diagnostics: {
      cause: 'installer_failed',
      command: 'installer --pass correct\nhorse battery staple',
      exitCode: 1,
      tail: `${ESC}]0;installer --pass "correct\nhorse battery staple"${BEL}\n${AFTER}`,
      tool: 'not a tool --token zebraWitness',
    },
  };
  const normalized = normalizeAppUpdateRow(older, 't3-acp-adapters', 'mac');
  expect(normalized.diagnostics).toEqual({ cause: 'installer_failed', exitCode: 1 });
  const { texts, restored } = await savedPaths(root, older);
  expect(restored.diagnostics).toEqual({ cause: 'installer_failed', exitCode: 1 });
  for (const text of texts)
    for (const witness of [...WITNESSES, AFTER, 'installer --pass'])
      expect([witness, text.includes(witness)]).toEqual([witness, false]);
});

it('U13 a job saved before this change is rewritten without output on the next load', async () => {
  const root = tempRoot('aac-u13-saved-');
  const clean = {
    appId: 't3-acp-adapters',
    status: 'failed',
    messageCode: 'update_failed',
    previousVersion: null,
    version: null,
    manager: 'npm',
    updateAttempted: true,
    restartedProcesses: 0,
    diagnostics: { cause: 'installer_failed', tool: 'npm', exitCode: 1, phases: { update: 40 } },
  };
  await savedPaths(root, clean);
  const folder = path.join(root, 'app-updates');
  const expandedFile = path.join(folder, 'dashboard-job-expanded.json');
  // What an older package wrote: the same job with a command and output tail in its diagnostics.
  const saved = JSON.parse(fs.readFileSync(expandedFile, 'utf8'));
  for (const row of saved.job.results)
    if (row.appId === 't3-acp-adapters' && row.platform === 'mac')
      Object.assign(row.diagnostics, {
        command: 'npm install --token zebraWitness',
        tail: 'installer --pass "correct\nhorse battery staple"',
      });
  fs.writeFileSync(expandedFile, JSON.stringify(saved));
  expect(fs.readFileSync(expandedFile, 'utf8')).toContain('zebraWitness');
  const legacyBefore = fs.readFileSync(path.join(folder, 'dashboard-job.json'), 'utf8');

  const job = new AppUpdateService({ ccsDir: root }).getStatus().job!;
  const row = job.results.find((r) => r.appId === 't3-acp-adapters' && r.platform === 'mac')!;
  expect(row.diagnostics).toEqual({
    cause: 'installer_failed',
    tool: 'npm',
    exitCode: 1,
    phases: { update: 40 },
  });
  for (const file of ['dashboard-job.json', 'dashboard-job-expanded.json']) {
    const text = fs.readFileSync(path.join(folder, file), 'utf8');
    for (const witness of ['zebraWitness', 'horse', 'battery', 'staple'])
      expect([file, witness, text.includes(witness)]).toEqual([file, witness, false]);
    expect([file, /"(?:tail|command)"\s*:/.test(text)]).toEqual([file, false]);
  }
  // The rewritten pair still restores from the expanded file (its digest matches the legacy file).
  expect(fs.readFileSync(path.join(folder, 'dashboard-job.json'), 'utf8')).toBe(legacyBefore);
  const again = new AppUpdateService({ ccsDir: root }).getStatus().job!;
  expect(
    again.results.find((r) => r.appId === 't3-acp-adapters' && r.platform === 'mac')!.diagnostics
  ).toEqual(row.diagnostics);
  expect(fs.readdirSync(folder).filter((name) => name.endsWith('.lock'))).toEqual([]);
});

it('U13 a saved file with output that cannot be restored is removed on load', () => {
  const root = tempRoot('aac-u13-unrestorable-');
  const folder = path.join(root, 'app-updates');
  fs.mkdirSync(folder, { recursive: true });
  const expandedFile = path.join(folder, 'dashboard-job-expanded.json');
  fs.writeFileSync(
    expandedFile,
    JSON.stringify({
      legacyDigest: 'not a digest',
      job: { results: [{ diagnostics: { cause: 'unknown', tail: 'horse battery staple' } }] },
    })
  );
  expect(new AppUpdateService({ ccsDir: root }).getStatus().job).toBeNull();
  expect(fs.existsSync(expandedFile)).toBe(false);
});

it('U13 ordinary output is not kept either, but the cause, tool and exit code are', async () => {
  const root = tempRoot('aac-u13-control-');
  const control = 'installer: unpacking zcode-acp-server 0.84.3\nENOSPC: no space left on device';
  const [{ row, inMemory }] = helperRows(root, [control], 'out');
  expect(inMemory).toBe(true);
  const normalized = normalizeAppUpdateRow(row, 't3-acp-adapters', 'mac');
  expect(normalized.diagnostics).toEqual({
    cause: 'installer_failed',
    tool: 'unlisted',
    exitCode: 1,
  });
  const { texts } = await savedPaths(path.join(root, 'job'), row);
  for (const text of texts) expect(text).not.toContain('ENOSPC');
});

it('U13 the scrubbers stay as defence in depth for remaining free text', () => {
  for (const [name, sample] of [...H1_PROBE, ...H1_OSC])
    expect([name, scrubText(sample)]).toEqual([name, REST]);
  const line = 'installer failed: exit code=1; ENOSPC: no space left on device';
  expect(scrubText(line)).toBe(line);
  expect(scrubText('callback code=abc123def')).toBe(REMOVED);
  expect(scrubText('verification code=482913')).toBe(REMOVED);
  const ordinary =
    'installer: unpacking zcode-acp-server 0.84.3 (mkdir -p prefix; lsof -a -p 4242 found 0 holders)';
  expect(scrubText(ordinary)).toBe(ordinary);
  // Owner paths are the remaining free text: a flag in one, even inside an ANSI title, ends it.
  const owner = normalizeDiagnostics({
    cause: 'wrong_prefix',
    owner: {
      executable: `${ESC}]0;--pass ${BEL}horse/battery/staple`,
      packages: [
        {
          package: 'zcode-acp-server',
          manager: 'npm',
          prefix: '--pass=/' + 'horse battery staple/'.repeat(12),
          source: 't3-config',
          version: '0.84.2',
        },
      ],
    },
  });
  expect(JSON.stringify(owner)).not.toMatch(/horse|battery|staple/);
});
