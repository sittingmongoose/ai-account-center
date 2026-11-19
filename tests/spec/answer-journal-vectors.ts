/**
 * G16b: what the TS answer store (src/web-server/status/answers.ts, StatusAnswers) writes and folds,
 * for the Rust store (crates/aac-status/src/answers_engine.rs) to reproduce byte for byte. Each
 * scenario is a script of operations (share files, the clock, journal faults, the doorbell, sends,
 * retry passes, a fresh instance on the same store) run against the real StatusAnswers over
 * temporary roots; each `check` records the exact journal bytes, every file under `v1/keys` and
 * `v1/answers`, the rings, the folded history (entries, sends, delivery views) at that moment.
 * `crates/aac-status/tests/answer_journal.rs` replays the same script and compares each check.
 *
 * Ids are pinned (randomUUID numbered, the 8-byte journal line ids numbered) and the key is a fixed
 * seed, so the file is the same on every run and the Rust replay uses the same values. The fault
 * hooks patch fs for the journal's descriptors only, as status-answer-worker.ts does.
 *
 *   bun tests/spec/answer-journal-vectors.ts          # check tests/spec/answer-journal-vectors.json
 *   bun tests/spec/answer-journal-vectors.ts --write  # regenerate it (review the diff)
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

process.env.AAC_STATUS_NATIVE = 'off';
delete process.env.AAC_STATUS_MODULE;
delete process.env.AAC_STATUS_DOORBELL;

// Pinned ids: before answers.ts binds its crypto imports.
// eslint-disable-next-line @typescript-eslint/no-require-imports
const crypto = require('crypto');
let uuids = 0;
let lines = 0;
crypto.randomUUID = () => `00000000-0000-4000-8000-${(uuids += 1).toString(16).padStart(12, '0')}`;
const realRandomBytes = crypto.randomBytes;
crypto.randomBytes = (size: number, ...rest: unknown[]) =>
  size === 8
    ? Buffer.from((lines += 1).toString(16).padStart(16, '0'), 'hex')
    : realRandomBytes(size, ...rest);

// Journal faults, as status-answer-worker.ts injects them.
type Match = { type: string; state?: string } | null;
const faults: { writeOnce: Match; writeAlways: Match; fsyncOnce: Match; voidAlways: boolean } = {
  writeOnce: null,
  writeAlways: null,
  fsyncOnce: null,
  voidAlways: false,
};
const journalFds = new Set<number>();
const lastData = new Map<number, string>();
const realOpen = fs.openSync;
const realClose = fs.closeSync;
const realWrite = fs.writeSync;
const realFsync = fs.fsyncSync;
const matches = (m: Match, data: string) =>
  !!m &&
  data.includes(`"type":"${m.type}"`) &&
  (m.state === undefined || data.includes(`"state":"${m.state}"`));
fs.openSync = ((file: fs.PathLike, ...args: unknown[]) => {
  const fd = (realOpen as (...a: unknown[]) => number)(file, ...args);
  if (String(file).endsWith(`${path.sep}answers.jsonl`) && args[0] === 'a+') journalFds.add(fd);
  return fd;
}) as typeof fs.openSync;
fs.closeSync = ((fd: number) => {
  journalFds.delete(fd);
  realClose(fd);
}) as typeof fs.closeSync;
fs.writeSync = ((fd: number, data: unknown, ...args: unknown[]) => {
  if (journalFds.has(fd)) {
    const text = Buffer.from(data as Buffer).toString();
    if (faults.voidAlways && text.includes('"type":"void"'))
      throw Object.assign(Error('Injected void line write error'), { code: 'EIO' });
    if (!text.includes('"type":"void"')) {
      if (matches(faults.writeOnce, text)) {
        faults.writeOnce = null;
        throw Object.assign(Error('Injected journal write error'), { code: 'EIO' });
      }
      if (matches(faults.writeAlways, text))
        throw Object.assign(Error('Injected journal write error'), { code: 'EIO' });
    }
    lastData.set(fd, text);
  }
  return (realWrite as (...a: unknown[]) => number)(fd, data, ...args);
}) as typeof fs.writeSync;
fs.fsyncSync = ((fd: number) => {
  if (journalFds.has(fd) && matches(faults.fsyncOnce, lastData.get(fd) ?? '')) {
    faults.fsyncOnce = null;
    throw Object.assign(Error('Injected journal fsync error'), { code: 'EIO' });
  }
  return realFsync(fd);
}) as typeof fs.fsyncSync;

const { AgentReportsReader } = await import('../../src/web-server/status/agent-reports-reader');
const answersModule = await import('../../src/web-server/status/answers');
const { StatusAnswers, deliveryView, summarySteps } = answersModule;
const { parseAnswerEntry, normalizeNote } = await import(
  '../../src/web-server/status/answer-entries'
);
const { findSecretIn } = await import('../../src/web-server/status/schema-helpers');
const { findTypedSecret } = await import('../../src/web-server/status/typed-secrets');
const { canonicalJson } = await import('../../src/web-server/status/answer-signing');
const helpers = await import('../unit/web-server/status/status-test-helpers');
const { cardDoc, NOW } = helpers;

/** The fixed answer key: PKCS#8 v1 over a fixed seed (the bytes Node writes). */
const SEED = Buffer.alloc(32, 0x5a);
const KEY_PEM = `-----BEGIN PRIVATE KEY-----\n${Buffer.concat([
  Buffer.from('302e020100300506032b657004220420', 'hex'),
  SEED,
]).toString('base64')}\n-----END PRIVATE KEY-----\n`;

const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-answer-journal-'));

type Op = Record<string, unknown> & { op: string };
type Ring = { to: string; subject: string; body: string };

/** The note's secret finding, as the TS route's admission computes it for the Rust store. */
function noteSecret(raw: unknown): string | null {
  const entry = parseAnswerEntry(raw);
  if (!entry) return null;
  const note = normalizeNote(entry.note);
  if (!note) return null;
  return findSecretIn(note, note) ?? findTypedSecret(note) ?? null;
}

function shareFiles(root: string): Record<string, string> {
  const out: Record<string, string> = {};
  const walk = (dir: string, rel: string) => {
    if (!fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) return;
    for (const name of fs.readdirSync(dir).sort()) {
      const full = path.join(dir, name);
      const relName = `${rel}/${name}`;
      if (fs.statSync(full).isDirectory()) walk(full, relName);
      else out[relName] = fs.readFileSync(full, 'utf8');
    }
  };
  walk(path.join(root, 'v1', 'keys'), 'keys');
  walk(path.join(root, 'v1', 'answers'), 'answers');
  return out;
}

async function settle(): Promise<void> {
  for (let i = 0; i < 10; i += 1) await new Promise((resolve) => setTimeout(resolve, 5));
}

async function runScenario(name: string, ops: Op[]) {
  uuids = 0;
  lines = 0;
  faults.writeOnce = faults.writeAlways = faults.fsyncOnce = null;
  faults.voidAlways = false;
  const root = path.join(scratch, name, 'reports');
  const store = path.join(scratch, name, 'store', 'status');
  fs.mkdirSync(path.join(root, 'v1'), { recursive: true });
  fs.mkdirSync(store, { recursive: true });
  fs.writeFileSync(path.join(store, 'answer-key.pem'), KEY_PEM, { mode: 0o600 });
  const clock = { now: NOW };
  const rings: Ring[] = [];
  const ringFail = { on: false };
  const make = () => {
    const reader = new AgentReportsReader({ root: () => root, now: () => clock.now });
    return new StatusAnswers({
      readCard: (id) => reader.readCardNow(id),
      storeDir: () => store,
      reportsRoot: () => root,
      doorbell: async (to: string, subject: string, body: string) => {
        rings.push({ to, subject, body });
        return ringFail.on
          ? { ok: false, detail: 'pm-mail failed.' }
          : { ok: true, detail: `Rang ${to} through pm-mail.` };
      },
      now: () => clock.now,
    });
  };
  let answers = make();
  for (const op of ops) {
    switch (op.op) {
      case 'write': {
        const file = path.join(root, op.path as string);
        fs.mkdirSync(path.dirname(file), { recursive: true });
        fs.writeFileSync(file, op.text as string);
        break;
      }
      case 'remove':
        fs.rmSync(path.join(root, op.path as string), { recursive: true, force: true });
        break;
      case 'clock':
        clock.now = op.now as number;
        break;
      case 'ring':
        ringFail.on = op.fail as boolean;
        break;
      case 'faults':
        Object.assign(faults, op.faults);
        break;
      case 'submit': {
        const entries = op.entries as unknown[];
        op.noteSecrets = entries.map(noteSecret);
        op.expect = await answers.submitSet(
          entries,
          op.device as string,
          (op.clientSendId as string | undefined) ?? null
        );
        await settle();
        break;
      }
      case 'retry':
        await answers.retryPending();
        await settle();
        break;
      case 'reopen':
        answers = make();
        break;
      case 'check': {
        const list = answers.list();
        const sends = answers.sendList();
        const summaries = summarySteps(sends);
        op.expect = JSON.parse(
          JSON.stringify({
            journal: fs.existsSync(path.join(store, 'answers.jsonl'))
              ? fs.readFileSync(path.join(store, 'answers.jsonl'), 'utf8')
              : null,
            files: shareFiles(root),
            rings,
            entries: list,
            sends,
            views: list.map((entry) => deliveryView(entry, summaries)),
          })
        );
        break;
      }
      default:
        throw new Error(`unknown op ${op.op}`);
    }
  }
  return { name, key: KEY_PEM, ops };
}

const cardFile = (doc: ReturnType<typeof cardDoc>) => ({
  op: 'write',
  path: `v1/writers/${doc.writer}/cards/${doc.id}.json`,
  text: `${JSON.stringify(doc, null, 2)}\n`,
});
const sha = (doc: ReturnType<typeof cardDoc>) =>
  crypto.createHash('sha256').update(`${JSON.stringify(doc, null, 2)}\n`).digest('hex');
const entry = (doc: ReturnType<typeof cardDoc>, fields: Record<string, unknown> = {}) => ({
  cardId: doc.id,
  cardSeq: doc.seq,
  cardSha256: sha(doc),
  choice: ['A'],
  via: 'deck',
  ...fields,
});
const check = () => ({ op: 'check' });
const scenarios: unknown[] = [];

{
  const q1 = cardDoc('vm.lead', 'Q1', { seq: 3 });
  const q2 = cardDoc('vm.lead', 'Q2');
  const q3 = cardDoc('mac.worker', 'Q3', { kind: 'objection', options: [], recommendation: null });
  const q4 = cardDoc('mac.silent', 'Q4', { replyTo: { pmMail: null, t3Thread: null } });
  scenarios.push(
    await runScenario('batch', [
      cardFile(q1),
      cardFile(q2),
      cardFile(q3),
      cardFile(q4),
      {
        op: 'submit',
        device: 'Mac',
        entries: [
          entry(q1, { choice: ['B'], note: '  Keep it inside — test at 320 px.\n  Thanks ' }),
          entry(q2, { choice: [], kind: 'note', note: 'Use the other approach.' }),
          entry(q3, { choice: [], kind: 'object', note: 'Not yet: blocked\r\nby X.' }),
          entry(q4),
        ],
      },
      check(),
    ])
  );
}

{
  const sample = cardDoc('vm.lead', 'S1', { kind: 'sample' });
  const judged = cardDoc('vm.lead', 'J1', { kind: 'judgement' });
  const action = cardDoc('vm.lead', 'A1', { kind: 'action', options: [], recommendation: null });
  const objection = cardDoc('vm.lead', 'O1', {
    kind: 'objection',
    options: [],
    recommendation: null,
  });
  const plain = cardDoc('vm.lead', 'P1');
  scenarios.push(
    await runScenario('refusals', [
      cardFile(sample),
      cardFile(judged),
      cardFile(action),
      cardFile(objection),
      cardFile(plain),
      {
        op: 'submit',
        device: 'iPhone',
        entries: [
          { cardId: plain.id, cardSeq: 1, choice: ['A'], via: 'deck' },
          { ...entry(plain), extra: true },
          entry(plain, { cardSha256: 'f'.repeat(64) }),
          entry(plain, { cardSeq: 2 }),
          entry(plain, { choice: ['C'] }),
          entry(plain, { choice: ['A', 'B'] }),
          entry(sample),
          entry(judged),
          entry(plain, { choice: [], kind: 'note' }),
          entry(objection, { choice: [], kind: 'object' }),
          entry(plain, { via: 'recommended', choice: ['B'] }),
          entry(plain, { note: 'bad \u0007 bell' }),
          entry(plain, { note: 'mail me at someone@example.com' }),
          entry(action, { choice: [], kind: 'done' }),
          entry(plain, { via: 'recommended' }),
          entry(plain),
        ],
      },
      check(),
    ])
  );
}

{
  const r1 = cardDoc('vm.lead', 'R1');
  const r2 = cardDoc('vm.lead', 'R2');
  const r3 = cardDoc('vm.lead', 'R3');
  const r4 = cardDoc('vm.lead', 'R4', { kind: 'objection', options: [], recommendation: null });
  const draft = 'a'.repeat(32);
  const draftNote = entry(r4, { choice: [], kind: 'object', note: 'Still wrong.', entryId: draft });
  scenarios.push(
    await runScenario('replay-and-drafts', [
      cardFile(r1),
      cardFile(r2),
      cardFile(r3),
      cardFile(r4),
      { op: 'submit', device: 'Mac', clientSendId: 'client-send-0001', entries: [entry(r1), entry(r2)] },
      { op: 'submit', device: 'Mac', clientSendId: 'client-send-0001', entries: [entry(r1), entry(r2)] },
      { op: 'submit', device: 'Mac', clientSendId: 'client-send-0001', entries: [entry(r3)] },
      { op: 'submit', device: 'Mac', entries: [draftNote] },
      { op: 'clock', now: NOW + 60_000 },
      { op: 'submit', device: 'Mac', entries: [draftNote] },
      {
        op: 'submit',
        device: 'Mac',
        entries: [{ ...draftNote, note: 'Other words.' }],
      },
      check(),
      { op: 'reopen' },
      { op: 'submit', device: 'Mac', clientSendId: 'client-send-0001', entries: [entry(r1), entry(r2)] },
      { op: 'submit', device: 'Mac', entries: [draftNote] },
      check(),
    ])
  );
}

{
  const q = cardDoc('vm.lead', 'RG1');
  const p = cardDoc('mac.worker', 'RG2');
  scenarios.push(
    await runScenario('ring-retry', [
      cardFile(q),
      cardFile(p),
      { op: 'ring', fail: true },
      { op: 'submit', device: 'Mac', entries: [entry(q), entry(p)] },
      check(),
      { op: 'clock', now: NOW + 60_000 },
      { op: 'retry' },
      check(),
      { op: 'ring', fail: false },
      { op: 'clock', now: NOW + 120_000 },
      { op: 'retry' },
      check(),
      { op: 'retry' },
      check(),
    ])
  );
}

{
  const q = cardDoc('vm.lead', 'B1');
  const n = cardDoc('vm.lead', 'B2');
  scenarios.push(
    await runScenario('share-blocked', [
      cardFile(q),
      cardFile(n),
      { op: 'write', path: 'v1/answers', text: 'not a folder' },
      {
        op: 'submit',
        device: 'Mac',
        entries: [entry(q), entry(n, { choice: [], kind: 'note', note: 'Words.' })],
      },
      check(),
      { op: 'remove', path: 'v1/answers' },
      { op: 'clock', now: NOW + 60_000 },
      { op: 'retry' },
      check(),
    ])
  );
}

{
  const v1 = cardDoc('vm.lead', 'V1');
  const v2 = cardDoc('vm.lead', 'V1', { seq: 2, title: 'Question V1, again' });
  scenarios.push(
    await runScenario('superseded', [
      cardFile(v1),
      { op: 'write', path: 'v1/answers', text: 'not a folder' },
      { op: 'submit', device: 'Mac', entries: [entry(v1)] },
      cardFile(v2),
      { op: 'clock', now: NOW + 1_000 },
      { op: 'submit', device: 'Mac', entries: [entry(v2, { choice: ['B'] })] },
      check(),
      { op: 'remove', path: 'v1/answers' },
      { op: 'clock', now: NOW + 60_000 },
      { op: 'retry' },
      check(),
    ])
  );
}

{
  const f1 = cardDoc('vm.lead', 'F1');
  const f2 = cardDoc('vm.lead', 'F2');
  const f3 = cardDoc('vm.lead', 'F3');
  const f4 = cardDoc('vm.lead', 'F4');
  scenarios.push(
    await runScenario('journal-faults', [
      cardFile(f1),
      cardFile(f2),
      cardFile(f3),
      cardFile(f4),
      // The send line is refused: the send is rebuilt from its answer lines.
      { op: 'faults', faults: { writeOnce: { type: 'send' } } },
      { op: 'submit', device: 'Mac', clientSendId: 'client-send-0002', entries: [entry(f1)] },
      check(),
      // Every doorbell `done` line is refused: a pending completion, landed by the next pass.
      { op: 'faults', faults: { writeAlways: { type: 'delivery', state: 'done' } } },
      { op: 'submit', device: 'Mac', entries: [entry(f2)] },
      check(),
      { op: 'faults', faults: { writeAlways: null } },
      { op: 'clock', now: NOW + 60_000 },
      { op: 'retry' },
      check(),
      // The answer line lands and its fsync fails: a void line cancels it.
      { op: 'faults', faults: { fsyncOnce: { type: 'answer' } } },
      { op: 'submit', device: 'Mac', entries: [entry(f3)] },
      check(),
      // The void is refused too: unclear, held as voided, written again later.
      { op: 'faults', faults: { fsyncOnce: { type: 'answer' }, voidAlways: true } },
      { op: 'submit', device: 'Mac', entries: [entry(f4)] },
      check(),
      { op: 'faults', faults: { voidAlways: false } },
      { op: 'retry' },
      check(),
      { op: 'reopen' },
      { op: 'retry' },
      check(),
    ])
  );
}

{
  // A fresh store instance continues another's journal: pending deliveries finish on its first pass.
  const h1 = cardDoc('vm.lead', 'H1');
  const h2 = cardDoc('mac.worker', 'H2');
  scenarios.push(
    await runScenario('handover', [
      cardFile(h1),
      cardFile(h2),
      { op: 'write', path: 'v1/answers', text: 'not a folder' },
      { op: 'submit', device: 'Windows', clientSendId: 'client-send-0003', entries: [entry(h1), entry(h2)] },
      { op: 'reopen' },
      { op: 'remove', path: 'v1/answers' },
      { op: 'clock', now: NOW + 60_000 },
      { op: 'retry' },
      check(),
      { op: 'submit', device: 'Windows', clientSendId: 'client-send-0003', entries: [entry(h1), entry(h2)] },
      check(),
    ])
  );
}

const vectors = { scenarios };
fs.rmSync(scratch, { recursive: true, force: true });
const file = path.join(import.meta.dir, 'answer-journal-vectors.json');
if (process.argv.includes('--write')) {
  fs.writeFileSync(file, JSON.stringify(vectors, null, 1) + '\n');
} else {
  // canonicalJson: object key order is not part of the comparison here; the Rust test compares bytes.
  assert.equal(
    canonicalJson(JSON.parse(fs.readFileSync(file, 'utf8'))),
    canonicalJson(vectors),
    'answer-journal-vectors.json is stale; regenerate and review'
  );
}
console.log(`answer journal vectors: ${scenarios.length} scenarios`);
