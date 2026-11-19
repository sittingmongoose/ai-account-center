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
  ['usage-hub', ['dashboard', 'usage-hub', 'status']],
  ['usage-hub', ['dashboard', 'usage-hub', 'generate', '--stdout']],
  ['usage-hub', ['dashboard', 'usage-hub', 'rotate']],
  ['usage-hub', ['dashboard', 'usage-hub', 'off']],
  ['usage-snapshot', ['dashboard', 'usage-snapshot', 'status']],
  ['usage-snapshot', ['dashboard', 'usage-snapshot', 'set', '/tmp/x']],
  ['usage-snapshot', ['dashboard', 'usage-snapshot', 'off']],
  ['cursor-key', ['dashboard', 'cursor-key']],
  ['cursor-key', ['dashboard', 'cursor-key', 'set']],
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

console.log(`\n${cases - failures}/${cases} G19 CLI parity cases passed.`);
process.exit(failures ? 1 : 0);
