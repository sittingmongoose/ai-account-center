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
    choice: ['A'],
    note: 'Ünïcode note, "quoted"\nsecond line',
    via: 'card',
    answeredAt: '2026-11-06T17:02:11.000Z',
    answeredBy: 'owner',
    device: 'iPhone',
  };

  it('writes canonical JSON with sorted keys at every depth', () => {
    expect(canonicalJson({ b: 1, a: { d: [3, { z: 1, y: 2 }], c: null } })).toBe(
      '{"a":{"c":null,"d":[3,{"y":2,"z":1}]},"b":1}'
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
  });
});

describe('answers', () => {
  it('stores, signs, shares and rings, in that order, with the note verbatim', async () => {
    const { root, store, answers, rings } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 3 }));
    const note = '  Keep it inside — but test at 320 px.\n  Thanks  ';
    const outcome = await answers.submit(
      { cardId: 'vm.lead--Q1', cardSeq: 3, cardSha256: sha, choice: ['A'], note },
      'iPhone'
    );
    expect(outcome.ok).toBe(true);
    if (!outcome.ok) return;
    expect(outcome.entry.answer.note).toBe('Keep it inside — but test at 320 px.\n  Thanks');
    expect(outcome.entry.answer.answeredBy).toBe('owner');
    const line = fs.readFileSync(path.join(store, 'answers.jsonl'), 'utf8').split('\n')[0];
    expect(JSON.parse(line).answer.cardSha256).toBe(sha);
    const shared = fs.readFileSync(path.join(root, 'v1', 'answers', 'vm.lead--Q1.json'), 'utf8');
    const pub = fs.readFileSync(path.join(root, 'v1', 'keys', 'aac-answers.pub'), 'utf8');
    expect(verifySignedAnswer(shared, pub).ok).toBe(true);
    expect(fs.readdirSync(path.join(root, 'v1', 'answers'))).toEqual(['vm.lead--Q1.json']);
    await settle();
    expect(rings).toHaveLength(1);
    expect(rings[0].to).toBe('lead');
    expect(rings[0].body).toContain('vm.lead--Q1');
    expect(rings[0].body).not.toContain('320 px');
    expect(deliveryView(answers.history(1)[0]).state).toBe('rang');
    // A second store reads the same history back.
    const again = new StatusAnswers({
      readCard: async () => null,
      storeDir: () => store,
      reportsRoot: () => root,
    });
    expect(again.history()[0].doorbell.state).toBe('done');
    expect(again.isAnswered('vm.lead--Q1', 3, sha)).toBe(true);
  });

  it('refuses every way an answer can be wrong', async () => {
    const { root, answers } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 2 }));
    const base = { cardId: 'vm.lead--Q1', cardSeq: 2, cardSha256: sha, choice: ['A'] };
    const code = async (input: Partial<typeof base> & { note?: string }) => {
      const outcome = await answers.submit({ ...base, ...input }, 'Mac');
      return outcome.ok ? 'ok' : `${outcome.status} ${outcome.code}`;
    };
    expect(await code({ cardSeq: 1 })).toBe('409 card_changed');
    expect(await code({ cardSha256: 'b'.repeat(64) })).toBe('409 card_changed');
    expect(await code({ cardId: 'vm.lead--Q9' })).toBe('409 card_changed');
    expect(await code({ choice: ['C'] })).toBe('400 invalid_choice');
    expect(await code({ choice: ['A', 'B'] })).toBe('400 invalid_choice');
    expect(await code({ choice: ['A', 'A'] })).toBe('400 invalid_choice');
    expect(await code({ choice: [] })).toBe('400 invalid_choice');
    expect(await code({ note: 'x'.repeat(ANSWER_NOTE_MAX + 1) })).toBe('400 invalid_note');
    expect(await code({ note: 'bad \u0000 byte' })).toBe('400 invalid_note');
    // A note alone is allowed when the card allows a note.
    expect(await code({ choice: [], note: 'Something else entirely' })).toBe('ok');
    // That version is answered now.
    expect(await code({})).toBe('409 card_changed');
    // A withdrawn card and a card that forbids notes.
    const withdrawn = writeCard(root, cardDoc('vm.lead', 'Q2', { state: 'withdrawn' }));
    expect(await code({ cardId: 'vm.lead--Q2', cardSeq: 1, cardSha256: withdrawn })).toBe(
      '409 card_changed'
    );
    const noNote = writeCard(root, cardDoc('vm.lead', 'Q3', { allowNote: false }));
    expect(
      await code({ cardId: 'vm.lead--Q3', cardSeq: 1, cardSha256: noNote, note: 'hi' })
    ).toBe('400 invalid_choice');
    const multi = writeCard(root, cardDoc('vm.lead', 'Q4', { multiple: true }));
    expect(
      await code({ cardId: 'vm.lead--Q4', cardSeq: 1, cardSha256: multi, choice: ['B', 'A'] })
    ).toBe('ok');
  });

  it('serializes two answers to the same card version: one wins, one is told it changed', async () => {
    const { root, answers } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const input = { cardId: 'vm.lead--Q1', cardSeq: 1, cardSha256: sha, choice: ['A'] };
    const [a, b] = await Promise.all([answers.submit(input, 'Mac'), answers.submit(input, 'iPhone')]);
    expect([a.ok, b.ok].sort()).toEqual([false, true]);
  });

  it('approves a batch except the exceptions, refusing a changed card individually', async () => {
    const { root, answers, rings } = setup();
    const batch = 'F-questions';
    const shaQ1 = writeCard(root, cardDoc('vm.lead', 'Q1', { batch }));
    const shaQ2 = writeCard(root, cardDoc('vm.lead', 'Q2', { batch, recommendation: { key: 'B' } }));
    const shaQ3 = writeCard(root, cardDoc('vm.lead', 'Q3', { batch }));
    const shaQ4 = writeCard(root, cardDoc('vm.lead', 'Q4', { batch, recommendation: null }));
    const shaQ5 = writeCard(root, cardDoc('vm.lead', 'Q5', { batch: 'other' }));
    // Q3 changes after the page loaded it.
    writeCard(root, cardDoc('vm.lead', 'Q3', { batch, seq: 2, whatChanged: 'New option' }));
    const result = await answers.submitBatch(
      {
        batch,
        cards: [
          { id: 'vm.lead--Q1', seq: 1, cardSha256: shaQ1 },
          { id: 'vm.lead--Q2', seq: 1, cardSha256: shaQ2 },
          { id: 'vm.lead--Q3', seq: 1, cardSha256: shaQ3 },
          { id: 'vm.lead--Q4', seq: 1, cardSha256: shaQ4 },
          { id: 'vm.lead--Q5', seq: 1, cardSha256: shaQ5 },
        ],
        except: ['vm.lead--Q1'],
      },
      'iPad'
    );
    expect(result.results.map((r) => [r.id, r.outcome])).toEqual([
      ['vm.lead--Q1', 'excepted'],
      ['vm.lead--Q2', 'answered'],
      ['vm.lead--Q3', 'card_changed'],
      ['vm.lead--Q4', 'no_recommendation'],
      ['vm.lead--Q5', 'not_in_batch'],
    ]);
    const answered = result.results[1].entry?.answer;
    expect(answered?.choice).toEqual(['B']);
    expect(answered?.via).toBe('approve-all-except');
    expect(answered?.batchAnswer).toBe(result.batchAnswer);
    expect(answers.isAnswered('vm.lead--Q1', 1, shaQ1)).toBe(false);
    await settle();
    expect(rings.map((r) => r.subject)).toEqual(['Answer for vm.lead--Q2']);
  });

  it('records a failed doorbell, shows it, and retries until it rings', async () => {
    let ok = false;
    const { root, answers } = setup({
      doorbell: async () =>
        ok ? { ok: true, detail: 'Rang.' } : { ok: false, detail: 'pm-mail failed.' },
    });
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    await answers.submit(
      { cardId: 'vm.lead--Q1', cardSeq: 1, cardSha256: sha, choice: ['A'] },
      'Mac'
    );
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
    // A file where the answers folder should be makes the share write fail.
    fs.writeFileSync(path.join(root, 'v1', 'answers'), 'blocker');
    const outcome = await answers.submit(
      { cardId: 'vm.lead--Q1', cardSeq: 1, cardSha256: sha, choice: ['A'] },
      'Mac'
    );
    expect(outcome.ok).toBe(true);
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
    await answers.submit({ cardId: 'vm.lead--Q1', cardSeq: 1, cardSha256: sha, choice: ['A'] }, 'Mac');
    await settle();
    const view = deliveryView(answers.history(1)[0]);
    expect(view.state).toBe('failed');
    expect(view.detail).toContain('no pm-mail recipient');
    const sha2 = writeCard(root, cardDoc('vm.lead', 'Q2'));
    await answers.submit({ cardId: 'vm.lead--Q2', cardSeq: 1, cardSha256: sha2, choice: ['A'] }, 'Mac');
    await settle();
    for (let i = 0; i < MAX_DELIVERY_ATTEMPTS + 3; i += 1) await answers.retryPending();
    expect(answers.history(1)[0].doorbell.attempts).toBe(MAX_DELIVERY_ATTEMPTS);
    expect(deliveryView(answers.history(1)[0]).detail).toContain(`after ${MAX_DELIVERY_ATTEMPTS} attempts`);
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
      await answers.submit({ cardId: 'vm.lead--Q1', cardSeq: 1, cardSha256: sha, choice: ['A'] }, 'Mac');
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
