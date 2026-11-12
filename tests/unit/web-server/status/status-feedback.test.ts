import { computerReportSchema } from '../../../../src/web-server/status/agent-reports-reader';
import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import {
  AgentReportsReader,
  ensureReportsLayout,
  packagedSchemasDir,
  publishSchemas,
} from '../../../../src/web-server/status/agent-reports-reader';
import { SEND_SCHEMA, pmMailDoorbell } from '../../../../src/web-server/status/answer-delivery';
import { verifySignedDocument } from '../../../../src/web-server/status/answer-signing';
import {
  deliveryView,
  StatusAnswers,
  type Doorbell,
} from '../../../../src/web-server/status/answers';
import { parseReportFile } from '../../../../src/web-server/status/report-schema';
import type { CommandRunner } from '../../../../src/web-server/status/status-collectors';
import { buildStatusView, JUDGEMENT_ANCHORED } from '../../../../src/web-server/status/status-view';
import { cardDoc, NOW, tempDirs, threadDoc, writeCard } from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

const json = (value: unknown) => Buffer.from(JSON.stringify(value));

function setup(doorbell?: Doorbell) {
  const root = dirs.make('ccs-status-reports-');
  const store = path.join(dirs.make('ccs-status-store-'), 'status');
  const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
  const rings: Array<{ to: string; subject: string; body: string }> = [];
  const answers = new StatusAnswers({
    readCard: (id) => reader.readCardNow(id),
    storeDir: () => store,
    reportsRoot: () => root,
    now: () => NOW,
    doorbell:
      doorbell ??
      (async (to, subject, body) => {
        rings.push({ to, subject, body });
        return { ok: true, detail: `Rang ${to}.` };
      }),
  });
  return { root, store, reader, answers, rings };
}

async function settle(): Promise<void> {
  for (let i = 0; i < 5; i += 1) await new Promise((resolve) => setTimeout(resolve, 5));
}

const entry = (cardId: string, sha: string, fields: Record<string, unknown> = {}) => ({
  cardId,
  cardSeq: 1,
  cardSha256: sha,
  choice: ['A'],
  via: 'deck',
  ...fields,
});

describe('one ring per asker per send, and a signed send summary', () => {
  it('groups by asker, names the batch and each card, and signs the whole send', async () => {
    const { root, answers, rings } = setup();
    const a1 = writeCard(root, cardDoc('vm.lead', 'Q1', { batch: 'F-questions' }));
    const a2 = writeCard(root, cardDoc('vm.lead', 'Q2', { batch: 'F-questions' }));
    const b1 = writeCard(root, cardDoc('mac.other', 'Q1'));
    const none = writeCard(
      root,
      cardDoc('vm.quiet', 'Q1', { replyTo: { pmMail: null, t3Thread: null } })
    );
    const results = await answers.submitSet(
      [
        entry('vm.lead--Q1', a1, { note: 'Exactly "this".' }),
        entry('mac.other--Q1', b1, { choice: ['B'] }),
        entry('vm.lead--Q2', a2, { choice: [], note: 'Why not both?' }),
        entry('vm.quiet--Q1', none),
        entry('vm.lead--Q9', a1),
      ],
      'iPhone'
    );
    expect(results.map((r) => r.outcome)).toEqual([
      'recorded',
      'recorded',
      'recorded',
      'recorded',
      'card_changed',
    ]);
    await settle();
    expect(rings.map((r) => [r.to, r.subject]).sort()).toEqual([
      ['lead', '2 answers from the owner (F-questions)'],
      ['other', '1 answer from the owner'],
    ]);
    const lead = rings.find((r) => r.to === 'lead')?.body ?? '';
    expect(lead).toContain('Batch: F-questions');
    expect(lead).toContain('Cards: vm.lead--Q1, vm.lead--Q2');
    expect(lead).toContain('Words: Exactly "this".');
    expect(lead).toContain('Words: Why not both?');
    expect(lead).toContain('Choice: none');
    expect(lead).not.toContain('mac.other');
    const quiet = answers.history().find((e) => e.answer.cardId === 'vm.quiet--Q1');
    expect(quiet && deliveryView(quiet)).toMatchObject({
      state: 'rang',
      detail: 'No mail name; the agent reads its answers file.',
    });
    const [send] = answers.sendList();
    expect(send.summary.state).toBe('done');
    expect(lead).toContain(`agent-reports/v1/answers/batches/${send.sendId}.json`);
    const file = fs.readFileSync(
      path.join(root, 'v1', 'answers', 'batches', `${send.sendId}.json`)
    );
    const pub = fs.readFileSync(path.join(root, 'v1', 'keys', 'aac-answers.pub'), 'utf8');
    const checked = verifySignedDocument(file, pub, SEND_SCHEMA);
    expect(checked.ok).toBe(true);
    if (!checked.ok) return;
    expect(checked.answer.sendId).toBe(send.sendId);
    expect(
      (checked.answer.entries as Array<Record<string, unknown>>).map((e) => [
        e.cardId,
        e.kind,
        e.choice,
        e.note,
      ])
    ).toEqual([
      ['vm.lead--Q1', 'answer', ['A'], 'Exactly "this".'],
      ['mac.other--Q1', 'answer', ['B'], ''],
      ['vm.lead--Q2', 'note', [], 'Why not both?'],
      ['vm.quiet--Q1', 'answer', ['A'], ''],
    ]);
    const tampered = JSON.parse(file.toString());
    tampered.entries[0].note = 'Something else';
    expect(verifySignedDocument(JSON.stringify(tampered), pub, SEND_SCHEMA).ok).toBe(false);
  });

  it('retries a failed ring once per asker, and a failed summary', async () => {
    let ok = false;
    const calls: string[] = [];
    const { root, store, answers } = setup(async (to) => {
      calls.push(to);
      return ok ? { ok: true, detail: 'Rang.' } : { ok: false, detail: 'pm-mail failed.' };
    });
    const q1 = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const q2 = writeCard(root, cardDoc('vm.lead', 'Q2'));
    fs.mkdirSync(path.join(root, 'v1', 'answers'), { recursive: true });
    fs.writeFileSync(path.join(root, 'v1', 'answers', 'batches'), 'blocker');
    await answers.submitSet([entry('vm.lead--Q1', q1), entry('vm.lead--Q2', q2)], 'Mac');
    await settle();
    expect(calls).toEqual(['lead']);
    expect(answers.sendList()[0].summary.state).toBe('failed');
    ok = true;
    fs.rmSync(path.join(root, 'v1', 'answers', 'batches'));
    await answers.retryPending();
    expect(calls).toEqual(['lead', 'lead']);
    expect(answers.sendList()[0].summary.state).toBe('done');
    expect(answers.history().every((e) => deliveryView(e).state === 'rang')).toBe(true);
    // A new store reads the send and its summary steps back from the private store.
    const again = new StatusAnswers({ readCard: async () => null, storeDir: () => store });
    expect(again.sendList()).toHaveLength(1);
    expect(again.sendList()[0].summary).toMatchObject({ state: 'done', attempts: 2 });
    expect(again.sendList()[0].entryIds).toHaveLength(2);
  });

  it('sends the ring body on stdin, not as an argument', async () => {
    const seen: Array<{ args: string[]; input?: string; deadlineAt?: number }> = [];
    const run: CommandRunner = async (_file, args, options) => {
      seen.push({ args, input: options.input, deadlineAt: options.deadlineAt });
      return { code: 0, stdout: 'sent x', timedOut: false, missing: false };
    };
    const deadlineAt = Date.now() + 100;
    const result = await pmMailDoorbell(run)(
      'lead',
      '1 answer from the owner',
      'x'.repeat(200_000),
      deadlineAt
    );
    expect(result.ok).toBe(true);
    expect(seen[0].args).toEqual([
      '--as',
      'aac-answers',
      'send',
      'lead',
      '1 answer from the owner',
    ]);
    expect(seen[0].input).toHaveLength(200_000);
    expect(seen[0].deadlineAt).toBe(deadlineAt);
  });
});

describe('judgement and sample cards', () => {
  it('refuses a sample and an anchored judgement, and takes a plain judgement', async () => {
    const { root, answers } = setup();
    const sample = writeCard(root, cardDoc('vm.lead', 'S1', { kind: 'sample' }));
    const anchored = writeCard(root, cardDoc('vm.lead', 'J1', { kind: 'judgement' }));
    const plain = writeCard(
      root,
      cardDoc('vm.lead', 'J2', {
        kind: 'judgement',
        recommendation: null,
        options: [
          { key: 'A', label: 'Same meaning' },
          { key: 'B', label: 'Different' },
          { key: 'C', label: 'Can’t tell' },
        ],
      })
    );
    const results = await answers.submitSet(
      [
        entry('vm.lead--S1', sample),
        entry('vm.lead--J1', anchored),
        entry('vm.lead--J2', plain, { choice: ['C'] }),
      ],
      'Mac'
    );
    expect(results.map((r) => r.code ?? r.outcome)).toEqual([
      'sample_card',
      'judgement_anchored',
      'recorded',
    ]);
    expect(results[0].error).toBe('This is a format sample; no answer is wanted.');
  });

  it('shows an anchored judgement as a problem card and a sample as not answerable', async () => {
    const root = dirs.make();
    writeReport(root);
    const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
    writeCard(root, cardDoc('vm.lead', 'J1', { kind: 'judgement' }));
    writeCard(root, cardDoc('vm.lead', 'S1', { kind: 'sample' }));
    writeCard(root, cardDoc('vm.lead', 'J2', { kind: 'judgement', recommendation: null }));
    await reader.scan();
    const empty = { data: null, asOf: null, error: null };
    const view = buildStatusView({
      now: NOW,
      timeZone: 'UTC',
      reports: reader.snapshot(),
      registry: empty,
      machines: { vm: empty, mac: empty, windows: empty, nas1: empty, alexis: empty },
      browsers: empty,
      simlock: empty,
      usage: null,
      answers: [],
    });
    const byId = Object.fromEntries(view.cards.open.map((c) => [c.id, c]));
    expect(byId['vm.lead--J1']).toMatchObject({
      kind: 'judgement',
      answerable: false,
      problem: JUDGEMENT_ANCHORED,
    });
    expect(byId['vm.lead--S1']).toMatchObject({ kind: 'sample', answerable: false, problem: null });
    expect(byId['vm.lead--J2']).toMatchObject({
      kind: 'judgement',
      answerable: true,
      problem: null,
    });
  });
});

function writeReport(root: string): void {
  const file = path.join(root, 'v1', 'writers', 'vm.lead', 'thread.json');
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(threadDoc('vm.lead')));
}

describe('fields from the threads’ feedback', () => {
  const parseCard = (fields: Record<string, unknown>) =>
    parseReportFile('card', json(cardDoc('vm.lead', 'Q1', fields)), 'vm.lead', 'vm.lead--Q1');

  it('passes through verdict, group, agreement, doubt, appliesTo, changesCanon, related and stance', () => {
    const parsed = parseCard({
      project: null,
      stream: null,
      replyTo: { pmMail: null, t3Thread: null },
      verdict: 'adapt',
      group: 'retention',
      agreement: { same: false, note: 'Judges split 2-1' },
      doubt: 'The sample may be too small.',
      appliesTo: { count: 14, description: 'Every superseded note in the plan' },
      changesCanon: [{ ref: 'PLAN.md#retention', what: 'Keep superseded notes' }],
      related: ['vm.lead--Q2'],
      options: [
        { key: 'A', label: 'Keep ours', stance: 'keep' },
        { key: 'B', label: 'Borrow theirs', stance: 'borrow' },
      ],
    });
    expect(parsed.ok).toBe(true);
    if (!parsed.ok) return;
    const doc = parsed.value.doc;
    expect(doc).toMatchObject({
      project: null,
      stream: null,
      replyTo: { pmMail: null, t3Thread: null },
      verdict: 'adapt',
      group: 'retention',
      agreement: { same: false, note: 'Judges split 2-1' },
      appliesTo: { count: 14, description: 'Every superseded note in the plan' },
      related: ['vm.lead--Q2'],
    });
    expect(doc.options.map((o) => o.stance)).toEqual(['keep', 'borrow']);
    expect(
      parseCard({
        options: [
          { key: 'A', label: 'A', stance: 'steal' },
          { key: 'B', label: 'B' },
        ],
      })
    ).toEqual({
      ok: false,
      reason: 'options[0].stance is missing or not valid',
    });
    expect(parseCard({ appliesTo: { count: 3 } })).toEqual({
      ok: false,
      reason: 'appliesTo.description is missing or not valid',
    });
  });

  it('validates quote and planChange sections', () => {
    const ok = parseCard({
      sections: [
        {
          type: 'quote',
          title: 'What the plan says',
          items: [
            {
              text: 'Keep every fact.\n\nNever summarise.',
              where: 'Plan > Retention',
              context: ['Superseded 2026-10-09'],
              source: 'PLAN.md',
              link: { label: 'Plan', url: '/mnt/Cursor/share/plan.md' },
              version: 'v3',
            },
          ],
        },
        {
          type: 'planChange',
          doc: 'PLAN.md',
          unit: 'Retention',
          now: 'Keep all',
          proposed: 'Keep 90 days',
        },
      ],
    });
    expect(ok.ok).toBe(true);
    if (ok.ok) {
      expect(ok.value.doc.sections[0]).toMatchObject({ type: 'quote', fold: false });
      expect(ok.value.doc.sections[1]).toEqual({
        type: 'planChange',
        title: null,
        fold: false,
        doc: 'PLAN.md',
        unit: 'Retention',
        now: 'Keep all',
        proposed: 'Keep 90 days',
      });
    }
    expect(
      parseCard({ sections: [{ type: 'planChange', doc: 'PLAN.md', unit: 'U', now: 'x' }] })
    ).toEqual({
      ok: false,
      reason: 'sections[0].proposed is missing or not valid',
    });
    expect(parseCard({ sections: [{ type: 'quote', items: [{ where: 'x' }] }] })).toEqual({
      ok: false,
      reason: 'sections[0].items[0].text is missing or not valid',
    });
  });

  it('takes threads with unknown doing, agent fields, project and stream', () => {
    const parsed = parseReportFile(
      'thread',
      json(
        threadDoc('vm.t3-1a2b3c4d', {
          doing: null,
          project: null,
          stream: null,
          agent: { name: null, model: null },
        })
      ),
      'vm.t3-1a2b3c4d'
    );
    expect(parsed.ok && parsed.value.doc.agent.model).toBeNull();
  });
});

describe('runs', () => {
  const run = (fields: Record<string, unknown> = {}) => ({
    schema: 'agent-reports/run@1',
    id: 'vm.lead--R1',
    writer: 'vm.lead',
    seq: 4,
    writtenAt: '2026-11-06T16:00:00Z',
    source: 'observed',
    project: null,
    stream: null,
    task: { id: 'B09', type: null, title: 'Re-audit batch B09' },
    model: { id: null, provider: null, harness: null, effort: null, account: 'zcode' },
    startedAt: null,
    endedAt: '2026-11-06T16:30:00Z',
    outcome: 'done',
    cost: {
      usd: null,
      credits: null,
      quota: null,
      tokens: { in: null, out: null, cached: null },
      estimate: {
        source: 'quota-watch',
        method: 'token-share at list price',
        credits: 12.4,
        quota: { account: 'zcode', window: '5h', deltaPct: 3.5 },
        tokens: { in: 1000, out: 200, cached: null },
      },
    },
    review: {
      of: 'vm.lead--R0',
      verdict: 'fail',
      findings: { high: 1, medium: 0, low: 2 },
      overturned: true,
      by: 'gpt-6.1-sol/max',
      disputed: true,
    },
    flags: [],
    evidence: [],
    process: { id: 'scout', version: '2.3', sha256: null },
    metrics: { factsKept: 72, repairs: null },
    imported: { from: 'LEAD-STATE.md', at: '2026-11-05T10:00:00Z' },
    ...fields,
  });

  it('accepts the new run fields and keeps unknowns null', () => {
    const parsed = parseReportFile('run', json(run()), 'vm.lead');
    expect(parsed.ok).toBe(true);
    if (!parsed.ok) return;
    const doc = parsed.value.doc;
    expect(doc.model).toEqual({
      id: null,
      provider: null,
      harness: null,
      effort: null,
      account: 'zcode',
    });
    expect(doc.startedAt).toBeNull();
    expect(doc.cost?.estimate?.credits).toBe(12.4);
    expect(doc.cost?.credits).toBeNull();
    expect(doc.review).toMatchObject({ by: 'gpt-6.1-sol/max', disputed: true });
    expect(doc.process).toEqual({ id: 'scout', version: '2.3', sha256: null });
    expect(doc.metrics).toEqual({ factsKept: 72, repairs: null });
    expect(doc.imported?.from).toBe('LEAD-STATE.md');
  });

  it('refuses bad metrics, flags and ids', () => {
    expect(parseReportFile('run', json(run({ metrics: { 'bad name': 1 } })), 'vm.lead').ok).toBe(
      false
    );
    expect(parseReportFile('run', json(run({ metrics: { x: 'many' } })), 'vm.lead')).toEqual({
      ok: false,
      reason: 'metrics.x is missing or not valid',
    });
    expect(
      parseReportFile(
        'run',
        json(run({ flags: [{ kind: 'lazy', detail: 'x', evidence: 'y' }] })),
        'vm.lead'
      )
    ).toEqual({ ok: false, reason: 'flags[0].kind is missing or not valid' });
    expect(parseReportFile('run', json(run({ id: 'vm.other--R1' })), 'vm.lead')).toEqual({
      ok: false,
      reason: 'id does not name its writer',
    });
  });
});

describe('published schemas', () => {
  it('copies the packaged schemas into v1/schemas when missing or different, and only there', async () => {
    const root = dirs.make();
    expect(await publishSchemas(root)).toEqual([]);
    await ensureReportsLayout(root);
    const names = fs
      .readdirSync(packagedSchemasDir())
      .filter((n) => n.endsWith('.json'))
      .sort();
    expect(names).toEqual([
      'answer.json',
      'card.json',
      'run.json',
      'status.json',
      'tasks.json',
      'thread.json',
    ]);
    expect(await publishSchemas(root)).toEqual(names);
    expect(await publishSchemas(root)).toEqual([]);
    const card = path.join(root, 'v1', 'schemas', 'card.json');
    fs.writeFileSync(card, '{}');
    expect(await publishSchemas(root)).toEqual(['card.json']);
    expect(
      fs.readFileSync(card).equals(computerReportSchema(fs.readFileSync(path.join(packagedSchemasDir(), 'card.json'))))
    ).toBe(true);
    expect(
      fs.readdirSync(path.join(root, 'v1', 'schemas')).filter((n) => n.startsWith('.'))
    ).toEqual([]);
  });
});
