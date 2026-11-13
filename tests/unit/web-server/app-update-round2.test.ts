import { afterEach, expect, it } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {
  normalizeAppUpdateRow,
  UPDATE_APP_LABELS,
} from '../../../src/web-server/services/app-update-contract';
import {
  CREDENTIAL_FLAGS,
  CREDENTIAL_MARKERS,
  scrubText,
} from '../../../src/web-server/services/app-update-diagnostics';
import { AppUpdateService } from '../../../src/web-server/services/app-update-service';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

it('C1 drops whole credential carriers through command, export, normalization and saved jobs', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-round2-'));
  roots.push(root);
  const samples = [
    '{"//registry.example.test/:_auth":"dXNlcjpmaXh0dXJl"}',
    'Cookie: SID=fixtureCookieOne;\n\tHSID=fixtureCookieTwo',
    'installer failed --password "first second third"',
    'token=' + 'x'.repeat(70000) + '\n    continuationWitness',
    'x'.repeat(700) + ' api_key=markerBeyondLineLimit',
  ];
  const script = `import json,pathlib,sys
from unittest import mock
sys.path.insert(0, sys.argv[1])
import app_update_common as c
rows=[]
with mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(sys.argv[2])):
    for sample in json.load(sys.stdin):
        with c.row_diagnostics() as record:
            try:
                c.command([sys.executable, '-c', 'import sys; sys.stderr.write('+repr(sample)+'); sys.exit(1)'],
                          errors=True, label='fixture updater')
            except c.UpdateFailure:
                pass
        row=c.result('t3-acp-adapters','mac','failed',code='update_failed')
        row['diagnostics']=record.export(row)
        rows.append(row)
print(json.dumps(rows))`;
  const child = Bun.spawnSync(
    ['python3', '-c', script, path.join(process.cwd(), 'scripts/app-updates'), root],
    { stdin: Buffer.from(JSON.stringify(samples)), env: { ...process.env, CCS_HOME: root } }
  );
  expect(child.exitCode).toBe(0);
  const rows = JSON.parse(child.stdout.toString()) as Record<string, unknown>[];
  for (const [index, raw] of rows.entries()) {
    // A credential flag, or a quote after a marker, ends the kept output; other markers drop
    // their line and its continuations.
    const removed = /^[{]|^installer/.test(samples[index])
      ? '<rest of output removed: may contain a credential>'
      : '<line removed: may contain a credential>';
    expect(scrubText(samples[index])).toBe(removed);
    const normalized = normalizeAppUpdateRow(raw, 't3-acp-adapters', 'mac');
    expect(normalized.diagnostics?.tail).toBe(removed);
    const storage = path.join(root, String(index));
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
    const saved = new AppUpdateService({ ccsDir: storage }).getStatus().job!;
    expect(
      saved.results.find((row) => row.appId === 't3-acp-adapters' && row.platform === 'mac')
        ?.diagnostics?.tail
    ).toBe(removed);
    for (const file of ['dashboard-job.json', 'dashboard-job-expanded.json']) {
      const text = fs.readFileSync(path.join(storage, 'app-updates', file), 'utf8');
      for (const witness of [
        'dXNlcjpmaXh0dXJl',
        'fixtureCookieOne',
        'fixtureCookieTwo',
        'second third',
        'continuationWitness',
        'markerBeyondLineLimit',
      ])
        expect(text).not.toContain(witness);
    }
  }
});

const AFTER = 'update step finished';
const REMOVED = '<line removed: may contain a credential>';
const REST = '<rest of output removed: may contain a credential>';

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
const H1_WITNESSES = ['horse', 'battery', 'staple', 'correctHorseWitness', 'continuationWitness'];
const H1_CONTROL =
  'installer: unpacking zcode-acp-server 0.84.3 (mkdir -p prefix; lsof -a -p 4242 found 0 holders)';

/** Every flag the token pass redacts, both dash forms, quoted and unquoted, '=' and space. */
function h1Samples(): string[] {
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

function helperRows(root: string, samples: string[], argv = false): Record<string, unknown>[] {
  const script = `import json,pathlib,sys
from unittest import mock
sys.path.insert(0, sys.argv[1])
import app_update_common as c
rows=[]
with mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(sys.argv[2])):
    for sample in json.load(sys.stdin):
        with c.row_diagnostics() as record:
            try:
                if sys.argv[3] == 'argv':
                    child = pathlib.Path(sys.argv[2]) / 'fail.py'
                    child.write_text('import sys; sys.exit(1)')
                    c.command([sys.executable, str(child), '--pass', sample], errors=True)
                else:
                    c.command([sys.executable, '-c', 'import sys; sys.stderr.write('+repr(sample)+'); sys.exit(1)'],
                              errors=True, label='fixture updater')
            except c.UpdateFailure:
                pass
        row=c.result('t3-acp-adapters','mac','failed',code='update_failed')
        row['diagnostics']=record.export(row)
        rows.append(row)
print(json.dumps(rows))`;
  const child = Bun.spawnSync(
    [
      'python3',
      '-c',
      script,
      path.join(process.cwd(), 'scripts/app-updates'),
      root,
      argv ? 'argv' : 'out',
    ],
    { stdin: Buffer.from(JSON.stringify(samples)), env: { ...process.env, CCS_HOME: root } }
  );
  expect(child.exitCode).toBe(0);
  return JSON.parse(child.stdout.toString()) as Record<string, unknown>[];
}

/** Rows whose deciding command received each sample as one whole argument. */
function helperArgRows(root: string, args: string[]): Record<string, unknown>[] {
  const script = `import json,pathlib,sys
from unittest import mock
sys.path.insert(0, sys.argv[1])
import app_update_common as c
root=pathlib.Path(sys.argv[2]); child=root/'fail.py'; child.write_text('import sys; sys.exit(1)')
rows=[]
with mock.patch.object(pathlib.Path, 'home', return_value=root):
    for arg in json.load(sys.stdin):
        with c.row_diagnostics() as record:
            try:
                c.command([sys.executable, str(child), arg], errors=True)
            except c.UpdateFailure:
                pass
        row=c.result('t3-acp-adapters','mac','failed',code='update_failed')
        row['diagnostics']=record.export(row)
        rows.append(row)
print(json.dumps(rows))`;
  const child = Bun.spawnSync(
    ['python3', '-c', script, path.join(process.cwd(), 'scripts/app-updates'), root],
    { stdin: Buffer.from(JSON.stringify(args)), env: { ...process.env, CCS_HOME: root } }
  );
  expect(child.exitCode).toBe(0);
  return JSON.parse(child.stdout.toString()) as Record<string, unknown>[];
}

/** Runs one job whose Mac adapter row is `raw`, then restores it from disk. */
async function savedTail(storage: string, raw: Record<string, unknown>) {
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
  const diagnostics = saved.results.find(
    (row) => row.appId === 't3-acp-adapters' && row.platform === 'mac'
  )?.diagnostics;
  return {
    tail: diagnostics?.tail,
    command: diagnostics?.command,
    files: [...files, job, JSON.stringify(saved)],
  };
}

it('H1 the whole-line and token credential definitions match the Python helper', () => {
  const child = Bun.spawnSync([
    'python3',
    '-c',
    'import json,sys; sys.path.insert(0, sys.argv[1]); import app_update_common as c; ' +
      'print(json.dumps([list(c.CREDENTIAL_FLAGS), list(c.CREDENTIAL_MARKERS)]))',
    path.join(process.cwd(), 'scripts/app-updates'),
  ]);
  expect(child.exitCode).toBe(0);
  expect(JSON.parse(child.stdout.toString())).toEqual([
    [...CREDENTIAL_FLAGS],
    [...CREDENTIAL_MARKERS],
  ]);
  const flag = new RegExp(`^--?(?:${CREDENTIAL_FLAGS.join('|')})$`, 'i');
  for (const alias of H1_FLAGS) expect(flag.test(alias)).toBe(true);
  expect(CREDENTIAL_MARKERS).toContain(`--?(?:${CREDENTIAL_FLAGS.join('|')})\\b`);
});

it('H1 every recognised flag alias drops its whole line through command, export, normalization, saved jobs and restoration', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-h1-'));
  roots.push(root);
  const samples = h1Samples();
  const rows = helperRows(root, samples);
  const removed = REST;
  for (const [index, raw] of rows.entries()) {
    expect([samples[index], scrubText(samples[index])]).toEqual([samples[index], removed]);
    const normalized = normalizeAppUpdateRow(raw, 't3-acp-adapters', 'mac');
    expect([samples[index], normalized.diagnostics?.tail]).toEqual([samples[index], removed]);
    const { tail, files } = await savedTail(path.join(root, String(index)), raw);
    expect([samples[index], tail]).toEqual([samples[index], removed]);
    for (const text of [JSON.stringify(raw), ...files])
      for (const witness of H1_WITNESSES) expect(text).not.toContain(witness);
  }
}, 60_000);

it('H1 an ordinary installer line keeps its text through the same path', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-h1-control-'));
  roots.push(root);
  const [raw] = helperRows(root, [H1_CONTROL]);
  expect(scrubText(H1_CONTROL)).toBe(H1_CONTROL);
  expect(normalizeAppUpdateRow(raw, 't3-acp-adapters', 'mac').diagnostics?.tail).toBe(H1_CONTROL);
  expect((await savedTail(path.join(root, 'job'), raw)).tail).toBe(H1_CONTROL);
});

// [output, expected tail]: round-1 witnesses. Each carries a credential flag, so its line and the
// rest of the output go.
const H1_MULTILINE: Array<[string, string]> = [
  [`installer --pass "correct\nhorse battery staple"\n${AFTER}`, REST],
  [`installer --pass 'correct\nhorse battery staple'\n${AFTER}`, REST],
  [`installer -OTP 'correct\nhorse battery\nstaple'\n${AFTER}`, REST],
  [`installer -OTP "correct\nhorse battery staple"\n${AFTER}`, REST],
  [`installer --password "correct\nhorse battery staple"\n${AFTER}`, REST],
  [`installer --password 'correct\nhorse battery staple'\n${AFTER}`, REST],
  [`installer --pass "correct\\"\nhorse battery staple"\n${AFTER}`, REST],
  [`installer --pass "correct\nhorse battery staple\n${AFTER}`, REST],
  [`installer -OTP 'correct\nhorse\nbattery staple\n${AFTER}`, REST],
  ['installer --password "correct\nhorse battery staple', REST],
];

it('H1 a multi-line value after a flag never keeps a tail, through every saved path', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-h1-multiline-'));
  roots.push(root);
  const rows = helperRows(
    root,
    H1_MULTILINE.map(([sample]) => sample)
  );
  for (const [index, raw] of rows.entries()) {
    const [sample, expected] = H1_MULTILINE[index];
    expect([sample, scrubText(sample)]).toEqual([sample, expected]);
    const normalized = normalizeAppUpdateRow(raw, 't3-acp-adapters', 'mac');
    expect([sample, normalized.diagnostics?.tail]).toEqual([sample, expected]);
    const { tail, files } = await savedTail(path.join(root, String(index)), raw);
    expect([sample, tail]).toEqual([sample, expected]);
    expect(files[1]).toContain(expected.split('\n').pop()!);
    for (const text of [JSON.stringify(raw), ...files])
      for (const witness of H1_WITNESSES) expect(text).not.toContain(witness);
  }
}, 30_000);

it('H1 an exit code keeps its failure line through the same path', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-h1-exit-'));
  roots.push(root);
  const line = 'installer failed: exit code=1; ENOSPC: no space left on device';
  const [raw] = helperRows(root, [line]);
  expect(scrubText(line)).toBe(line);
  expect(scrubText('callback code=abc123def')).toBe(REMOVED);
  expect(scrubText('verification code=482913')).toBe(REMOVED);
  expect(normalizeAppUpdateRow(raw, 't3-acp-adapters', 'mac').diagnostics?.tail).toBe(line);
  expect((await savedTail(path.join(root, 'job'), raw)).tail).toBe(line);
});

// RECHECK-H1-2 probe: 24 output carriers. Each holds a credential flag, or a quote after a marker,
// so its line and the rest of the output go, with no quote parsing.
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

it('H1 recheck-2 output carriers end the kept output, through every saved path', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-h1-probe-'));
  roots.push(root);
  const rows = helperRows(
    root,
    H1_PROBE.map(([, sample]) => sample)
  );
  for (const [index, raw] of rows.entries()) {
    const [name, sample] = H1_PROBE[index];
    expect([name, (raw.diagnostics as Record<string, unknown>).tail]).toEqual([name, REST]);
    expect([name, scrubText(sample)]).toEqual([name, REST]);
    const normalized = normalizeAppUpdateRow(raw, 't3-acp-adapters', 'mac');
    expect([name, normalized.diagnostics?.tail]).toEqual([name, REST]);
    const { tail, files } = await savedTail(path.join(root, String(index)), raw);
    expect([name, tail]).toEqual([name, REST]);
    expect(files[1]).toContain(REST);
    for (const text of [JSON.stringify(raw), JSON.stringify(normalized), ...files])
      for (const witness of H1_WITNESSES)
        expect([name, witness, text.includes(witness)]).toEqual([name, witness, false]);
  }
}, 60_000);

it('H1 recheck-2 a line break in an argument never leaves a tail in the recorded command', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-h1-argv-'));
  roots.push(root);
  const values = ['correct\nhorse battery staple', 'zebraWitness'];
  const rows = helperRows(root, values, true);
  for (const [index, raw] of rows.entries()) {
    expect((raw.diagnostics as Record<string, unknown>).command).toBe(REST);
    const normalized = normalizeAppUpdateRow(raw, 't3-acp-adapters', 'mac');
    expect(normalized.diagnostics?.command).toBe(REST);
    const { command, files } = await savedTail(path.join(root, String(index)), raw);
    expect(command).toBe(REST);
    for (const text of [JSON.stringify(raw), JSON.stringify(normalized), ...files])
      for (const witness of [...H1_WITNESSES, 'zebraWitness']) expect(text).not.toContain(witness);
  }
  // A stored row from an older helper: the dashboard collapses line breaks before scrubbing.
  const stored = {
    appId: 't3-acp-adapters',
    status: 'failed',
    messageCode: 'update_failed',
    diagnostics: {
      command: 'installer --pass correct\nhorse battery staple',
      cause: 'installer_failed',
    },
  };
  expect(normalizeAppUpdateRow(stored, 't3-acp-adapters', 'mac').diagnostics?.command).toBe(REST);
});

it('H1 recheck-3 a long slash equals argument never leaves fragments in the recorded command', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-h1-longargv-'));
  roots.push(root);
  // Path shortening keeps an argument's last characters; the flag must be seen on the complete
  // argument before that shortening can remove it.
  const args = [
    '--pass=/' + 'horse battery staple/'.repeat(12),
    '--password=/' + 'horse battery staple/'.repeat(12),
  ];
  const rows = helperArgRows(root, args);
  for (const [index, raw] of rows.entries()) {
    const arg = args[index];
    expect(scrubText(arg)).toBe(REST);
    expect((raw.diagnostics as Record<string, unknown>).command).toBe(REST);
    const normalized = normalizeAppUpdateRow(raw, 't3-acp-adapters', 'mac');
    expect(normalized.diagnostics?.command).toBe(REST);
    const { command, files } = await savedTail(path.join(root, String(index)), raw);
    expect(command).toBe(REST);
    expect(files[1]).toContain(REST);
    for (const text of [JSON.stringify(raw), JSON.stringify(normalized), ...files])
      for (const witness of H1_WITNESSES) expect(text).not.toContain(witness);
    // A stored row from an older helper: its unshortened command is cleaned on read,
    // through the service job, both job files and restoration by a fresh service.
    const stored = {
      ...raw,
      diagnostics: {
        ...(raw.diagnostics as Record<string, unknown>),
        command: `installer ${arg}`,
      },
    };
    expect(normalizeAppUpdateRow(stored, 't3-acp-adapters', 'mac').diagnostics?.command).toBe(REST);
    const restored = await savedTail(path.join(root, 'stored' + index), stored);
    expect(restored.command).toBe(REST);
    for (const text of restored.files)
      for (const witness of H1_WITNESSES) expect(text).not.toContain(witness);
  }
  // Past the stored-command length limit the flag still replaces the whole command:
  // the check runs on the complete value before any length guard or shortening.
  const longArg = '--pass=/' + 'horse battery staple/'.repeat(60);
  const longStored = {
    appId: 't3-acp-adapters',
    status: 'failed',
    messageCode: 'update_failed',
    diagnostics: {
      command: `installer ${longArg}`,
      cause: 'installer_failed',
      owner: { manager: 'native', executable: longArg },
    },
  };
  const longNormalized = normalizeAppUpdateRow(longStored, 't3-acp-adapters', 'mac');
  expect(longNormalized.diagnostics?.command).toBe(REST);
  expect(longNormalized.diagnostics?.owner?.executable).toBe(REST);
  for (const witness of H1_WITNESSES) expect(JSON.stringify(longNormalized)).not.toContain(witness);
  // Short and ordinary commands still display normally.
  expect(scrubText('installer --verbose share/file.txt')).toBe(
    'installer --verbose share/file.txt'
  );
  const control = {
    appId: 't3-acp-adapters',
    status: 'failed',
    messageCode: 'update_failed',
    diagnostics: { command: 'installer --verbose share/file.txt', cause: 'installer_failed' },
  };
  expect(normalizeAppUpdateRow(control, 't3-acp-adapters', 'mac').diagnostics?.command).toBe(
    'installer --verbose share/file.txt'
  );
  const [controlRow] = helperArgRows(root, ['--verbose']);
  expect(
    normalizeAppUpdateRow(controlRow, 't3-acp-adapters', 'mac').diagnostics?.command
  ).toContain('--verbose');
}, 60_000);
