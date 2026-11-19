/**
 * R4 owned share readers: the cross-language parity corpus. The real TypeScript readers
 * (AgentReportsReader, QuotaReportReader, PmMailRegistryReader, StatusDocsReader,
 * CatalogReader) scan a scripted virtual share — every step a full filesystem image with
 * injected ENOENT/ESTALE/EACCES failures — and their snapshots and change flags are pinned
 * in `tests/spec/share/cases.json`. `crates/aac-status/tests/share_parity.rs` replays the
 * same steps against the Rust readers and requires the same bytes (`js_text` writes numbers
 * and key order as JavaScript does).
 *
 *   bun tests/spec/share-read-parity.ts --regenerate   # rewrite the corpus (review the diff)
 *   bun tests/spec/share-read-parity.ts                # check the corpus against the readers
 *
 * The status-documents expectations need the aac-status native module (the projection the
 * TypeScript wrapper calls); without it they are null and the Rust test skips them. The
 * build host (Nas1) regenerates the full corpus. Fixture data only: no real share is read.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../..', import.meta.url));
const args = process.argv.slice(2);
const regenerate = args.includes('--regenerate');
const corpusFile = path.join(root, 'tests/spec/share/cases.json');

for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
process.env.AAC_NO_LOCAL_RUST_BUILD = '1';

// Dynamic imports: the readers capture their environment at module load (the harness clears
// CCS_* and forbids local Rust builds first), as the other spec harnesses do.
const { AgentReportsReader } = await import('../../src/web-server/status/agent-reports-reader');
const { QuotaReportReader, QUOTA_REPORT_CAP } = await import('../../src/web-server/status/quota-report');
const { PmMailRegistryReader } = await import('../../src/web-server/status/status-collectors');
const { StatusDocsReader } = await import('../../src/web-server/status/status-docs');
const { CatalogReader } = await import('../../src/web-server/scoreboard/catalog');
const { readRegularFile } = await import('../../src/web-server/status/share-read');
const { threadDoc, tasksDoc, cardDoc, item, NOW, ago } = await import(
  '../unit/web-server/status/status-test-helpers'
);
const { quotaDocument } = await import('./read-view-fixtures');

let statusHandle: { call(method: string, args: string): Promise<string> } | null = null;
try {
  const { openStatusHandle } = await import('../../src/status-native/pinned');
  const opened = openStatusHandle();
  if (!('unavailable' in opened)) statusHandle = opened;
} catch {
  statusHandle = null;
}

const ROOT = '/fixture/agent-reports';
const REGDIR = '/fixture/registry';
const W1 = 'vm.lead';
const W2 = 'mac.dev';
const W3 = 'nas1.bot';

// ------------------------------------------------------------------------------------
// The virtual share: one image per step, in listing (insertion) order.

interface Node {
  type: 'file' | 'dir' | 'link' | 'fifo';
  content?: string;
  mtime?: number;
  ino?: number;
  ctime?: number;
  size?: number;
  target?: string;
}
type Tree = Array<[string, Node]>;
interface Errors {
  [path: string]: { list?: string; stat?: string; read?: string };
}

let ino = 100;
const dir = (): Node => ({ type: 'dir' });
const file = (content: string, fields: Partial<Node> = {}): Node => ({
  type: 'file',
  content,
  mtime: NOW - 60_000,
  ctime: NOW - 60_000,
  ino: ino++,
  ...fields,
});
const json = (value: unknown, fields: Partial<Node> = {}): Node =>
  file(`${JSON.stringify(value, null, 2)}\n`, fields);

function makeError(code: string): Error & { code?: string; errno?: number } {
  if (code === 'ESTALE') {
    return Object.assign(new Error('Unknown system error -116, open'), {
      code: 'UNKNOWN',
      errno: -116,
    });
  }
  return Object.assign(new Error(`${code} fixture`), { code });
}

let tree: Map<string, Node> = new Map();
let errors: Errors = {};

function resolve(file: string): Node | undefined {
  const node = tree.get(file);
  if (node?.type === 'link' && node.target) return resolve(node.target);
  return node;
}

const fakeReaddir = async (dirPath: string, options?: { withFileTypes?: boolean }) => {
  // The virtual tree is POSIX; path.join spells Windows separators on a Windows host.
  dirPath = dirPath.replace(/\\/g, '/');
  const injected = errors[dirPath]?.list;
  if (injected) throw makeError(injected);
  const node = tree.get(dirPath);
  if (!node) throw makeError('ENOENT');
  if (node.type !== 'dir') throw makeError('ENOTDIR');
  const prefix = `${dirPath}/`;
  const names: Array<{ name: string; node: Node }> = [];
  for (const [key, value] of tree) {
    if (!key.startsWith(prefix)) continue;
    const rest = key.slice(prefix.length);
    if (rest.includes('/')) continue;
    names.push({ name: rest, node: value });
  }
  if (options?.withFileTypes) {
    return names.map(({ name, node }) => ({
      name,
      isDirectory: () => node.type === 'dir',
      isFile: () => node.type === 'file',
      isSymbolicLink: () => node.type === 'link',
    }));
  }
  return names.map(({ name }) => name);
};

const fakeStat = async (file: string) => {
  file = file.replace(/\\/g, '/');
  const injected = errors[file]?.stat;
  if (injected) throw makeError(injected);
  const node = resolve(file);
  if (!node) throw makeError('ENOENT');
  return {
    size: node.size ?? Buffer.byteLength(node.content ?? ''),
    mtimeMs: node.mtime ?? NOW - 60_000,
    ctimeMs: node.ctime ?? node.mtime ?? NOW - 60_000,
    ino: node.ino ?? 0,
    dev: 1,
    isFile: () => node.type === 'file',
    isDirectory: () => node.type === 'dir',
  };
};

const fakeOpen = async (file: string, flags: number) => {
  file = file.replace(/\\/g, '/');
  const injected = errors[file]?.read;
  if (injected) throw makeError(injected);
  const node = tree.get(file);
  if (!node) throw makeError('ENOENT');
  const noFollow = (flags & (fs.constants.O_NOFOLLOW ?? 0)) !== 0;
  if (noFollow && node.type === 'link') throw makeError('ELOOP');
  const bytes = Buffer.from(node.content ?? '');
  return {
    fd: 999,
    stat: async () => fakeStat(file),
    read: async (buffer: Buffer, offset: number, length: number, position: number) => {
      const slice = bytes.subarray(position, position + length);
      slice.copy(buffer, offset);
      return { bytesRead: slice.length, buffer };
    },
    close: async () => undefined,
  };
};

// Patch the one filesystem layer every reader uses (readRegularFile opens through it).
const realReaddir = fs.promises.readdir;
const realStat = fs.promises.stat;
const realOpen = fs.promises.open;
(fs.promises as unknown as Record<string, unknown>).readdir = fakeReaddir;
(fs.promises as unknown as Record<string, unknown>).stat = fakeStat;
(fs.promises as unknown as Record<string, unknown>).open = fakeOpen;

const readerFs = {
  readdir: (dir: string) => fakeReaddir(dir, { withFileTypes: true }),
  stat: fakeStat,
  readFile: (file: string, capBytes: number) => readRegularFile(file, capBytes),
};

let currentNow = NOW;
const reports = new AgentReportsReader({ root: () => ROOT, now: () => currentNow, fs: readerFs });
const quota = new QuotaReportReader({
  root: () => ROOT,
  now: () => currentNow,
  fs: {
    readdir: (dir: string) => fakeReaddir(dir) as Promise<string[]>,
    stat: fakeStat,
    readFile: (file: string) => readRegularFile(file, QUOTA_REPORT_CAP),
  },
});
const registry = new PmMailRegistryReader(() => REGDIR);
const docs = new StatusDocsReader({
  root: () => ROOT,
  now: () => currentNow,
  fs: readerFs,
  handle: () => statusHandle as never,
});
const catalog = new CatalogReader({ root: () => ROOT, now: () => currentNow, fs: readerFs });

const stringify = (value: unknown): string =>
  JSON.stringify(value, (_key, item: unknown) =>
    item instanceof Map ? Object.fromEntries(item) : item
  );

interface Step {
  name: string;
  now: number;
  tree: Tree;
  errors?: Errors;
  limits?: { writers?: number; cards?: number; bytes?: number };
  docsLimits?: { writers?: number; retained?: number };
}

interface Expect {
  reports: string;
  reportsChanged: boolean;
  quota: string;
  quotaChanged: boolean;
  registry: unknown;
  docs: string | null;
  docsChanged: boolean | null;
  catalog: string;
}

const steps: Step[] = [];
const expect: Expect[] = [];

function step(name: string, tree: Tree, options: Partial<Step> = {}) {
  steps.push({ name, now: NOW, tree, ...options });
}

// The base writers folder with one good thread, tasks and card.
const baseTree = (extra: Tree = []): Tree => [
  [`${ROOT}/v1`, dir()],
  [`${ROOT}/v1/writers`, dir()],
  [`${ROOT}/v1/writers/${W1}`, dir()],
  [`${ROOT}/v1/writers/${W1}/thread.json`, json(threadDoc(W1))],
  [`${ROOT}/v1/writers/${W1}/tasks.json`, json(tasksDoc(W1, [item('t-1', 'working')]))],
  [`${ROOT}/v1/writers/${W1}/cards`, dir()],
  [`${ROOT}/v1/writers/${W1}/cards/${W1}--q1.json`, json(cardDoc(W1, 'q1'))],
  ...extra,
];

const quotaTree = (doc: unknown, fields: Partial<Node> = {}): Tree => [
  [`${ROOT}/v1/quota`, dir()],
  [`${ROOT}/v1/quota/report.json`, json(doc, fields)],
];

const catalogTree = (extra: Tree = []): Tree => [
  [`${ROOT}/v1/catalog`, dir()],
  [
    `${ROOT}/v1/catalog/models.json`,
    json({
      schema: 'catalog-models@1',
      models: [
        {
          model: 'openai/gpt-6.1-sol',
          label: 'GPT-6.1 Sol',
          family: 'gpt',
          runtimes: ['codex'],
          efforts: ['high', 'max'],
          prices: null,
          notes: null,
        },
      ],
    }),
  ],
  [
    `${ROOT}/v1/catalog/task-types.json`,
    json({
      schema: 'catalog-task-types@1',
      taskTypes: [
        { id: 'review', label: 'Review', description: null, unit: 'task', measures: [], byProcess: false },
      ],
    }),
  ],
  ...extra,
];

const registryTree = (extra: Tree = []): Tree => [
  [REGDIR, dir()],
  [
    `${REGDIR}/lead.json`,
    json({ name: 'lead', kind: 'impl', machine: 'ubuntuserver', cwd: '/home/x', about: 'Working', last_seen: ago(30_000) }),
  ],
  ...extra,
];

// ------------------------------------------------------------------------------------
// The scenarios.

step('empty share', []);
step('one writer accepted', baseTree());
step(
  'second writer and a status document',
  baseTree([
    [`${ROOT}/v1/writers/${W2}`, dir()],
    [`${ROOT}/v1/writers/${W2}/thread.json`, json(threadDoc(W2, { seq: 3, state: 'waiting' }))],
    [
      `${ROOT}/v1/writers/${W1}/status.json`,
      json({
        schema: 'agent-reports/status@1',
        writer: W1,
        seq: 2,
        writtenAt: ago(30_000),
        t3Thread: 'thread-1',
        summary: 'Half done.',
        percent: 50,
        done: [{ id: 'd1', title: 'Done one' }],
        left: [],
        blockers: [],
        looping: null,
        title: 'A status',
      }),
    ],
  ])
);
step(
  'seq went back on the thread',
  baseTree([
    [`${ROOT}/v1/writers/${W1}/thread.json`, json(threadDoc(W1, { seq: 0, doing: 'Older' }), { mtime: NOW - 30_000, ino: 9001 })],
  ])
);
step(
  'same bytes again keep firstSeenAt',
  baseTree([[`${ROOT}/v1/writers/${W1}/thread.json`, json(threadDoc(W1), { mtime: NOW - 10_000, ino: 9002 })]])
);
step(
  'refusals: bad JSON, NUL, secret, schema, writer, cap',
  baseTree([
    [`${ROOT}/v1/writers/${W2}`, dir()],
    [`${ROOT}/v1/writers/${W2}/thread.json`, file('{not json')],
    [`${ROOT}/v1/writers/${W2}/tasks.json`, file('{"a":1}\u0000')],
    [`${ROOT}/v1/writers/${W2}/cards`, dir()],
    [`${ROOT}/v1/writers/${W2}/cards/${W2}--s1.json`, json(cardDoc(W2, 's1', { title: 'key sk-abcdefghijklmnopqrstuvwxyz123456' }))],
    [`${ROOT}/v1/writers/${W3}`, dir()],
    [`${ROOT}/v1/writers/${W3}/thread.json`, json({ ...threadDoc(W3), schema: 'agent-reports/thread@2' })],
    [`${ROOT}/v1/writers/${W3}/tasks.json`, json({ ...tasksDoc(W3, []), writer: 'vm.other' })],
    [`${ROOT}/v1/writers/${W3}/cards`, dir()],
    [`${ROOT}/v1/writers/${W3}/cards/${W3}--big.json`, file('x'.repeat(65 * 1024), { size: 65 * 1024 })],
  ])
);
step(
  'card id rules',
  baseTree([
    [`${ROOT}/v1/writers/${W1}/cards/${W1}--bad id.json`, json(cardDoc(W1, 'bad id'))],
    [`${ROOT}/v1/writers/${W1}/cards/${W2}--x.json`, json(cardDoc(W2, 'x'))],
    [`${ROOT}/v1/writers/${W1}/cards/${W1}--one.json`, json(cardDoc(W1, 'one', { options: [{ key: 'A', label: 'Only' }] }))],
  ])
);
step(
  'transient stat error once is silent',
  baseTree(),
  { errors: { [`${ROOT}/v1/writers/${W1}/thread.json`]: { stat: 'ESTALE' } } }
);
step(
  'transient stat error twice is named',
  baseTree(),
  { errors: { [`${ROOT}/v1/writers/${W1}/thread.json`]: { stat: 'ESTALE' } } }
);
step(
  'the file reads again and clears the problem',
  baseTree([[`${ROOT}/v1/writers/${W1}/thread.json`, json(threadDoc(W1, { seq: 2 }), { mtime: NOW, ino: 9100 })]])
);
step(
  'listing failure names the folder rows at once (EACCES)',
  baseTree(),
  { errors: { [`${ROOT}/v1/writers/${W1}`]: { list: 'EACCES' } } }
);
step(
  'transient listing failure once is silent',
  baseTree(),
  { errors: { [`${ROOT}/v1/writers/${W1}`]: { list: 'ESTALE' } } }
);
step(
  'transient listing failure twice is named',
  baseTree(),
  { errors: { [`${ROOT}/v1/writers/${W1}`]: { list: 'ESTALE' } } }
);
step(
  'removed file kept through the grace',
  baseTree().filter(([key]) => key !== `${ROOT}/v1/writers/${W1}/cards/${W1}--q1.json`),
  { now: NOW + 60_000 }
);
step(
  'removed file gone after the grace',
  baseTree().filter(([key]) => key !== `${ROOT}/v1/writers/${W1}/cards/${W1}--q1.json`),
  { now: NOW + 180_001 }
);
step(
  'fifo and link where a card belongs',
  baseTree([
    [`${ROOT}/v1/writers/${W1}/cards/${W1}--fifo.json`, { type: 'fifo' }],
    [`${ROOT}/v1/writers/${W1}/cards/${W1}--link.json`, { type: 'link', target: `${ROOT}/v1/writers/${W1}/thread.json` }],
  ])
);
step(
  'writer limit keeps the newest folders',
  baseTree([
    [`${ROOT}/v1/writers/${W2}`, dir()],
    [`${ROOT}/v1/writers/${W2}/thread.json`, json(threadDoc(W2))],
    [`${ROOT}/v1/writers/${W3}`, dir()],
    [`${ROOT}/v1/writers/${W3}/thread.json`, json(threadDoc(W3))],
  ]),
  { limits: { writers: 2 } }
);
step(
  'card and byte limits',
  baseTree([
    [`${ROOT}/v1/writers/${W1}/cards/${W1}--q2.json`, json(cardDoc(W1, 'q2'))],
    [`${ROOT}/v1/writers/${W1}/cards/${W1}--q3.json`, json(cardDoc(W1, 'q3'))],
  ]),
  { limits: { cards: 2, bytes: 4096 } }
);
step(
  'boards stat inside the share only',
  baseTree([
    [
      `${ROOT}/v1/writers/${W1}/tasks.json`,
      json(tasksDoc(W1, [item('t-1', 'working')], { board: '/mnt/Cursor/share/board.md' })),
    ],
    [
      `${ROOT}/v1/writers/${W1}/thread.json`,
      json(threadDoc(W1, { links: [{ label: 'Board', path: '\\\\TRUENAS\\Cursor\\share\\other.md' }, { label: 'doc', path: '/mnt/Cursor/share/x.md' }] })),
    ],
    ['/fixture/share', dir()],
    ['/fixture/share/board.md', file('# board', { mtime: NOW - 5_000 })],
  ])
);
step(
  'non-writer folders and dotfiles',
  baseTree([
    [`${ROOT}/v1/writers/NotAWriter`, dir()],
    [`${ROOT}/v1/writers/.hidden`, dir()],
    [`${ROOT}/v1/writers/${W1}/cards/.dot.json`, json(cardDoc(W1, 'dot'))],
  ])
);
step('quota report accepted', [...baseTree(), ...quotaTree(quotaDocument())]);
step(
  'quota older seq ignored',
  [...baseTree(), ...quotaTree(quotaDocument({ seq: 0 }), { mtime: NOW, ino: 9200 })]
);
step(
  'quota refusal keeps the last good copy',
  [...baseTree(), ...quotaTree({ schema: 'quota-report@2' }, { mtime: NOW + 1000, ino: 9201 })]
);
step(
  'quota missing through the grace then gone',
  baseTree(),
  { now: NOW + 180_002 }
);
step(
  'registry rows and heuristics',
  [
    ...baseTree(),
    ...registryTree([
      [`${REGDIR}/mac-thread.json`, json({ name: 'mac-thread', kind: 'review', machine: 'Jareds-MacBook', cwd: '/Users/x', about: 'Reviewing', last_seen: ago(1000) })],
      [`${REGDIR}/win.json`, json({ name: 'win', machine: 'SomePC', cwd: 'C:\\work', about: 'x'.repeat(400), last_seen: '2026-02-30T00:00:00Z' })],
      [`${REGDIR}/nas.json`, json({ name: 'nas', machine: '', cwd: '/data/share', about: '  pad  ', last_seen: ago(0) })],
      [`${REGDIR}/not-a-thread.json`, json({ other: true })],
      [`${REGDIR}/bad-name.json`, json({ name: 'bad name' })],
      [`${REGDIR}/secret.json`, json({ name: 'secret', about: 'token sk-abcdefghijklmnopqrstuvwxyz123456' })],
      [`${REGDIR}/.dot.json`, json({ name: 'dot' })],
    ]),
  ]
);
step(
  'registry file refused then read again',
  [...baseTree(), ...registryTree()],
  { errors: { [`${REGDIR}/lead.json`]: { read: 'EACCES' } } }
);
step('registry file reads again', [...baseTree(), ...registryTree()]);
step(
  'registry folder missing',
  baseTree(),
  { errors: { [REGDIR]: { list: 'ENOENT' } } }
);
step(
  'catalog accepted with aliases and measures',
  [
    ...baseTree(),
    ...catalogTree([
      [
        `${ROOT}/v1/catalog/model-aliases.json`,
        json({
          schema: 'agent-reports/model-aliases@1',
          aliases: { 'gpt-6.1-sol': 'openai/gpt-6.1-sol' },
          effortSuffixes: ['-high', '-max', '-high'],
        }),
      ],
      [
        `${ROOT}/v1/catalog/measures.json`,
        json({
          schema: 'catalog-measures@1',
          minRuns: 3,
          measures: [
            { id: 'quality', label: 'Quality', better: 'higher', unit: '%', description: null, metric: { of: 'reviews', per: 'run' }, preliminaryUnless: null },
          ],
        }),
      ],
    ]),
  ]
);
step(
  'catalog refusal keeps the last good copy',
  [
    ...baseTree(),
    ...catalogTree([
      [`${ROOT}/v1/catalog/models.json`, file('{bad', { mtime: NOW, ino: 9300 })],
    ]),
  ]
);
step(
  'status document seq went back',
  baseTree([
    [
      `${ROOT}/v1/writers/${W1}/status.json`,
      json({
        schema: 'agent-reports/status@1',
        writer: W1,
        seq: 5,
        writtenAt: ago(30_000),
        t3Thread: 'thread-1',
        summary: 'Newer.',
        done: [],
        left: [],
        blockers: [],
      }),
    ],
  ])
);
step(
  'status document older copy',
  baseTree([
    [
      `${ROOT}/v1/writers/${W1}/status.json`,
      json({
        schema: 'agent-reports/status@1',
        writer: W1,
        seq: 4,
        writtenAt: ago(10_000),
        t3Thread: 'thread-1',
        summary: 'Older.',
        done: [],
        left: [],
        blockers: [],
      }, { mtime: NOW, ino: 9400 }),
    ],
  ])
);
step(
  'status document v2 refusal',
  baseTree([
    [
      `${ROOT}/v1/writers/${W1}/status.json`,
      json({
        schema: 'agent-reports/status@1',
        writer: W1,
        seq: 6,
        writtenAt: ago(5_000),
        t3Thread: 'thread-1',
        summary: 'Bad stages.',
        done: [],
        left: [],
        blockers: [],
        stages: [{ label: 'only', state: 'now' }],
      }, { mtime: NOW + 1000, ino: 9401 }),
    ],
  ])
);

// ------------------------------------------------------------------------------------
// Run the readers over the steps and pin their answers.

for (const [index, step] of steps.entries()) {
  currentNow = step.now;
  tree = new Map(step.tree);
  errors = step.errors ?? {};
  // The limits are the reader's own; rebuild the readers when a step narrows them.
  const reportsReader = step.limits
    ? new AgentReportsReader({ root: () => ROOT, now: () => currentNow, fs: readerFs, limits: step.limits })
    : reports;
  const docsReader = step.docsLimits
    ? new StatusDocsReader({ root: () => ROOT, now: () => currentNow, fs: readerFs, handle: () => statusHandle as never, limits: step.docsLimits })
    : docs;
  const reportsChanged = await reportsReader.scan();
  const quotaChanged = await quota.scan();
  const docsChanged = statusHandle ? await docsReader.scan() : null;
  let registryAnswer: unknown;
  try {
    const scan = await registry.scan(step.now);
    registryAnswer = { threads: scan.threads, problems: scan.problems, changed: scan.changed };
  } catch (error) {
    registryAnswer = { error: (error as Error).message };
  }
  await catalog.scan();
  expect.push({
    reports: stringify(reportsReader.snapshot()),
    reportsChanged,
    quota: stringify(quota.snapshot()),
    quotaChanged,
    registry: registryAnswer,
    docs: statusHandle ? stringify(docsReader.snapshot()) : null,
    docsChanged,
    catalog: stringify(catalog.snapshot()),
  });
  void index;
}

const corpus = {
  version: 1,
  root: ROOT,
  registryDir: REGDIR,
  nativeStatusDocs: statusHandle !== null,
  steps: steps.map((step, index) => ({
    name: step.name,
    now: step.now,
    tree: step.tree,
    errors: step.errors ?? {},
    limits: step.limits ?? null,
    docsLimits: step.docsLimits ?? null,
    expect: expect[index],
  })),
};

if (regenerate) {
  fs.mkdirSync(path.dirname(corpusFile), { recursive: true });
  fs.writeFileSync(corpusFile, `${JSON.stringify(corpus, null, 2)}\n`);
  console.log(`wrote ${corpusFile} (${steps.length} steps, native status docs: ${statusHandle !== null})`);
} else {
  const held = JSON.parse(fs.readFileSync(corpusFile, 'utf8'));
  const ran = JSON.parse(JSON.stringify(corpus));
  // The corpus is recorded with the native module (the status-documents projection the
  // TypeScript wrapper calls). Without it this run has no status-documents answers, so the
  // check compares every other reader and leaves the docs expectations to a module run.
  const docsChecked = statusHandle !== null || held.nativeStatusDocs !== true;
  if (!docsChecked) {
    for (const doc of [held, ran]) {
      delete doc.nativeStatusDocs;
      for (const step of doc.steps) {
        delete step.expect.docs;
        delete step.expect.docsChanged;
      }
    }
  }
  assert.deepEqual(ran, held, 'the readers changed; review and --regenerate');
  console.log(
    `share-read parity: ${steps.length} steps match the corpus` +
      (docsChecked ? '' : ' (status documents not checked: no native module; set AAC_STATUS_MODULE)')
  );
}

// Restore the real filesystem for any later import.
(fs.promises as unknown as Record<string, unknown>).readdir = realReaddir;
(fs.promises as unknown as Record<string, unknown>).stat = realStat;
(fs.promises as unknown as Record<string, unknown>).open = realOpen;
