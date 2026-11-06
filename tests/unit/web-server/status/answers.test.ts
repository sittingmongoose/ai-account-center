import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import {
  canonicalJson,
  loadOrCreateAnswerKey,
  signAnswer,
  verifySignedAnswer,
  type UnsignedAnswer,
} from '../../../../src/web-server/status/answer-signing';
import {
  ANSWER_NOTE_MAX,
  MAX_DELIVERY_ATTEMPTS,
  StatusAnswers,
  answerDevice,
  deliveryView,
  normalizeNote,
  type Doorbell,
} from '../../../../src/web-server/status/answers';
import { cardDoc, NOW, tempDirs, writeCard } from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

function setup(options: { doorbell?: Doorbell } = {}) {
  const root = dirs.make('ccs-status-reports-');
  const store = path.join(dirs.make('ccs-status-store-'), 'status');
  const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
  const rings: Array<{ to: string; subject: string; body: string }> = [];
  const doorbell: Doorbell =
    options.doorbell ??
    (async (to, subject, body) => {
      rings.push({ to, subject, body });
      return { ok: true, detail: `Rang ${to} through pm-mail.` };
    });
  const answers = new StatusAnswers({
    readCard: (id) => reader.readCardNow(id),
    storeDir: () => store,
    reportsRoot: () => root,
    doorbell,
    now: () => NOW,
  });
  return { root, store, answers, rings };
}

async function settle(): Promise<void> {
  for (let i = 0; i < 5; i += 1) await new Promise((resolve) => setTimeout(resolve, 5));
}

describe('answer signing', () => {
  const unsigned: UnsignedAnswer = {
    schema: 'agent-reports/answer@1',
    cardId: 'vm.lead--Q1',
    cardSeq: 2,
    cardSha256: 'a'.repeat(64),
    kind: 'answer',
    choice: ['A'],
    note: 'Ünïcode note, "quoted"\nsecond line',
    via: 'deck',
    answeredAt: '2026-11-06T17:02:11.000Z',
    answeredBy: 'owner',
    device: 'iPhone',
  };

  it('writes canonical JSON with sorted keys at every depth', () => {
    expect(canonicalJson({ b: 1, a: { d: [3, { z: 1, y: 2 }], c: null } })).toBe(
      '{"a":{"c":null,"d":[3,{"y":2,"z":1}]},"b":1}'
    );
  });

  it('orders non-ASCII keys by UTF-16 code unit, as RFC 8785 does', () => {
    // U+FF21 (fullwidth A) sorts after the surrogate pair of U+1F600 by code unit (0xD83D < 0xFF21),
    // though its code point is lower; "é" (0xE9) sorts after "z" and before both.
    const value = { '\uff21': 1, '\u{1f600}': 2, é: 3, z: 4, A: 5, '': 6 };
    expect(canonicalJson(value)).toBe('{"":6,"A":5,"z":4,"é":3,"\u{1f600}":2,"\uff21":1}');
    expect(canonicalJson({ n: [1.5, -0, 1e21, 'x\u2028'], t: true })).toBe(
      '{"n":[1.5,0,1e+21,"x\u2028"],"t":true}'
    );
  });

  it('round-trips a signature and detects tampering, other keys and unsigned files', () => {
    const store = dirs.make();
    const key = loadOrCreateAnswerKey(store);
    expect(fs.statSync(path.join(store, 'answer-key.pem')).mode & 0o777).toBe(0o600);
    expect(key.keyId).toMatch(/^[0-9a-f]{16}$/);
    expect(loadOrCreateAnswerKey(store).keyId).toBe(key.keyId);
    const signed = signAnswer(unsigned, key);
    const text = JSON.stringify(signed, null, 2);
    expect(verifySignedAnswer(text, key.publicKeyPem)).toEqual({ ok: true, answer: signed });
    const tampered = JSON.stringify({ ...signed, choice: ['B'] });
    expect(verifySignedAnswer(tampered, key.publicKeyPem)).toEqual({
      ok: false,
      reason: 'bad signature',
    });
    const other = loadOrCreateAnswerKey(dirs.make());
    expect(verifySignedAnswer(text, other.publicKeyPem)).toEqual({
      ok: false,
      reason: 'signed by another key',
    });
    const { signature: _drop, ...bare } = signed;
    expect(verifySignedAnswer(JSON.stringify(bare), key.publicKeyPem)).toEqual({
      ok: false,
      reason: 'unsigned',
    });
    expect(verifySignedAnswer('nope', key.publicKeyPem).ok).toBe(false);
    const short = JSON.stringify({ ...signed, signature: { ...signed.signature, value: 'AAAA' } });
    expect(verifySignedAnswer(short, key.publicKeyPem)).toEqual({
      ok: false,
      reason: 'bad signature',
    });
    // Non-ASCII keys and values sign and verify.
    const unicode = signAnswer({ ...unsigned, note: 'ü ✓ 😀', device: 'iPhone' }, key);
    expect(verifySignedAnswer(JSON.stringify(unicode), key.publicKeyPem).ok).toBe(true);
  });
});

type Entry = Record<string, unknown>;
const entry = (cardId: string, sha: string, fields: Entry = {}): Entry => ({
  cardId,
  cardSeq: 1,
  cardSha256: sha,
  choice: ['A'],
  via: 'deck',
  ...fields,
});

describe('answers', () => {
  it('stores, signs, shares and rings, in that order, with the note verbatim', async () => {
    const { root, store, answers, rings } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 3 }));
    const note = '  Keep it inside — but test at 320 px.\n  Thanks  ';
    const results = await answers.submitSet(
      [entry('vm.lead--Q1', sha, { cardSeq: 3, note })],
      'iPhone'
    );
    expect(results).toEqual([
      { cardId: 'vm.lead--Q1', outcome: 'recorded', code: null, error: null },
    ]);
    const recorded = answers.history(1)[0];
    expect(recorded.answer.note).toBe('Keep it inside — but test at 320 px.\n  Thanks');
    expect(recorded.answer.kind).toBe('answer');
    expect(recorded.answer.answeredBy).toBe('owner');
    const line = fs.readFileSync(path.join(store, 'answers.jsonl'), 'utf8').split('\n')[0];
    expect(JSON.parse(line).answer.cardSha256).toBe(sha);
    const shared = fs.readFileSync(path.join(root, 'v1', 'answers', 'vm.lead--Q1.json'), 'utf8');
    const pub = fs.readFileSync(path.join(root, 'v1', 'keys', 'aac-answers.pub'), 'utf8');
    expect(verifySignedAnswer(shared, pub).ok).toBe(true);
    expect(fs.readdirSync(path.join(root, 'v1', 'answers')).sort()).toEqual([
      'batches',
      'vm.lead--Q1.json',
    ]);
    await settle();
    expect(rings).toHaveLength(1);
    expect(rings[0].to).toBe('lead');
    expect(rings[0].subject).toBe('1 answer from the owner');
    // The doorbell carries the card id, the exact choice and words, the UTC time and the card hash.
    expect(rings[0].body).toContain('Card vm.lead--Q1 (seq 3)');
    expect(rings[0].body).toContain('Choice: A (First)');
    expect(rings[0].body).toContain('Words: Keep it inside — but test at 320 px.\n      Thanks');
    expect(rings[0].body).toContain(`Time (UTC): ${recorded.answer.answeredAt}`);
    expect(rings[0].body).toContain(`Card sha256: ${sha}`);
    expect(rings[0].body).toContain('The signed files are the authority');
    expect(rings[0].body).toContain(`Signed file: agent-reports/v1/answers/vm.lead--Q1.json`);
    const keyId = recorded.answer.signature.keyId;
    expect(fs.readFileSync(path.join(root, 'v1', 'keys', `${keyId}.pub`), 'utf8')).toBe(pub);
    expect(deliveryView(answers.history(1)[0]).state).toBe('rang');
    const again = new StatusAnswers({
      readCard: async () => null,
      storeDir: () => store,
      reportsRoot: () => root,
    });
    expect(again.history()[0].doorbell.state).toBe('done');
    expect(again.isAnswered('vm.lead--Q1', 3, sha)).toBe(true);
  });

  it('records each entry of a set on its own and refuses every way an entry can be wrong', async () => {
    const { root, answers } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 2 }));
    const q2 = writeCard(root, cardDoc('vm.lead', 'Q2', { state: 'withdrawn' }));
    const q3 = writeCard(root, cardDoc('vm.lead', 'Q3', { multiple: true }));
    const q4 = writeCard(root, cardDoc('vm.lead', 'Q4', { recommendation: null }));
    const base = entry('vm.lead--Q1', sha, { cardSeq: 2 });
    const results = await answers.submitSet(
      [
        { ...base, cardSeq: 1 },
        { ...base, cardSha256: 'b'.repeat(64) },
        { ...base, cardId: 'vm.lead--Q9' },
        { ...base, choice: ['C'] },
        { ...base, choice: ['A', 'B'] },
        { ...base, choice: ['A', 'A'] },
        { ...base, choice: [] },
        { ...base, note: 'x'.repeat(ANSWER_NOTE_MAX + 1) },
        { ...base, note: 'bad \u0000 byte' },
        { ...base, via: 'card' },
        { ...base, extra: 1 },
        'not an entry',
        { ...base, choice: ['B'], via: 'recommended' },
        entry('vm.lead--Q2', q2),
        entry('vm.lead--Q3', q3, { choice: ['B', 'A'] }),
        entry('vm.lead--Q4', q4, { via: 'recommended' }),
        base,
        base,
      ],
      'Mac'
    );
    expect(results.map((r) => r.code ?? r.outcome)).toEqual([
      'card_changed',
      'card_changed',
      'card_changed',
      'invalid_choice',
      'invalid_choice',
      'invalid_choice',
      'empty_entry',
      'invalid_note',
      'invalid_note',
      'invalid_entry',
      'invalid_entry',
      'invalid_entry',
      'invalid_via',
      'card_changed',
      'recorded',
      'invalid_via',
      'recorded',
      'card_changed',
    ]);
    expect(results[0]).toEqual({
      cardId: 'vm.lead--Q1',
      outcome: 'card_changed',
      code: 'card_changed',
      error: 'This card changed or is no longer open. Reload it and answer again.',
    });
    expect(results[3].outcome).toBe('refused');
    expect(results[11].cardId).toBeNull();
  });

  it('records a note alone as a note: signed and rung, but the card stays open', async () => {
    const { root, answers, rings } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const results = await answers.submitSet(
      [
        entry('vm.lead--Q1', sha, { choice: [], note: 'What about a third way?' }),
        entry('vm.lead--Q1', sha, { choice: [], note: 'And a second note.' }),
      ],
      'iPhone'
    );
    expect(results.map((r) => r.outcome)).toEqual(['recorded', 'recorded']);
    expect(answers.isAnswered('vm.lead--Q1', 1, sha)).toBe(false);
    const notes = answers.history();
    expect(notes.map((n) => n.answer.kind)).toEqual(['note', 'note']);
    const files = fs
      .readdirSync(path.join(root, 'v1', 'answers'))
      .filter((f) => f !== 'batches')
      .sort();
    expect(files).toHaveLength(2);
    expect(files.every((f) => /^vm\.lead--Q1\.note-[0-9a-f]{12}\.json$/.test(f))).toBe(true);
    const pub = fs.readFileSync(path.join(root, 'v1', 'keys', 'aac-answers.pub'), 'utf8');
    for (const file of files)
      expect(
        verifySignedAnswer(fs.readFileSync(path.join(root, 'v1', 'answers', file)), pub).ok
      ).toBe(true);
    await settle();
    // One send, one asker: one ring naming both notes.
    expect(rings.map((r) => r.subject)).toEqual(['2 answers from the owner']);
    expect(rings[0].body.match(/Card vm\.lead--Q1 /g)).toHaveLength(2);
    // The card can still be answered.
    const answered = await answers.submitSet([entry('vm.lead--Q1', sha)], 'iPhone');
    expect(answered[0].outcome).toBe('recorded');
    expect(answers.isAnswered('vm.lead--Q1', 1, sha)).toBe(true);
  });

  it("takes objections with the owner's words and done on action cards, and checks kind against the card", async () => {
    const { root, answers, rings } = setup();
    const none = { options: [], recommendation: null };
    const obj = writeCard(root, cardDoc('vm.lead', 'O1', { kind: 'objection', ...none }));
    const act = writeCard(root, cardDoc('vm.lead', 'D1', { kind: 'action', ...none }));
    const q = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const results = await answers.submitSet(
      [
        entry('vm.lead--O1', obj, { choice: [], kind: 'object' }),
        entry('vm.lead--O1', obj, { choice: ['A'], kind: 'object', note: 'No' }),
        entry('vm.lead--D1', act, { choice: [], kind: 'object', note: 'No' }),
        entry('vm.lead--Q1', q, { choice: [], kind: 'done' }),
        entry('vm.lead--D1', act, { choice: ['A'] }),
        entry('vm.lead--O1', obj, { choice: [], kind: 'object', note: 'Keep the old layout.' }),
        entry('vm.lead--D1', act, { choice: [], kind: 'done' }),
        entry('vm.lead--O1', obj, { choice: [], kind: 'object', note: 'Again' }),
        entry('vm.lead--Q1', q, { choice: [], kind: 'answer' }),
        entry('vm.lead--Q1', q, { choice: [], kind: 'shout' }),
      ],
      'iPhone'
    );
    expect(results.map((r) => r.code ?? r.outcome)).toEqual([
      'words_required',
      'invalid_choice',
      'invalid_kind',
      'invalid_kind',
      'invalid_choice',
      'recorded',
      'recorded',
      'recorded',
      'invalid_kind',
      'invalid_entry',
    ]);
    // An objection leaves the card open for the agent's reply; done closes the action card.
    expect(answers.isAnswered('vm.lead--O1', 1, obj)).toBe(false);
    expect(answers.isAnswered('vm.lead--D1', 1, act)).toBe(true);
    expect(answers.history().map((e) => e.answer.kind)).toEqual(['object', 'done', 'object']);
    await settle();
    expect(rings.map((r) => r.subject)).toEqual(['3 answers from the owner']);
    expect(rings[0].body).toContain('Words: Keep the old layout.');
    expect(rings[0].body).toContain('Choice: none');
  });

  it('marks unchanged recommendations as recommended', async () => {
    const { root, answers } = setup();
    const sha = writeCard(
      root,
      cardDoc('vm.lead', 'Q1', { recommendation: { key: 'B', why: null } })
    );
    const results = await answers.submitSet(
      [entry('vm.lead--Q1', sha, { choice: ['B'], via: 'recommended' })],
      'iPad'
    );
    expect(results[0].outcome).toBe('recorded');
    expect(answers.history(1)[0].answer.via).toBe('recommended');
  });

  it('serializes two sets answering the same card version: one wins, one is told it changed', async () => {
    const { root, answers } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const [a, b] = await Promise.all([
      answers.submitSet([entry('vm.lead--Q1', sha)], 'Mac'),
      answers.submitSet([entry('vm.lead--Q1', sha)], 'iPhone'),
    ]);
    expect([a[0].outcome, b[0].outcome].sort()).toEqual(['card_changed', 'recorded']);
  });

  it('records a failed doorbell, shows it, and retries until it rings', async () => {
    let ok = false;
    const { root, answers } = setup({
      doorbell: async () =>
        ok ? { ok: true, detail: 'Rang.' } : { ok: false, detail: 'pm-mail failed.' },
    });
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    await answers.submitSet([entry('vm.lead--Q1', sha)], 'Mac');
    await settle();
    let view = deliveryView(answers.history(1)[0]);
    expect(view.state).toBe('failed');
    expect(view.detail).toContain('retrying');
    ok = true;
    await answers.retryPending();
    view = deliveryView(answers.history(1)[0]);
    expect(view.state).toBe('rang');
    expect(answers.history(1)[0].doorbell.attempts).toBe(2);
  });

  it('records a failed share write, never rings before the signed copy exists, and retries', async () => {
    const { root, answers, rings } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    fs.writeFileSync(path.join(root, 'v1', 'answers'), 'blocker');
    const results = await answers.submitSet([entry('vm.lead--Q1', sha)], 'Mac');
    expect(results[0].outcome).toBe('recorded');
    await settle();
    expect(rings).toHaveLength(0);
    expect(deliveryView(answers.history(1)[0]).state).toBe('failed');
    fs.rmSync(path.join(root, 'v1', 'answers'));
    await answers.retryPending();
    expect(answers.history(1)[0].share.state).toBe('done');
    expect(rings).toHaveLength(1);
  });

  it('says when nobody can be rung, and stops retrying after the limit', async () => {
    const { root, answers } = setup({ doorbell: async () => ({ ok: false, detail: 'x' }) });
    const sha = writeCard(
      root,
      cardDoc('vm.lead', 'Q1', { replyTo: { pmMail: null, t3Thread: null } })
    );
    await answers.submitSet([entry('vm.lead--Q1', sha)], 'Mac');
    await settle();
    // No mail name: recorded normally, nobody rung, the agent reads its answers file.
    const view = deliveryView(answers.history(1)[0]);
    expect(view.state).toBe('rang');
    expect(view.detail).toBe('No mail name; the agent reads its answers file.');
    const sha2 = writeCard(root, cardDoc('vm.lead', 'Q2'));
    await answers.submitSet([entry('vm.lead--Q2', sha2)], 'Mac');
    await settle();
    for (let i = 0; i < MAX_DELIVERY_ATTEMPTS + 3; i += 1) await answers.retryPending();
    expect(answers.history(1)[0].doorbell.attempts).toBe(MAX_DELIVERY_ATTEMPTS);
    expect(deliveryView(answers.history(1)[0]).detail).toContain(
      `after ${MAX_DELIVERY_ATTEMPTS} attempts`
    );
  });

  it('turns the doorbell off for test servers', async () => {
    const { root, store } = setup();
    const reader = new AgentReportsReader({ root: () => root });
    const answers = new StatusAnswers({
      readCard: (id) => reader.readCardNow(id),
      storeDir: () => store,
      reportsRoot: () => root,
    });
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const before = process.env.AAC_STATUS_DOORBELL;
    process.env.AAC_STATUS_DOORBELL = 'off';
    try {
      await answers.submitSet([entry('vm.lead--Q1', sha)], 'Mac');
      await settle();
    } finally {
      if (before === undefined) delete process.env.AAC_STATUS_DOORBELL;
      else process.env.AAC_STATUS_DOORBELL = before;
    }
    expect(answers.history(1)[0].doorbell.state).toBe('skipped');
  });

  it('normalizes notes and names devices', () => {
    expect(normalizeNote('  hi  ')).toBe('hi');
    expect(normalizeNote(undefined)).toBe('');
    expect(normalizeNote(5)).toBeNull();
    expect(normalizeNote('\ud800')).toBeNull();
    expect(answerDevice('Mozilla/5.0 (iPhone; CPU iPhone OS 27_1 like Mac OS X)')).toBe('iPhone');
    expect(answerDevice('Mozilla/5.0 (Windows NT 10.0; Win64; x64)')).toBe('Windows');
    expect(answerDevice(undefined)).toBe('browser');
  });
});
