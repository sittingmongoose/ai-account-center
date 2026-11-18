import { afterEach, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import fs from 'fs';
import path from 'path';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import { ANSWER_SCHEMA } from '../../../../src/web-server/status/report-schema';
import {
  loadOrCreateAnswerKey,
  signAnswer,
} from '../../../../src/web-server/status/answer-signing';
import { StatusAnswers } from '../../../../src/web-server/status/answers';
import { PinnedThreadsCollector } from '../../../../src/web-server/status/pinned-threads';
import { StatusService } from '../../../../src/web-server/status/status-service';
import { buildStatusView } from '../../../../src/web-server/status/status-view';
import { cardDoc, tempDirs, threadDoc, writeReport } from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

async function fixture(exact?: string) {
  const home = dirs.make();
  const root = path.join(home, 'agent-reports');
  const store = path.join(home, '.ccs', 'status');
  const now = Date.parse('2026-11-18T18:51:00Z');
  const fixtureCard = cardDoc('vm.lead', 'SR1', { seq: 2308 });
  const bytes = exact
    ? fs.readFileSync(path.join(exact, 'card.json'))
    : Buffer.from(JSON.stringify(fixtureCard));
  const card = JSON.parse(bytes.toString()) as typeof fixtureCard;
  const key = exact ? null : loadOrCreateAnswerKey(store);
  const hash = createHash('sha256').update(bytes).digest('hex');
  const answer = exact
    ? JSON.parse(fs.readFileSync(path.join(exact, 'answer.json'), 'utf8'))
    : signAnswer(
        {
          schema: ANSWER_SCHEMA,
          cardId: card.id,
          cardSeq: card.seq,
          cardSha256: hash,
          kind: 'answer',
          choice: ['A'],
          note: '',
          via: 'deck',
          answeredAt: '2026-11-18T18:50:41.881Z',
          answeredBy: 'owner',
          device: 'iPhone',
        },
        key!
      );
  expect(answer.cardSha256).toBe(hash);
  expect(answer.cardSeq).toBe(card.seq);
  const cardFile = path.join(root, 'v1', 'writers', card.writer, 'cards', card.id + '.json');
  const answerFile = path.join(root, 'v1', 'answers', card.id + '.json');
  fs.mkdirSync(path.dirname(cardFile), { recursive: true });
  fs.mkdirSync(path.dirname(answerFile), { recursive: true });
  fs.writeFileSync(cardFile, bytes);
  if (exact) {
    fs.copyFileSync(path.join(exact, 'answer.json'), answerFile);
  } else {
    fs.writeFileSync(answerFile, JSON.stringify(answer));
  }
  const threadId = 'pin-fixture';
  const what = 'Card SR1: pick the design';
  writeReport(
    root,
    card.writer,
    'thread.json',
    threadDoc(card.writer, {
      agent: { t3Thread: threadId },
      blockedOn: { kind: 'owner', what },
    })
  );
  writeReport(root, card.writer, 'status.json', {
    schema: 'agent-reports/status@1',
    seq: 1,
    writer: card.writer,
    t3Thread: threadId,
    writtenAt: '2026-11-18T18:35:40Z',
    blockers: [
      { text: what, waitingOn: 'owner' },
      { text: 'CI', waitingOn: 'review' },
    ],
  });
  let pinnedInput:
    | {
        openCards: Array<{ id: string }>;
        threads: Array<{ blockedOn: { what: string } | null }>;
        statuses: Array<{ blockers: Array<{ text: string; waitingOn: string }> }>;
      }
    | undefined;
  const pinned = new PinnedThreadsCollector({
    reportsRoot: () => root,
    readRemote: async () => '[]',
    handle: {
      call: async (method, args) => {
        const input = JSON.parse(args);
        switch (method) {
          case 'pins':
            return JSON.stringify({
              pinned: [{ id: threadId, title: 'Fixture', pinOrderKey: 'a' }],
            });
          case 'parseRemotePins':
            return JSON.stringify({ ok: true, pinned: [] });
          case 'mergePins':
            return JSON.stringify({ pinned: [] });
          case 'parseStatus': {
            const bytes = Buffer.from(input.contentBase64, 'base64');
            return JSON.stringify({
              ok: true,
              doc: JSON.parse(bytes.toString()),
              sha256: createHash('sha256').update(bytes).digest('hex'),
            });
          }
          case 'compose':
            pinnedInput = input;
            return JSON.stringify({
              asOfMs: now,
              head: 'Fixture',
              counts: { pinned: 1, looping: 0, waitingOnYou: Number(input.openCards.length > 0) },
              machineProblems: [],
              updates: [
                {
                  id: threadId,
                  title: 'Fixture',
                  looping: { answer: 'no' },
                  blocked: input.openCards.length ? { text: what, waitingOnYou: true } : null,
                },
              ],
            });
          default:
            throw new Error('Unexpected pin operation');
        }
      },
    },
  });
  await pinned.collect();

  const reader = new AgentReportsReader({ root: () => root, now: () => now });
  const answerDeps = {
    storeDir: () => store,
    reportsRoot: () => root,
    readCard: (id: string) => reader.readCardNow(id),
    doorbell: async () => ({ ok: true, detail: 'Fixture doorbell.' }),
    ...(exact
      ? { readPublicKey: () => fs.readFileSync(path.join(exact, 'answer.pub'), 'utf8') }
      : {}),
  };
  const answers = new StatusAnswers(answerDeps);
  const createService = (store: StatusAnswers) =>
    new StatusService({
      reader,
      answers: store,
      pinned,
      now: () => now,
      historyStore: () => null,
      timeZone: () => 'UTC',
      spendNative: null,
      run: async () => {
        throw new Error('No external collectors in this fixture');
      },
      readLocalMachine: async () => {
        throw new Error('No machine sampling in this fixture');
      },
      readRemoteMachine: async () => {
        throw new Error('No machine sampling in this fixture');
      },
    });
  const service = createService(answers);
  return {
    home,
    root,
    card,
    cardFile,
    key,
    answerFile,
    answer,
    service,
    pinned,
    what,
    restart: () => createService(new StatusAnswers(answerDeps)),
    pinnedInput: () => pinnedInput,
  };
}

async function exercise(exact?: string) {
  const {
    home,
    root,
    card,
    cardFile,
    key,
    answerFile,
    answer,
    service,
    restart,
    pinnedInput,
    what,
  } = await fixture(exact);
  // Same seq and hash signed by a different key cannot close the card, even if its key is on the share.
  const otherKey = loadOrCreateAnswerKey(path.join(home, 'untrusted'));
  const keyDir = path.join(root, 'v1', 'keys');
  fs.mkdirSync(keyDir, { recursive: true });
  fs.writeFileSync(path.join(keyDir, otherKey.keyId + '.pub'), otherKey.publicKeyPem);
  const original = fs.readFileSync(answerFile);
  const { signature: _signature, ...unsigned } = answer;
  fs.writeFileSync(answerFile, JSON.stringify(signAnswer(unsigned, otherKey)));
  await service.scanReports();
  expect(service.view().headline.counts.questions).toBe(1);

  // A verified signature over a different hash also leaves this version open.
  if (key) {
    fs.writeFileSync(
      answerFile,
      JSON.stringify(signAnswer({ ...unsigned, cardSha256: '0'.repeat(64) }, key))
    );
    await service.scanReports();
    expect(service.view().headline.counts.questions).toBe(1);
  }
  fs.writeFileSync(answerFile, original);
  await service.scanReports();
  const answered = service.view();
  expect(answered.headline.counts.questions).toBe(0);
  expect(answered.headline.counts.openCards).toBe(0);
  expect(answered.cards.open).toEqual([]);
  expect(answered.cards.answered).toMatchObject([
    { card: { id: card.id, seq: 2308 }, answer: { choice: ['A'], answeredAt: answer.answeredAt } },
  ]);

  fs.writeFileSync(
    cardFile,
    JSON.stringify({ ...card, seq: 2309, whatChanged: 'New information.' })
  );
  await service.scanReports();
  const reasked = service.view();
  expect(reasked.headline.counts.questions).toBe(1);
  expect(reasked.cards.open).toMatchObject([
    { id: card.id, seq: 2309, whatChanged: 'New information.' },
  ]);

  // The old signed answer is still present when another Dashboard starts.
  expect(pinnedInput()?.openCards).toMatchObject([{ id: card.id }]);
  expect(pinnedInput()?.threads[0].blockedOn).toMatchObject({ what });
  expect(pinnedInput()?.statuses[0].blockers).toHaveLength(2);
  const fresh = restart();
  await fresh.scanReports();
  expect(fresh.view().cards.open).toMatchObject([{ id: card.id, seq: 2309 }]);

  if (key) {
    const reopenedHash = createHash('sha256').update(fs.readFileSync(cardFile)).digest('hex');
    const note = signAnswer(
      {
        ...unsigned,
        cardSeq: 2309,
        cardSha256: reopenedHash,
        kind: 'note',
        choice: [],
        note: 'Please use the updated design.',
      },
      key
    );
    fs.writeFileSync(
      path.join(path.dirname(answerFile), card.id + '.note-0123456789ab.json'),
      JSON.stringify(note)
    );
    await fresh.scanReports();
    expect(fresh.view().cards.open).toEqual([]);
    expect(fresh.view().cards.answered).toMatchObject([
      { card: { id: card.id, seq: 2309 }, answer: { kind: 'note', note: note.note } },
    ]);
  }
  fs.writeFileSync(
    cardFile,
    JSON.stringify({
      ...card,
      seq: 2310,
      state: 'withdrawn',
      reason: 'Settled.',
    })
  );
  await fresh.scanReports();
  expect(fresh.view().cards.open).toEqual([]);
  expect(pinnedInput()?.threads[0].blockedOn).toBeNull();
  expect(pinnedInput()?.statuses[0].blockers).toEqual([{ text: 'CI', waitingOn: 'review' }]);
}

it('uses a signed answer when the local journal lacks it, and leaves re-asks open', () =>
  exercise());

const exact = process.env.AAC_SR1_FIXTURE;
if (exact) it('uses the original SR1 card and signed answer', () => exercise(exact));

function counts(view: ReturnType<StatusService['view']>) {
  return [
    view.headline.counts.questions,
    view.cards.answered?.length,
    view.headline.counts.waitingPinned,
  ];
}

it('publishes the answer and linked pinned wait together across a held poll', async () => {
  const { answerFile, service, pinned } = await fixture();
  const signed = fs.readFileSync(answerFile);
  fs.unlinkSync(answerFile);
  await service.scanReports();
  expect(counts(service.view())).toEqual([1, 0, 1]);

  fs.writeFileSync(answerFile, signed);
  const entered = Promise.withResolvers<void>();
  const release = Promise.withResolvers<void>();
  const scanStatusDocs = pinned.scanStatusDocs.bind(pinned);
  pinned.scanStatusDocs = async () => {
    entered.resolve();
    await release.promise;
    await scanStatusDocs();
  };
  const poll = service.scanReports();
  try {
    await entered.promise;
    expect(
      counts(
        buildStatusView(
          typeof service.readRequestInputs === 'function'
            ? await service.readRequestInputs()
            : service.readInputs()
        )
      )
    ).toEqual([1, 0, 1]);
  } finally {
    release.resolve();
    await poll;
  }
  expect(counts(service.view())).toEqual([0, 1, 0]);
});

it('publishes a local send with its recomposed pins before the next reports poll', async () => {
  const { answerFile, answer, service } = await fixture();
  fs.unlinkSync(answerFile);
  await service.scanReports();
  expect(counts(service.view())).toEqual([1, 0, 1]);

  const results = await service.answers.submitSet(
    [
      {
        cardId: answer.cardId,
        cardSeq: answer.cardSeq,
        cardSha256: answer.cardSha256,
        choice: ['A'],
        note: '',
        via: 'deck',
      },
    ],
    'iPhone'
  );
  expect(results[0].outcome).toBe('recorded');
  expect(counts(service.view())).toEqual([1, 0, 1]);
  expect(counts(buildStatusView(await service.readRequestInputs()))).toEqual([0, 1, 0]);
});

it('applies the same signed-copy precedence during a poll and after restart', async () => {
  const { answer, answerFile, key, service, restart } = await fixture();
  const { signature: _signature, ...unsigned } = answer;
  const noteFile = path.join(path.dirname(answerFile), answer.cardId + '.note-0123456789ab.json');
  const write = (file: string, doc: typeof unsigned) => {
    const signed = signAnswer(doc, key!);
    fs.writeFileSync(file, JSON.stringify(signed));
    return signed;
  };
  fs.unlinkSync(answerFile);
  write(noteFile, { ...unsigned, kind: 'note', choice: [], note: 'Earlier.' });
  const recovered = (owner: StatusService) => owner.view().cards.answered?.map((row) => row.answer);
  await service.scanReports();
  expect(recovered(service)).toMatchObject([{ kind: 'note', note: 'Earlier.' }]);

  const choice = write(answerFile, {
    ...unsigned,
    choice: ['B'],
    answeredAt: '2026-11-18T18:50:55Z',
  });
  const cold = restart();
  await cold.scanReports();
  await service.scanReports();
  expect(recovered(service)).toEqual(recovered(cold));
  expect(recovered(cold)).toMatchObject([{ kind: 'answer', choice: ['B'] }]);

  const tied = write(noteFile, {
    ...unsigned,
    kind: 'note',
    choice: [],
    note: 'Same instant.',
    answeredAt: '2026-11-18T18:50:55.000Z',
  });
  const expected = tied.signature.value > choice.signature.value ? tied : choice;
  await service.scanReports();
  const restarted = restart();
  await restarted.scanReports();
  expect(recovered(service)).toEqual(recovered(restarted));
  expect(recovered(restarted)).toMatchObject([
    { kind: expected.kind, choice: expected.choice, note: expected.note },
  ]);

  write(noteFile, {
    ...unsigned,
    kind: 'note',
    choice: [],
    note: 'Latest.',
    answeredAt: '2026-11-18T18:50:59Z',
  });
  await service.scanReports();
  await restarted.scanReports();
  expect(recovered(service)).toEqual(recovered(restarted));
  expect(recovered(service)).toMatchObject([{ kind: 'note', note: 'Latest.' }]);

  // A missing copy during a poll doesn't resurrect a verified version.
  fs.unlinkSync(noteFile);
  fs.unlinkSync(answerFile);
  await service.scanReports();
  expect(recovered(service)).toMatchObject([{ kind: 'note', note: 'Latest.' }]);
});
