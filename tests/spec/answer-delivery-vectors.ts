/**
 * G16: what the TS answer delivery (src/web-server/status/answers.ts + answer-delivery.ts) produces,
 * for the Rust delivery engine (crates/aac-status/src/answer_delivery.rs) to reproduce. Each scenario
 * runs the real StatusAnswers over a fixture reports root and a fixture key, with a recording fake
 * doorbell (never pm-mail), and records: the journal entries and sends as delivered, every file under
 * `<reports>/v1/keys` and `<reports>/v1/answers` (exact bytes), the rings, and the private key so the
 * Rust replay signs with the same key.
 *
 *   bun tests/spec/answer-delivery-vectors.ts          # check tests/spec/answer-delivery-vectors.json
 *   bun tests/spec/answer-delivery-vectors.ts --write  # regenerate it (review the diff)
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

process.env.AAC_STATUS_NATIVE = 'off';
const { AgentReportsReader } = await import('../../src/web-server/status/agent-reports-reader');
const { StatusAnswers } = await import('../../src/web-server/status/answers');
const delivery = await import('../../src/web-server/status/answer-delivery');
const { shareFileName } = await import('../../src/web-server/status/answer-entries');
const helpers = await import('../unit/web-server/status/status-test-helpers');
const { cardDoc, NOW, writeCard } = helpers;

const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-answer-delivery-'));
let made = 0;
const fresh = (name: string) => {
  const dir = path.join(scratch, `${name}-${(made += 1)}`);
  fs.mkdirSync(dir, { recursive: true });
  return dir;
};

type Ring = { to: string; subject: string; body: string };
interface Run {
  root: string;
  store: string;
  answers: InstanceType<typeof StatusAnswers>;
  rings: Ring[];
  fail: { ring: boolean };
}

function setup(name: string, doorbell: 'record' | 'none' = 'record'): Run {
  const root = fresh(`${name}-reports`);
  const store = path.join(fresh(`${name}-store`), 'status');
  const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
  const rings: Ring[] = [];
  const fail = { ring: false };
  const answers = new StatusAnswers({
    readCard: (id) => reader.readCardNow(id),
    storeDir: () => store,
    reportsRoot: () => root,
    ...(doorbell === 'record'
      ? {
          doorbell: async (to: string, subject: string, body: string) => {
            rings.push({ to, subject, body });
            return fail.ring
              ? { ok: false, detail: 'pm-mail failed.' }
              : { ok: true, detail: `Rang ${to} through pm-mail.` };
          },
        }
      : {}),
    now: () => NOW,
  });
  return { root, store, answers, rings, fail };
}

async function settle(): Promise<void> {
  for (let i = 0; i < 8; i += 1) await new Promise((resolve) => setTimeout(resolve, 5));
}

function files(root: string): Record<string, string> {
  const out: Record<string, string> = {};
  const walk = (dir: string, rel: string) => {
    if (!fs.existsSync(dir)) return;
    // Fixed order (aac-answers.pub first): a random key id must not reorder the record.
    const names = fs
      .readdirSync(dir)
      .sort((x, y) =>
        x === 'aac-answers.pub' ? -1 : y === 'aac-answers.pub' ? 1 : x < y ? -1 : x > y ? 1 : 0
      );
    for (const name of names) {
      const full = path.join(dir, name);
      const relName = rel ? `${rel}/${name}` : name;
      if (fs.statSync(full).isDirectory()) walk(full, relName);
      else out[relName] = fs.readFileSync(full, 'utf8');
    }
  };
  walk(path.join(root, 'v1', 'keys'), 'keys');
  walk(path.join(root, 'v1', 'answers'), 'answers');
  return out;
}

/** A snapshot: later delivery mutates the live entries and the ring list, never this copy. */
function record(name: string, run: Run, extra: Record<string, unknown> = {}) {
  return JSON.parse(JSON.stringify(recordLive(name, run, extra)));
}

function recordLive(name: string, run: Run, extra: Record<string, unknown>) {
  return {
    name,
    key: fs.readFileSync(path.join(run.store, 'answer-key.pem'), 'utf8'),
    entries: run.answers.list().map((entry) => ({
      id: entry.id,
      sendId: entry.sendId,
      answer: entry.answer,
      card: entry.card,
      share: entry.share,
      doorbell: entry.doorbell,
      shareFile: shareFileName(entry.answer, entry.id),
    })),
    sends: run.answers.sendList(),
    files: files(run.root),
    rings: run.rings,
    ...extra,
  };
}

const entry = (cardId: string, sha: string, fields: Record<string, unknown> = {}) => ({
  cardId,
  cardSeq: 1,
  cardSha256: sha,
  choice: ['A'],
  via: 'deck',
  ...fields,
});

const scenarios: unknown[] = [];

{
  const run = setup('single');
  const sha = writeCard(run.root, cardDoc('vm.lead', 'Q1', { seq: 3 }));
  await run.answers.submitSet(
    [
      entry('vm.lead--Q1', sha, {
        cardSeq: 3,
        note: '  Keep it inside — but test at 320 px.\n  Thanks  ',
      }),
    ],
    'iPhone'
  );
  await settle();
  scenarios.push(record('single', run));
}

{
  const run = setup('batch');
  const a = writeCard(run.root, cardDoc('vm.lead', 'Q2', { batch: 'deck-7' }));
  const b = writeCard(run.root, cardDoc('vm.lead', 'Q3'));
  const c = writeCard(
    run.root,
    cardDoc('mac.worker', 'Q4', {
      batch: 'deck-7b',
      kind: 'objection',
      options: [],
      recommendation: null,
    })
  );
  const d = writeCard(
    run.root,
    cardDoc('mac.silent', 'Q5', { replyTo: { pmMail: null, t3Thread: null } })
  );
  await run.answers.submitSet(
    [
      entry('vm.lead--Q2', a, { choice: ['B'] }),
      entry('vm.lead--Q3', b, { choice: [], kind: 'note', note: 'Use the other approach.' }),
      entry('mac.worker--Q4', c, { choice: [], kind: 'object', note: 'Not yet: blocked\r\nby X.' }),
      entry('mac.silent--Q5', d),
    ],
    'Mac'
  );
  await settle();
  scenarios.push(record('batch', run));
}

{
  const run = setup('ring-retry');
  run.fail.ring = true;
  const sha = writeCard(run.root, cardDoc('vm.lead', 'Q6'));
  await run.answers.submitSet([entry('vm.lead--Q6', sha)], 'Mac');
  await settle();
  const failed = record('ring-retry.failed', run);
  run.fail.ring = false;
  await run.answers.retryPending();
  await settle();
  scenarios.push(failed, record('ring-retry.retried', run));
}

{
  const before = process.env.AAC_STATUS_DOORBELL;
  process.env.AAC_STATUS_DOORBELL = 'off';
  const run = setup('doorbell-off', 'none');
  const sha = writeCard(run.root, cardDoc('vm.lead', 'Q7'));
  await run.answers.submitSet([entry('vm.lead--Q7', sha)], 'Mac');
  await settle();
  if (before === undefined) delete process.env.AAC_STATUS_DOORBELL;
  else process.env.AAC_STATUS_DOORBELL = before;
  scenarios.push(record('doorbell-off', run));
}

{
  // A newer answer of ours is already on the share (another holder wrote it): the older one is superseded.
  // Placed before the send, signed with the store's own key (created first), so the guard runs.
  const signing = await import('../../src/web-server/status/answer-signing');
  const run = setup('newer-on-share');
  const key = signing.loadOrCreateAnswerKey(run.store);
  const old = writeCard(run.root, cardDoc('vm.lead', 'Q8', { seq: 1 }));
  const newer = signing.signAnswer(
    {
      schema: 'agent-reports/answer@1',
      cardId: 'vm.lead--Q8',
      cardSeq: 5,
      cardSha256: 'b'.repeat(64),
      kind: 'answer',
      choice: ['B'],
      note: '',
      via: 'deck',
      answeredAt: new Date(NOW - 1000).toISOString(),
      answeredBy: 'owner',
      device: 'Mac',
    },
    key
  );
  const newerText = `${JSON.stringify(newer, null, 2)}\n`;
  const target = path.join(run.root, 'v1', 'answers', 'vm.lead--Q8.json');
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, newerText);
  await run.answers.submitSet([entry('vm.lead--Q8', old)], 'Mac');
  await settle();
  scenarios.push(record('newer-on-share', run, { newerOnShare: newerText }));
}

const vectors = {
  doorbellSender: delivery.DOORBELL_SENDER,
  noMailDetail: delivery.NO_MAIL_DETAIL,
  doorbellOffDetail: delivery.DOORBELL_OFF_DETAIL,
  scenarios,
};
fs.rmSync(scratch, { recursive: true, force: true });
const file = path.join(import.meta.dir, 'answer-delivery-vectors.json');
/**
 * Keys, record ids, send ids and signatures are random per run: the check compares the shape with
 * each such value numbered by first appearance. The Rust replay uses the committed concrete values.
 */
function normalized(value: unknown): string {
  const seen = new Map<string, string>();
  const random =
    /-----BEGIN [A-Z ]+-----[^-]*-----END [A-Z ]+-----(?:\n)?|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{64}|(?<=(?:note|object)-)[0-9a-f]{12}|[A-Za-z0-9+/]{80,}={0,2}|[0-9a-f]{16}/g;
  return JSON.stringify(value).replace(random, (match) => {
    if (!seen.has(match)) seen.set(match, `<v${seen.size}>`);
    return seen.get(match) as string;
  });
}
if (process.argv.includes('--write'))
  fs.writeFileSync(file, JSON.stringify(vectors, null, 1) + '\n');
else
  assert.equal(
    normalized(JSON.parse(fs.readFileSync(file, 'utf8'))),
    normalized(vectors),
    'answer-delivery-vectors.json is stale; regenerate and review'
  );
console.log(`answer delivery vectors: ${scenarios.length} scenarios`);
