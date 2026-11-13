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
  const removed = '<line removed: may contain a credential>';
  for (const [index, raw] of rows.entries()) {
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

function helperRows(root: string, samples: string[]): Record<string, unknown>[] {
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
  const saved = new AppUpdateService({ ccsDir: storage }).getStatus().job!;
  const files = ['dashboard-job.json', 'dashboard-job-expanded.json'].map((file) =>
    fs.readFileSync(path.join(storage, 'app-updates', file), 'utf8')
  );
  return {
    tail: saved.results.find((row) => row.appId === 't3-acp-adapters' && row.platform === 'mac')
      ?.diagnostics?.tail,
    files,
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
  const removed = '<line removed: may contain a credential>';
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
