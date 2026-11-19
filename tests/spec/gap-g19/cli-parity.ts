/**
 * Gap G19 CLI parity: the native Rust CLI against the shipped TS CLI on fixtures.
 *
 *   bun tests/spec/gap-g19/cli-parity.ts <rust-cli>
 *
 * <rust-cli> is the built native binary (target/debug/ai-account-center). The TS side runs the
 * shipped entry (bun src/ccs.ts). Every case uses a temporary home; no provider calls, no real
 * credentials. store cases need the native store module (bun run store:build).
 *
 * CUT1 = A: the Settings-covered families are a deliberate behavior change, so their cases
 * assert the new Rust contract (guidance + exit 1) instead of TS equality. store and proxy
 * compare stdout, stderr and exit codes byte-for-byte after normalizing only what cannot be
 * equal by construction: temporary home paths and backup stamps/ids. The saved config.yaml is
 * compared by parsed value (the writers' YAML layouts differ; the dashboard reads values).
 */
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import yaml from 'js-yaml';

const repo = path.resolve(import.meta.dirname, '../../..');
const rustCli = process.argv[2];
if (!rustCli || !fs.existsSync(rustCli)) {
  console.error('usage: bun tests/spec/gap-g19/cli-parity.ts <rust-cli-binary>');
  process.exit(2);
}
const storeModule = path.join(
  repo,
  'dist/native',
  `aac-store.${process.platform}-${process.arch}.node`
);
const tsArgs = [path.join(repo, 'src/ccs.ts')];

interface Run {
  status: number;
  stdout: string;
  stderr: string;
}

function runCli(
  side: 'ts' | 'rust',
  home: string,
  args: string[],
  env: Record<string, string> = {}
): Run {
  const result = spawnSync(
    side === 'ts' ? 'bun' : rustCli,
    [...(side === 'ts' ? tsArgs : []), ...args],
    {
      cwd: repo,
      encoding: 'utf8',
      timeout: 60_000,
      env: {
        PATH: process.env.PATH,
        HOME: home,
        USERPROFILE: home,
        CCS_DIR: home,
        NO_COLOR: '1',
        TMPDIR: os.tmpdir(),
        AAC_STORE_MODULE: storeModule,
        ...env,
      },
    }
  );
  return { status: result.status ?? -1, stdout: result.stdout ?? '', stderr: result.stderr ?? '' };
}

let failures = 0;
let cases = 0;
function check(name: string, ok: boolean, detail = ''): void {
  cases += 1;
  if (ok) {
    console.log(`PASS ${name}`);
  } else {
    failures += 1;
    console.log(`FAIL ${name}${detail ? `\n${detail}` : ''}`);
  }
}

function freshHome(seed?: string): string {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-g19-'));
  if (seed) fs.cpSync(seed, home, { recursive: true });
  return home;
}

/** Replace what cannot be equal by construction: this run's home paths and backup stamps. */
function normalize(text: string, homes: string[]): string {
  let out = text;
  for (const home of homes) out = out.split(home).join('<home>');
  return out
    .replace(/bak-proxy-\d{8}T\d{6}Z(-\d+)?/g, 'bak-proxy-<stamp>')
    .replace(/Backup [0-9A-Za-z_-]+ written/g, 'Backup <id> written')
    .replace(/Restored backup [0-9A-Za-z_-]+\./g, 'Restored backup <id>.')
    .replace(/quarantine([\\/])[0-9A-Za-z_-]+/g, 'quarantine$1<id>')
    .replace(/"openedAt": \d+/g, '"openedAt": <ms>')
    .replace(/"checkedAt": \d+/g, '"checkedAt": <ms>');
}

function equalRuns(name: string, ts: Run, rust: Run, homes: string[]): void {
  const a = { ...ts, stdout: normalize(ts.stdout, homes), stderr: normalize(ts.stderr, homes) };
  const b = {
    ...rust,
    stdout: normalize(rust.stdout, homes),
    stderr: normalize(rust.stderr, homes),
  };
  check(
    name,
    a.status === b.status && a.stdout === b.stdout && a.stderr === b.stderr,
    `ts=${JSON.stringify(a)}\nrust=${JSON.stringify(b)}`
  );
}

// ---------------------------------------------------------------------------
// 1. Dropped families: the new Rust contract (Settings guidance, exit 1).
// ---------------------------------------------------------------------------
const dropped: Array<[string, string[]]> = [
  ['auth', ['dashboard', 'auth']],
  ['auth', ['dashboard', 'auth', 'setup']],
  ['auth', ['dashboard', 'auth', 'show']],
  ['auth', ['dashboard', 'auth', 'status']],
  ['auth', ['dashboard', 'auth', 'disable']],
  ['auth', ['config', 'auth', 'show']],
];
for (const [family, args] of dropped) {
  const home = freshHome();
  const rust = runCli('rust', home, args);
  check(
    `drop ${args.slice(1).join(' ')}`,
    rust.status === 1 &&
      rust.stdout === '' &&
      rust.stderr.startsWith(`[X] "dashboard ${family}" is managed in the dashboard now`) &&
      rust.stderr.includes('Settings'),
    JSON.stringify(rust)
  );
  fs.rmSync(home, { recursive: true, force: true });
}

// ---------------------------------------------------------------------------
// 1b. Ported admin families: usage-hub, usage-snapshot and cursor-key.
// The key itself is random, so the key and wall-clock fields are placeholders; the
// stored hash is checked against the printed key, which is the property that matters.
// ---------------------------------------------------------------------------
function adminNormalize(text: string, homes: string[]): string {
  return normalize(text, homes)
    .replace(/aacu_[A-Za-z0-9_-]{43}/g, '<hub-key>')
    .replace(/key fingerprint [0-9a-f]{8}/g, 'key fingerprint <fp>')
    .replace(/created [0-9T:.Z-]+/g, 'created <iso>')
    .replace(/Last written [0-9T:.Z-]+ \([^)]* ago\)/g, 'Last written <iso> (<span> ago)')
    .replace(/other computers: http:\/\/[^:\s]+:<dashboard port>/g, 'other computers: <lan>');
}

function equalAdmin(name: string, ts: Run, rust: Run, homes: string[]): void {
  const a = {
    ...ts,
    stdout: adminNormalize(ts.stdout, homes),
    stderr: adminNormalize(ts.stderr, homes),
  };
  const b = {
    ...rust,
    stdout: adminNormalize(rust.stdout, homes),
    stderr: adminNormalize(rust.stderr, homes),
  };
  check(
    name,
    a.status === b.status && a.stdout === b.stdout && a.stderr === b.stderr,
    `ts=${JSON.stringify(a)}\nrust=${JSON.stringify(b)}`
  );
}

const hubErrorCases: string[][] = [
  ['dashboard', 'usage-hub'],
  ['dashboard', 'usage-hub', '--help'],
  ['dashboard', 'usage-hub', 'bogus'],
  ['dashboard', 'usage-hub', 'generate'],
  ['dashboard', 'usage-hub', 'status', 'extra'],
  ['dashboard', 'usage-hub', 'rotate', '--stdout', 'extra'],
  ['dashboard', 'usage-hub', 'off', 'x'],
];
for (const args of hubErrorCases) {
  const homeA = freshHome();
  const homeB = freshHome();
  equalAdmin(
    `usage-hub ${args.slice(2).join(' ') || '(no args)'}`,
    runCli('ts', homeA, args),
    runCli('rust', homeB, args),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}

{
  const homeA = freshHome();
  const homeB = freshHome();
  const statusOff = ['dashboard', 'usage-hub', 'status'];
  equalAdmin(
    'usage-hub status off',
    runCli('ts', homeA, statusOff),
    runCli('rust', homeB, statusOff),
    [homeA, homeB]
  );

  const generate = ['dashboard', 'usage-hub', 'generate', '--stdout'];
  const genA = runCli('ts', homeA, generate);
  const genB = runCli('rust', homeB, generate);
  equalAdmin('usage-hub generate', genA, genB, [homeA, homeB]);
  const keyA = /aacu_[A-Za-z0-9_-]{43}/.exec(genA.stdout)?.[0] ?? '';
  const keyB = /aacu_[A-Za-z0-9_-]{43}/.exec(genB.stdout)?.[0] ?? '';
  const recordA = JSON.parse(
    fs.readFileSync(path.join(homeA, 'auth', 'usage-hub-key.json'), 'utf8')
  );
  const recordB = JSON.parse(
    fs.readFileSync(path.join(homeB, 'auth', 'usage-hub-key.json'), 'utf8')
  );
  check(
    'usage-hub stores only the key hash',
    keyA.length === 48 &&
      keyB.length === 48 &&
      recordA.version === 1 &&
      recordB.version === 1 &&
      recordA.keySha256 === createHash('sha256').update(keyA, 'utf8').digest('hex') &&
      recordB.keySha256 === createHash('sha256').update(keyB, 'utf8').digest('hex') &&
      Number.isFinite(Date.parse(recordA.createdAt)) &&
      Number.isFinite(Date.parse(recordB.createdAt))
  );

  equalAdmin(
    'usage-hub status on',
    runCli('ts', homeA, statusOff),
    runCli('rust', homeB, statusOff),
    [homeA, homeB]
  );
  equalAdmin(
    'usage-hub generate refuses an existing key',
    runCli('ts', homeA, generate),
    runCli('rust', homeB, generate),
    [homeA, homeB]
  );

  const rotate = ['dashboard', 'usage-hub', 'rotate', '--stdout'];
  const rotA = runCli('ts', homeA, rotate);
  const rotB = runCli('rust', homeB, rotate);
  equalAdmin('usage-hub rotate', rotA, rotB, [homeA, homeB]);
  const rotatedA = JSON.parse(
    fs.readFileSync(path.join(homeA, 'auth', 'usage-hub-key.json'), 'utf8')
  );
  const rotatedB = JSON.parse(
    fs.readFileSync(path.join(homeB, 'auth', 'usage-hub-key.json'), 'utf8')
  );
  check(
    'usage-hub rotate replaces the stored hash',
    rotatedA.keySha256 !== recordA.keySha256 && rotatedB.keySha256 !== recordB.keySha256
  );

  const off = ['dashboard', 'usage-hub', 'off'];
  equalAdmin('usage-hub off', runCli('ts', homeA, off), runCli('rust', homeB, off), [homeA, homeB]);
  equalAdmin('usage-hub off again', runCli('ts', homeA, off), runCli('rust', homeB, off), [
    homeA,
    homeB,
  ]);
  check(
    'usage-hub off removes the key file',
    !fs.existsSync(path.join(homeA, 'auth', 'usage-hub-key.json')) &&
      !fs.existsSync(path.join(homeB, 'auth', 'usage-hub-key.json'))
  );

  // A damaged key file reads as "invalid" on both sides.
  for (const home of [homeA, homeB]) {
    fs.mkdirSync(path.join(home, 'auth'), { recursive: true });
    fs.writeFileSync(
      path.join(home, 'auth', 'usage-hub-key.json'),
      '{"version":1,"keySha256":"nope"}\n'
    );
  }
  equalAdmin(
    'usage-hub status invalid',
    runCli('ts', homeA, statusOff),
    runCli('rust', homeB, statusOff),
    [homeA, homeB]
  );

  // Trusted local network on: the LAN lines and the configured range are printed.
  const config = [
    'version: 14',
    'dashboard_network:',
    '  trust_local_network: true',
    '  trusted_networks:',
    '    - 10.6.0.0/24',
    '',
  ].join('\n');
  fs.writeFileSync(path.join(homeA, 'config.yaml'), config);
  fs.writeFileSync(path.join(homeB, 'config.yaml'), config);
  equalAdmin(
    'usage-hub status trusted network',
    runCli('ts', homeA, statusOff),
    runCli('rust', homeB, statusOff),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}

const snapshotErrorCases: string[][] = [
  ['dashboard', 'usage-snapshot'],
  ['dashboard', 'usage-snapshot', '--help'],
  ['dashboard', 'usage-snapshot', 'bogus'],
  ['dashboard', 'usage-snapshot', 'status', 'extra'],
  ['dashboard', 'usage-snapshot', 'off', 'x'],
  ['dashboard', 'usage-snapshot', 'set'],
  ['dashboard', 'usage-snapshot', 'set', 'relative/path'],
  ['dashboard', 'usage-snapshot', 'set', '/tmp/aac-usage/../escape'],
];
for (const args of snapshotErrorCases) {
  const homeA = freshHome();
  const homeB = freshHome();
  equalAdmin(
    `usage-snapshot ${args.slice(2).join(' ') || '(no args)'}`,
    runCli('ts', homeA, args),
    runCli('rust', homeB, args),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}

{
  const homeA = freshHome();
  const homeB = freshHome();
  const folder = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-snap-'));
  const status = ['dashboard', 'usage-snapshot', 'status'];
  equalAdmin(
    'usage-snapshot status off',
    runCli('ts', homeA, status),
    runCli('rust', homeB, status),
    [homeA, homeB]
  );

  const missingParent = path.join(os.tmpdir(), `aac-snap-missing-${process.pid}`, 'child');
  const setMissing = ['dashboard', 'usage-snapshot', 'set', missingParent];
  equalAdmin(
    'usage-snapshot set refuses a missing parent',
    runCli('ts', homeA, setMissing),
    runCli('rust', homeB, setMissing),
    [homeA, homeB]
  );

  const set = ['dashboard', 'usage-snapshot', 'set', folder];
  equalAdmin('usage-snapshot set', runCli('ts', homeA, set), runCli('rust', homeB, set), [
    homeA,
    homeB,
  ]);
  const settingsA = JSON.parse(
    fs.readFileSync(path.join(homeA, 'agent-usage-snapshot.json'), 'utf8')
  );
  const settingsB = JSON.parse(
    fs.readFileSync(path.join(homeB, 'agent-usage-snapshot.json'), 'utf8')
  );
  check(
    'usage-snapshot settings agree',
    JSON.stringify(settingsA) === JSON.stringify(settingsB) &&
      settingsA.version === 1 &&
      settingsA.directory === folder
  );
  equalAdmin(
    'usage-snapshot status on without a snapshot',
    runCli('ts', homeA, status),
    runCli('rust', homeB, status),
    [homeA, homeB]
  );

  fs.writeFileSync(path.join(folder, 'usage.json'), '{"version":1}\n');
  const fixed = new Date('2026-11-19T12:00:00.000Z');
  fs.utimesSync(path.join(folder, 'usage.json'), fixed, fixed);
  equalAdmin(
    'usage-snapshot status with a snapshot',
    runCli('ts', homeA, status),
    runCli('rust', homeB, status),
    [homeA, homeB]
  );

  const located = [
    'dashboard',
    'usage-snapshot',
    'set',
    folder,
    '--location',
    'the Mac=/Volumes/share/aac-usage',
    '--location=Windows=\\\\TRUENAS\\share\\aac-usage',
  ];
  equalAdmin(
    'usage-snapshot set with locations',
    runCli('ts', homeA, located),
    runCli('rust', homeB, located),
    [homeA, homeB]
  );
  const locatedA = JSON.parse(
    fs.readFileSync(path.join(homeA, 'agent-usage-snapshot.json'), 'utf8')
  );
  const locatedB = JSON.parse(
    fs.readFileSync(path.join(homeB, 'agent-usage-snapshot.json'), 'utf8')
  );
  check(
    'usage-snapshot locations agree',
    JSON.stringify(locatedA) === JSON.stringify(locatedB) && locatedA.locations?.length === 2
  );
  equalAdmin(
    'usage-snapshot status lists locations',
    runCli('ts', homeA, status),
    runCli('rust', homeB, status),
    [homeA, homeB]
  );

  const badLocation = ['dashboard', 'usage-snapshot', 'set', folder, '--location', 'no-equals'];
  equalAdmin(
    'usage-snapshot set refuses a bad location',
    runCli('ts', homeA, badLocation),
    runCli('rust', homeB, badLocation),
    [homeA, homeB]
  );

  const off = ['dashboard', 'usage-snapshot', 'off'];
  equalAdmin('usage-snapshot off', runCli('ts', homeA, off), runCli('rust', homeB, off), [
    homeA,
    homeB,
  ]);
  equalAdmin(
    'usage-snapshot status after off',
    runCli('ts', homeA, status),
    runCli('rust', homeB, status),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
  fs.rmSync(folder, { recursive: true, force: true });
}

const cursorErrorCases: string[][] = [
  ['dashboard', 'cursor-key'],
  ['dashboard', 'cursor-key', 'help'],
  ['dashboard', 'cursor-key', 'bogus'],
  ['dashboard', 'cursor-key', 'set', 'extra'],
  ['dashboard', 'cursor-key', 'status'],
  ['dashboard', 'cursor-key', 'remove'],
  ['dashboard', 'cursor-key', 'set'],
];
for (const args of cursorErrorCases) {
  const homeA = freshHome();
  const homeB = freshHome();
  equalAdmin(
    `cursor-key ${args.slice(2).join(' ') || '(no args)'}`,
    runCli('ts', homeA, args),
    runCli('rust', homeB, args),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}

// ---------------------------------------------------------------------------
// 2. store: help, errors, and the four verbs on a fixture store.
// ---------------------------------------------------------------------------
const storeErrorCases: string[][] = [
  ['dashboard', 'store'],
  ['dashboard', 'store', 'help'],
  ['dashboard', 'store', '--help'],
  ['dashboard', 'store', 'bogus'],
  ['dashboard', 'store', 'status', 'extra'],
  ['dashboard', 'store', 'restore'],
  ['dashboard', 'store', 'restore', 'a', 'b'],
];
for (const args of storeErrorCases) {
  const homeA = freshHome();
  const homeB = freshHome();
  equalRuns(
    `store ${args.slice(2).join(' ') || '(no args)'}`,
    runCli('ts', homeA, args),
    runCli('rust', homeB, args),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}

// One fixture store, created by the Rust CLI's open, copied byte-equal to both sides.
const storeSeed = freshHome();
runCli('rust', storeSeed, ['dashboard', 'store', 'status']);
check('store fixture created', fs.existsSync(path.join(storeSeed, 'store')));

for (const verb of ['status', 'check']) {
  const homeA = freshHome(storeSeed);
  const homeB = freshHome(storeSeed);
  equalRuns(
    `store ${verb} on the fixture`,
    runCli('ts', homeA, ['dashboard', 'store', verb]),
    runCli('rust', homeB, ['dashboard', 'store', verb]),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}

// backup: the id and stamp differ by construction; the message shape and file count match,
// and both sides write a verified backup folder.
{
  const homeA = freshHome(storeSeed);
  const homeB = freshHome(storeSeed);
  const ts = runCli('ts', homeA, ['dashboard', 'store', 'backup']);
  const rust = runCli('rust', homeB, ['dashboard', 'store', 'backup']);
  const shape = /^\[OK\] Backup \S+ written and verified \((\d+) files\)\.\n$/;
  const tsMatch = shape.exec(ts.stdout);
  const rustMatch = shape.exec(rust.stdout);
  check(
    'store backup message and count',
    ts.status === 0 &&
      rust.status === 0 &&
      ts.stderr === '' &&
      rust.stderr === '' &&
      !!tsMatch &&
      !!rustMatch &&
      tsMatch[1] === rustMatch[1],
    `ts=${JSON.stringify(ts)}\nrust=${JSON.stringify(rust)}`
  );
  const backupsA = path.join(homeA, 'store', 'backups');
  const backupsB = path.join(homeB, 'store', 'backups');
  check(
    'store backup folders written',
    fs.existsSync(backupsA) &&
      fs.existsSync(backupsB) &&
      JSON.stringify(fs.readdirSync(backupsA).sort()) === JSON.stringify(['pool', 'sets']) &&
      JSON.stringify(fs.readdirSync(backupsB).sort()) === JSON.stringify(['pool', 'sets']) &&
      fs.readdirSync(path.join(backupsA, 'sets')).length === 1 &&
      fs.readdirSync(path.join(backupsB, 'sets')).length === 1
  );
  // restore: each side restores its own backup set; paths and ids normalize to the same message.
  const backupA = path.join(backupsA, 'sets', fs.readdirSync(path.join(backupsA, 'sets'))[0]);
  const backupB = path.join(backupsB, 'sets', fs.readdirSync(path.join(backupsB, 'sets'))[0]);
  const tsRestore = runCli('ts', homeA, ['dashboard', 'store', 'restore', backupA]);
  const rustRestore = runCli('rust', homeB, ['dashboard', 'store', 'restore', backupB]);
  equalRuns('store restore', tsRestore, rustRestore, [homeA, homeB]);
  // A refused restore: not a backup folder.
  const notBackup = path.join(homeA, 'not-a-backup');
  fs.mkdirSync(notBackup, { recursive: true });
  const notBackupB = path.join(homeB, 'not-a-backup');
  fs.mkdirSync(notBackupB, { recursive: true });
  equalRuns(
    'store restore refuses a non-backup',
    runCli('ts', homeA, ['dashboard', 'store', 'restore', notBackup]),
    runCli('rust', homeB, ['dashboard', 'store', 'restore', notBackupB]),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}

// lock_held: a holder process keeps the fixture store open; status then uses the API path.
// The fixture API server runs in its own process: runCli uses spawnSync, which blocks this
// process's event loop and would starve an in-process server while the CLI waits on it.
async function fixtureApiServer(): Promise<{ port: number; stop: () => void }> {
  const child = spawn(
    'bun',
    [
      '-e',
      `const server = Bun.serve({ port: 0, fetch(req) {
        const url = new URL(req.url);
        if (url.pathname !== '/api/store/status') return new Response('no', { status: 404 });
        if (req.headers.get('authorization') !== 'Bearer fixture-token') return new Response('no', { status: 401 });
        return Response.json({ state: 'fixture', readings: 7, sealed: true });
      }});
      console.log('port=' + server.port);
      setInterval(() => {}, 1000);`,
    ],
    { stdio: ['ignore', 'pipe', 'pipe'] }
  );
  const port = await new Promise<number>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('fixture server never bound')), 15_000);
    child.stdout.on('data', (chunk) => {
      const match = /port=(\d+)/.exec(String(chunk));
      if (match) {
        clearTimeout(timer);
        resolve(Number(match[1]));
      }
    });
    child.on('exit', (code) => {
      clearTimeout(timer);
      reject(new Error(`fixture server exited: ${code}`));
    });
  });
  return { port, stop: () => child.kill('SIGKILL') };
}

async function withStoreHolder<T>(fn: () => Promise<T> | T): Promise<T> {
  const child = spawn(
    'bun',
    [
      '-e',
      `import { loadNativeStore } from ${JSON.stringify(path.join(repo, 'src/store/native-loader.ts'))};
       const loaded = loadNativeStore({ env: process.env });
       if (!loaded.module) { console.error(loaded.reason); process.exit(3); }
       const handle = new loaded.module.NativeStore();
       const answer = await handle.call('open', JSON.stringify({ dir: process.argv[1] }));
       // Keep the NAPI handle rooted: bun -e module scope is not a GC root after evaluation,
       // and a collected handle drops the engine and releases the store lock.
       globalThis.__aacStoreHandle = handle;
       console.log('held');
       setInterval(() => { void globalThis.__aacStoreHandle; }, 1000);
       setTimeout(() => process.exit(0), 60000);`,
      path.join(storeSeed, 'store'),
    ],
    {
      cwd: repo,
      env: { ...process.env, CCS_HOME: storeSeed, NO_COLOR: '1', AAC_STORE_MODULE: storeModule },
      stdio: ['ignore', 'pipe', 'pipe'],
    }
  );
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('holder never opened the store')), 20_000);
    child.stdout.on('data', (chunk) => {
      if (String(chunk).includes('held')) {
        clearTimeout(timer);
        resolve();
      }
    });
    child.on('exit', (code) => {
      clearTimeout(timer);
      reject(new Error(`holder exited early: ${code}`));
    });
  });
  try {
    return await fn();
  } finally {
    child.kill('SIGKILL');
  }
}

await withStoreHolder(async () => {
  // No API env: both sides print the same guidance.
  // Both sides read the same held store (the holder owns storeSeed/store).
  equalRuns(
    'store status lock_held without API env',
    runCli('ts', storeSeed, ['dashboard', 'store', 'status']),
    runCli('rust', storeSeed, ['dashboard', 'store', 'status']),
    [storeSeed]
  );
  // With API env: a fixture server (own process) answers both sides identically.
  const server = await fixtureApiServer();
  const env = {
    AAC_DASHBOARD_URL: `http://127.0.0.1:${server.port}`,
    AAC_DEVICE_TOKEN: 'fixture-token',
  };
  equalRuns(
    'store status lock_held via API',
    runCli('ts', storeSeed, ['dashboard', 'store', 'status'], env),
    runCli('rust', storeSeed, ['dashboard', 'store', 'status'], env),
    [storeSeed]
  );
  const badEnv = {
    AAC_DASHBOARD_URL: `http://127.0.0.1:${server.port}`,
    AAC_DEVICE_TOKEN: 'wrong',
  };
  equalRuns(
    'store status API refusal',
    runCli('ts', storeSeed, ['dashboard', 'store', 'status'], badEnv),
    runCli('rust', storeSeed, ['dashboard', 'store', 'status'], badEnv),
    [storeSeed]
  );
  server.stop();
});
fs.rmSync(storeSeed, { recursive: true, force: true });

// ---------------------------------------------------------------------------
// 3. proxy: help, errors, status shapes, set/off with config and backup checks.
// ---------------------------------------------------------------------------
const proxyErrorCases: string[][] = [
  ['dashboard', 'proxy'],
  ['dashboard', 'proxy', 'help'],
  ['dashboard', 'proxy', 'bogus'],
  ['dashboard', 'proxy', 'status', 'extra'],
  ['dashboard', 'proxy', 'set'],
  ['dashboard', 'proxy', 'set', '--address'],
  ['dashboard', 'proxy', 'set', '--address', '192.168.50.20'],
  ['dashboard', 'proxy', 'set', '--origin', 'https://aac.example.test'],
  [
    'dashboard',
    'proxy',
    'set',
    '--address',
    '192.168.50.20',
    '--origin',
    'https://aac.example.test',
    '--wat',
  ],
  [
    'dashboard',
    'proxy',
    'set',
    '--address',
    '192.168.50.20',
    '--origin',
    'https://a.test',
    '--origin',
    'https://b.test',
  ],
];
for (const args of proxyErrorCases) {
  const homeA = freshHome();
  const homeB = freshHome();
  equalRuns(
    `proxy ${args.slice(2).join(' ') || '(no args)'}`,
    runCli('ts', homeA, args),
    runCli('rust', homeB, args),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}

const CONFIG_SEEDS: Record<string, string> = {
  'status absent': '',
  'status lan on':
    'version: 1\ndashboard_tls:\n  trusted_proxy: lan-https-proxy\n  trusted_proxy_addresses:\n    - 192.168.50.20\n  public_origin: https://aac.example.test\n',
  'status refused list':
    'version: 1\ndashboard_tls:\n  trusted_proxy: lan-https-proxy\n  trusted_proxy_addresses:\n    - 8.8.8.8\n',
  'status local kind': 'version: 1\ndashboard_tls:\n  trusted_proxy: tailscale-serve\n',
  'status invalid config': '- not a mapping\n',
};
for (const [name, seed] of Object.entries(CONFIG_SEEDS)) {
  const homeA = freshHome();
  const homeB = freshHome();
  if (seed) {
    fs.writeFileSync(path.join(homeA, 'config.yaml'), seed);
    fs.writeFileSync(path.join(homeB, 'config.yaml'), seed);
  }
  equalRuns(
    `proxy ${name}`,
    runCli('ts', homeA, ['dashboard', 'proxy', 'status']),
    runCli('rust', homeB, ['dashboard', 'proxy', 'status']),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}

interface MutationCase {
  name: string;
  seed: string;
  args: string[];
  /** Whether the command saves (and therefore backs up) the config. */
  writes: boolean;
}
const MUTATIONS: MutationCase[] = [
  {
    name: 'set from nothing',
    seed: '',
    writes: true,
    args: ['set', '--address', '192.168.50.20', '--origin', 'https://aac.example.test'],
  },
  {
    name: 'set with existing config',
    seed: 'version: 1\naccounts:\n  work:\n    email: a@b.test\n',
    writes: true,
    args: [
      'set',
      '--address',
      '192.168.50.20',
      '--address',
      '10.0.0.5',
      '--origin',
      'https://aac.example.test:8443',
    ],
  },
  {
    name: 'set refuses a public address',
    seed: 'version: 1\n',
    writes: false,
    args: ['set', '--address', '8.8.8.8', '--origin', 'https://aac.example.test'],
  },
  {
    name: 'set refuses loopback',
    seed: 'version: 1\n',
    writes: false,
    args: ['set', '--address', '127.0.0.1', '--origin', 'https://aac.example.test'],
  },
  {
    name: 'set refuses a bad origin',
    seed: 'version: 1\n',
    writes: false,
    args: ['set', '--address', '192.168.50.20', '--origin', 'https://aac.example.test/path'],
  },
  {
    name: 'set refuses a local kind without --replace',
    seed: 'version: 1\ndashboard_tls:\n  trusted_proxy: tailscale-serve\n',
    writes: false,
    args: ['set', '--address', '192.168.50.20', '--origin', 'https://aac.example.test'],
  },
  {
    name: 'set replaces a local kind with --replace',
    seed: 'version: 1\ndashboard_tls:\n  trusted_proxy: loopback-https-proxy\n',
    writes: true,
    args: [
      'set',
      '--address',
      '192.168.50.20',
      '--origin',
      'https://aac.example.test',
      '--replace',
    ],
  },
  {
    name: 'set no-op on identical values',
    seed: 'version: 1\ndashboard_tls:\n  trusted_proxy: lan-https-proxy\n  trusted_proxy_addresses:\n    - 192.168.50.20\n  public_origin: https://aac.example.test\n',
    writes: false,
    args: ['set', '--address', '192.168.50.20', '--origin', 'https://aac.example.test'],
  },
  { name: 'off when already off', seed: 'version: 1\n', writes: false, args: ['off'] },
  {
    name: 'off removes the lan block',
    seed: 'version: 1\ndashboard_tls:\n  trusted_proxy: lan-https-proxy\n  trusted_proxy_addresses:\n    - 192.168.50.20\n  public_origin: https://aac.example.test\n',
    writes: true,
    args: ['off'],
  },
  {
    name: 'off keeps other tls keys',
    seed: 'version: 1\ndashboard_tls:\n  trusted_proxy_addresses:\n    - 192.168.50.20\n  https_listener:\n    enabled: true\n    port: 3443\n    cert_path: /tmp/c.pem\n    key_path: /tmp/k.pem\n',
    writes: true,
    args: ['off'],
  },
];

function parsedConfig(home: string): Record<string, unknown> | null {
  const file = path.join(home, 'config.yaml');
  if (!fs.existsSync(file)) return null;
  return yaml.load(fs.readFileSync(file, 'utf8')) as Record<string, unknown>;
}

for (const mutation of MUTATIONS) {
  const homeA = freshHome();
  const homeB = freshHome();
  if (mutation.seed) {
    fs.writeFileSync(path.join(homeA, 'config.yaml'), mutation.seed);
    fs.writeFileSync(path.join(homeB, 'config.yaml'), mutation.seed);
  }
  const args = ['dashboard', 'proxy', ...mutation.args];
  const ts = runCli('ts', homeA, args);
  const rust = runCli('rust', homeB, args);
  equalRuns(`proxy ${mutation.name} output`, ts, rust, [homeA, homeB]);
  // The saved settings agree by value; every other original section keeps its value.
  const configA = parsedConfig(homeA);
  const configB = parsedConfig(homeB);
  const tlsA = configA?.dashboard_tls ?? null;
  const tlsB = configB?.dashboard_tls ?? null;
  check(
    `proxy ${mutation.name} saved dashboard_tls`,
    JSON.stringify(tlsA) === JSON.stringify(tlsB),
    `ts=${JSON.stringify(tlsA)}\nrust=${JSON.stringify(tlsB)}`
  );
  if (mutation.seed) {
    const seedConfig = yaml.load(mutation.seed) as Record<string, unknown>;
    const preserved = Object.keys(seedConfig)
      .filter((key) => key !== 'dashboard_tls')
      .every((key) => JSON.stringify(configB?.[key]) === JSON.stringify(seedConfig[key]));
    check(`proxy ${mutation.name} preserves other sections`, preserved);
  }
  // Backups: both sides save the exact previous bytes, 0600, when a file existed.
  const backupsA = fs.readdirSync(homeA).filter((name) => name.includes('.bak-proxy-'));
  const backupsB = fs.readdirSync(homeB).filter((name) => name.includes('.bak-proxy-'));
  check(
    `proxy ${mutation.name} backup presence`,
    backupsA.length === backupsB.length &&
      (mutation.writes && mutation.seed !== '' ? backupsA.length === 1 : backupsA.length === 0)
  );
  if (backupsA.length && backupsB.length) {
    const bytesA = fs.readFileSync(path.join(homeA, backupsA[0]));
    const bytesB = fs.readFileSync(path.join(homeB, backupsB[0]));
    check(
      `proxy ${mutation.name} backup bytes and mode`,
      bytesA.equals(bytesB) &&
        bytesA.toString('utf8') === mutation.seed &&
        (process.platform === 'win32' ||
          ((fs.statSync(path.join(homeA, backupsA[0])).mode & 0o777) === 0o600 &&
            (fs.statSync(path.join(homeB, backupsB[0])).mode & 0o777) === 0o600))
    );
  }
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}

// ---------------------------------------------------------------------------
// 4. codex-auth: help/version/retired words, the dropped families, import-default.
// ---------------------------------------------------------------------------
const CODEX_PARITY: string[][] = [
  ['codex-auth'],
  ['codex-auth', '--help'],
  ['codex-auth', '--version'],
  ['codex-auth', 'use'],
  ['codex-auth', 'switch'],
  ['codex-auth', 'activate'],
  ['codex-auth', 'bogus'],
];
for (const args of CODEX_PARITY) {
  const homeA = freshHome();
  const homeB = freshHome();
  equalRuns(
    `codex ${args.slice(1).join(' ') || '(no args)'}`,
    runCli('ts', homeA, args),
    runCli('rust', homeB, args),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}
for (const sub of ['create', 'login', 'show', 'list', 'status', 'remove']) {
  const home = freshHome();
  const rust = runCli('rust', home, ['codex-auth', sub]);
  check(
    `drop codex-auth ${sub}`,
    rust.status === 1 &&
      rust.stdout === '' &&
      rust.stderr.includes(`"codex-auth ${sub}" is managed in the dashboard's Accounts page`) &&
      rust.stderr.includes('Accounts'),
    JSON.stringify(rust)
  );
  fs.rmSync(home, { recursive: true, force: true });
}

function b64url(buf: Buffer): string {
  return buf.toString('base64').replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
function makeJwt(payload: object): string {
  return `${b64url(Buffer.from(JSON.stringify({ alg: 'none' })))}.${b64url(Buffer.from(JSON.stringify(payload)))}.sig`;
}

interface ImportCase {
  name: string;
  flags?: string[];
  auth?: unknown;
  existing?: boolean;
  history?: boolean;
  missing?: boolean;
}
const IMPORT_CASES: ImportCase[] = [
  { name: 'valid import', flags: ['work'] },
  { name: 'with history', flags: ['work', '--with-history'], history: true },
  { name: 'existing without force', flags: ['work'], existing: true },
  { name: 'existing with force', flags: ['work', '--force'], existing: true },
  { name: 'cliproxy format', flags: ['work'], auth: { type: 'codex' } },
  { name: 'missing legacy auth', flags: ['work'], missing: true },
  { name: 'no profile name', flags: [] },
  { name: 'invalid profile name', flags: ['Work'] },
  { name: 'unknown flag', flags: ['work', '--wat'] },
  { name: 'extra positional', flags: ['work', 'extra'] },
];
for (const testCase of IMPORT_CASES) {
  const homeA = freshHome();
  const homeB = freshHome();
  const setup = (home: string) => {
    const ccs = path.join(home, 'ccs');
    const legacy = path.join(home, 'legacy');
    fs.mkdirSync(ccs, { recursive: true });
    fs.mkdirSync(legacy, { recursive: true });
    if (!testCase.missing) {
      const auth = testCase.auth ?? {
        tokens: { id_token: makeJwt({ email: 'work@example.test' }) },
      };
      fs.writeFileSync(path.join(legacy, 'auth.json'), JSON.stringify(auth, null, 2));
    }
    if (testCase.history) {
      fs.writeFileSync(path.join(legacy, 'history.jsonl'), '{"q":1}\n');
      fs.mkdirSync(path.join(legacy, 'sessions/sub'), { recursive: true });
      fs.writeFileSync(path.join(legacy, 'sessions/a.json'), '{}');
      fs.writeFileSync(path.join(legacy, 'sessions/sub/b.json'), '{}');
    }
    if (testCase.existing) {
      fs.writeFileSync(
        path.join(ccs, 'codex-profiles.yaml'),
        "version: '1.0'\ndefault: null\nprofiles:\n  work:\n    type: codex\n    created: '2026-01-01T00:00:00.000Z'\n    last_used: null\n"
      );
      fs.mkdirSync(path.join(ccs, 'codex-instances/work'), { recursive: true });
      fs.writeFileSync(path.join(ccs, 'codex-instances/work/auth.json'), '{"old":true}');
    }
    return { ccs, legacy };
  };
  const a = setup(homeA);
  const b = setup(homeB);
  const args = ['codex-auth', 'import-default', ...(testCase.flags ?? ['work'])];
  const ts = runCli('ts', homeA, args, { LEGACY_CODEX_HOME: a.legacy, CCS_DIR: a.ccs });
  const rust = runCli('rust', homeB, args, { LEGACY_CODEX_HOME: b.legacy, CCS_DIR: b.ccs });
  const norm = (text: string) =>
    text
      .split(homeA)
      .join('<home>')
      .split(homeB)
      .join('<home>')
      .replace(/auth\.json\.bak-\d+/g, 'auth.json.bak-<ts>');
  check(
    `import-default ${testCase.name} output`,
    ts.status === rust.status &&
      norm(ts.stdout) === norm(rust.stdout) &&
      norm(ts.stderr) === norm(rust.stderr),
    `ts=${JSON.stringify(ts)}\nrust=${JSON.stringify(rust)}`
  );
  if (ts.status === 0 && rust.status === 0) {
    const profileName = (testCase.flags ?? ['work']).find((arg) => !arg.startsWith('--')) ?? 'work';
    interface RegistryProfile {
      type?: string;
      created?: string;
      last_used?: string | null;
      email?: string;
      plan_type?: string | null;
      account_id?: string;
    }
    interface Registry {
      version?: string;
      default?: string | null;
      profiles?: Record<string, RegistryProfile>;
    }
    const strip = (registry: Registry): Registry => {
      for (const profile of Object.values(registry.profiles ?? {})) {
        delete profile.created;
        delete profile.last_used;
      }
      return registry;
    };
    // Fixture-owned YAML written by the two CLIs; the shape is the registry contract.
    const regA = strip(
      yaml.load(fs.readFileSync(path.join(a.ccs, 'codex-profiles.yaml'), 'utf8')) as Registry
    );
    const regB = strip(
      yaml.load(fs.readFileSync(path.join(b.ccs, 'codex-profiles.yaml'), 'utf8')) as Registry
    );
    check(
      `import-default ${testCase.name} registry`,
      JSON.stringify(regA) === JSON.stringify(regB),
      `ts=${JSON.stringify(regA)}\nrust=${JSON.stringify(regB)}`
    );
    const authA = fs.readFileSync(path.join(a.ccs, `codex-instances/${profileName}/auth.json`));
    const authB = fs.readFileSync(path.join(b.ccs, `codex-instances/${profileName}/auth.json`));
    check(`import-default ${testCase.name} auth bytes`, authA.equals(authB));
    const linkA = fs.readlinkSync(path.join(a.ccs, `codex-instances/${profileName}/config.toml`));
    const linkB = fs.readlinkSync(path.join(b.ccs, `codex-instances/${profileName}/config.toml`));
    check(
      `import-default ${testCase.name} config link`,
      linkA === path.join(homeA, '.codex/config.toml') &&
        linkB === path.join(homeB, '.codex/config.toml')
    );
    if (testCase.history) {
      check(
        `import-default ${testCase.name} history files`,
        fs.existsSync(path.join(a.ccs, `codex-instances/${profileName}/sessions/sub/b.json`)) &&
          fs.existsSync(path.join(b.ccs, `codex-instances/${profileName}/sessions/sub/b.json`))
      );
    }
  }
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}

// ---------------------------------------------------------------------------
// 5. antigravity: help, status, the dropped recover, runtime refresh refusals.
// ---------------------------------------------------------------------------
const ANTIGRAVITY_PARITY: string[][] = [
  ['antigravity'],
  ['antigravity', 'help'],
  ['antigravity', 'status', 'extra'],
  ['antigravity', 'activate'],
  ['antigravity', 'runtime'],
  ['antigravity', 'runtime', 'help'],
  ['antigravity', 'runtime', 'bogus'],
  ['antigravity', 'signin'],
  ['antigravity', 'signin', '-x'],
  ['antigravity', 'bogus'],
];
for (const args of ANTIGRAVITY_PARITY) {
  const homeA = freshHome();
  const homeB = freshHome();
  equalRuns(
    `antigravity ${args.slice(1).join(' ') || '(no args)'}`,
    runCli('ts', homeA, args),
    runCli('rust', homeB, args),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}
{
  // status on an empty fixture: both read the packaged release and the same empty state.
  const homeA = freshHome();
  const homeB = freshHome();
  equalRuns(
    'antigravity status empty',
    runCli('ts', homeA, ['antigravity', 'status']),
    runCli('rust', homeB, ['antigravity', 'status']),
    [homeA, homeB]
  );
  // runtime refresh with no installation: the same refusal.
  equalRuns(
    'antigravity runtime refresh empty',
    runCli('ts', homeA, ['antigravity', 'runtime', 'refresh']),
    runCli('rust', homeB, ['antigravity', 'runtime', 'refresh']),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}
{
  const home = freshHome();
  const rust = runCli('rust', home, ['antigravity', 'recover']);
  check(
    'drop antigravity recover',
    rust.status === 1 &&
      rust.stdout === '' &&
      rust.stderr.includes('"antigravity recover" is managed in the dashboard'),
    JSON.stringify(rust)
  );
  fs.rmSync(home, { recursive: true, force: true });
}
{
  // signin delegates to the labelled TS CLI while prepare/complete stay in TS (M5).
  const home = freshHome();
  const fake = path.join(home, 'fake-cli.cjs');
  fs.writeFileSync(fake, "console.log(process.argv.slice(2).join(' '));\n");
  const rust = runCli('rust', home, ['antigravity', 'signin', 'work'], { AAC_TS_CLI: fake });
  check(
    'antigravity signin delegates',
    rust.status === 0 && rust.stdout === 'antigravity signin work\n',
    JSON.stringify(rust)
  );
  fs.rmSync(home, { recursive: true, force: true });
}

// ---------------------------------------------------------------------------
// 6. bar: help, version, the non-macOS refusals and flag validation.
// ---------------------------------------------------------------------------
const BAR_PARITY: string[][] = [
  ['bar'],
  ['bar', '--help'],
  ['bar', 'help'],
  ['bar', '--version'],
  ['bar', 'launch'],
  ['bar', 'serve'],
  ['bar', 'stop'],
  ['bar', 'status'],
  ['bar', 'bogus'],
  ['bar', 'install', '--wat'],
  ['bar', 'install', '--launch', '--launch'],
  ['bar', 'install', '--launch', '--no-launch'],
  ['bar', 'install'],
  ['bar', 'uninstall'],
];
for (const args of BAR_PARITY) {
  const homeA = freshHome();
  const homeB = freshHome();
  equalRuns(
    `bar ${args.slice(1).join(' ') || '(no args)'}`,
    runCli('ts', homeA, args),
    runCli('rust', homeB, args),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}
{
  // version with a recorded pin and no installed app.
  const homeA = freshHome();
  const homeB = freshHome();
  for (const home of [homeA, homeB]) {
    fs.mkdirSync(path.join(home, 'bar'), { recursive: true });
    fs.writeFileSync(path.join(home, 'bar/.version'), '1.2.3\n');
  }
  equalRuns(
    'bar version recorded',
    runCli('ts', homeA, ['bar', 'version']),
    runCli('rust', homeB, ['bar', 'version']),
    [homeA, homeB]
  );
  fs.rmSync(homeA, { recursive: true, force: true });
  fs.rmSync(homeB, { recursive: true, force: true });
}

console.log(`\n${cases - failures}/${cases} G19 CLI parity cases passed.`);
process.exit(failures ? 1 : 0);
