/** Regressions from the blind server review of the Status API (docs/status-tab.md). */
import { afterEach, describe, expect, it, spyOn } from 'bun:test';
import { execFileSync } from 'child_process';
import express from 'express';
import fs from 'fs';
import http, { type Server } from 'http';
import type { AddressInfo } from 'net';
import path from 'path';
import { Writable } from 'stream';
import { createStatusRouter } from '../../../../src/web-server/routes/status-routes';
import {
  agentReportsRoot,
  AgentReportsReader,
  DEFAULT_AGENT_REPORTS_DIR,
  newestFolders,
  publishSchemas,
  type ReaderFs,
} from '../../../../src/web-server/status/agent-reports-reader';
import { ringBody } from '../../../../src/web-server/status/answer-delivery';
import {
  deliveryView,
  StatusAnswers,
  type AnswerTiming,
  type Doorbell,
} from '../../../../src/web-server/status/answers';
import {
  QuotaReportReader,
  parseQuotaReport,
} from '../../../../src/web-server/status/quota-report';
import {
  findSecret,
  findSecretIn,
  isCardId,
  isPmMailName,
  isWriterName,
  jsonFromBytes,
  realTimestamp,
  REPORT_SIZE_CAPS,
} from '../../../../src/web-server/status/report-schema';
import { findTypedSecret } from '../../../../src/web-server/status/typed-secrets';
import {
  DEFAULT_PM_MAIL_REGISTRY,
  localMachineId,
  pmMailRegistryDir,
  PmMailRegistryReader,
  runCommand,
} from '../../../../src/web-server/status/status-collectors';
import { openCardFile, sendFileBytes } from '../../../../src/web-server/status/status-files';
import { openedFileIsUnder } from '../../../../src/web-server/status/share-read';
import {
  loadOrCreateAnswerKey,
  signAnswer,
  type UnsignedAnswer,
} from '../../../../src/web-server/status/answer-signing';
import { StatusService } from '../../../../src/web-server/status/status-service';
import { buildStatusView } from '../../../../src/web-server/status/status-view';
import { cardDoc, NOW, tempDirs, threadDoc, writeCard, writeReport } from './status-test-helpers';

const dirs = tempDirs();
let server: Server | undefined;
afterEach(async () => {
  if (server) await new Promise<void>((resolve) => server?.close(() => resolve()));
  server = undefined;
  dirs.cleanup();
});

const PNG = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  Buffer.alloc(200, 7),
]);
const CARD = 'vm.lead--Q1';

function setup(options: { doorbell?: Doorbell; timing?: Partial<AnswerTiming> } = {}) {
  const root = dirs.make('ccs-status-reports-');
  const store = path.join(dirs.make('ccs-status-store-'), 'status');
  const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
  const rings: Array<{ to: string; subject: string; body: string }> = [];
  const doorbell: Doorbell =
    options.doorbell ??
    (async (to, subject, body) => {
      rings.push({ to, subject, body });
      return { ok: true, detail: 'Rang.' };
    });
  const make = (extra: { readCard?: typeof reader.readCardNow; onChange?: () => void } = {}) =>
    new StatusAnswers({
      readCard: extra.readCard ?? ((id) => reader.readCardNow(id)),
      storeDir: () => store,
      reportsRoot: () => root,
      doorbell,
      now: () => NOW,
      timing: options.timing,
      onChange: extra.onChange,
    });
  fs.mkdirSync(path.join(root, 'v1'), { recursive: true });
  return { root, store, reader, rings, answers: make(), make };
}

function entry(cardId: string, sha: string, fields: Record<string, unknown> = {}) {
  return { cardId, cardSeq: 1, cardSha256: sha, choice: ['A'], via: 'deck', ...fields };
}

async function settle(): Promise<void> {
  for (let i = 0; i < 5; i += 1) await new Promise((resolve) => setTimeout(resolve, 5));
}

/** Fails instead of hanging the whole test run when the work blocks. */
function soon<T>(work: Promise<T>, ms = 3000): Promise<T> {
  return Promise.race([
    work,
    new Promise<never>((_, reject) => setTimeout(() => reject(new Error('still blocked')), ms)),
  ]);
}

/** The share stops answering under `v1`; every other path still does. */
function hangShare() {
  const hung = (file: fs.PathLike) => String(file).includes(`${path.sep}v1`);
  const realStat = fs.promises.stat;
  const realMkdir = fs.promises.mkdir;
  const never = new Promise<never>(() => undefined);
  const stat = spyOn(fs.promises, 'stat').mockImplementation(((file: fs.PathLike) =>
    String(file).endsWith(`${path.sep}v1`) ? never : realStat(file)) as typeof fs.promises.stat);
  const mkdir = spyOn(fs.promises, 'mkdir').mockImplementation(((
    file: fs.PathLike,
    options?: fs.MakeDirectoryOptions
  ) => (hung(file) ? never : realMkdir(file, options))) as typeof fs.promises.mkdir);
  return {
    mockRestore() {
      stat.mockRestore();
      mkdir.mockRestore();
    },
  };
}

function fifo(file: string): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  execFileSync('mkfifo', [file]);
}

describe('the test environment', () => {
  it('can never reach the real reports share', () => {
    expect(agentReportsRoot()).not.toBe(DEFAULT_AGENT_REPORTS_DIR);
    expect(pmMailRegistryDir()).not.toBe(DEFAULT_PM_MAIL_REGISTRY);
    expect(process.env.AAC_STATUS_DOORBELL).toBe('off');
  });
});

describe('timestamps are real calendar moments', () => {
  it('takes real dates and refuses rollovers, 24:00, leap seconds and wild offsets', () => {
    for (const ok of [
      '2026-11-06T16:00:00Z',
      '2026-11-06T16:00Z',
      '2026-11-06T16:00:00.123Z',
      '2028-02-29T00:00:00Z',
      '2026-12-31T23:59:59-05:00',
      '2026-01-01T00:00:00+14:00',
      // years 0000-0099 count as written (0000 and 2000 are leap years, 1900 is not)
      '0000-02-29T00:00:00Z',
      '0050-03-01T00:00:00Z',
      '2000-02-29T00:00:00Z',
    ])
      expect([ok, realTimestamp(ok)]).toEqual([ok, true]);
    for (const bad of [
      '2026-99-99T99:99:99Z',
      '2026-02-30T10:00:00Z',
      '2026-02-29T10:00:00Z',
      '2026-04-31T10:00:00Z',
      '2026-13-01T00:00:00Z',
      '2026-00-10T00:00:00Z',
      '2026-01-00T00:00:00Z',
      '2026-11-06T24:00:00Z',
      '2026-11-06T23:60:00Z',
      '2026-11-06T23:59:60Z',
      '2026-11-06T10:00:00+14:30',
      '2026-11-06T10:00:00+05:61',
      '1900-02-29T00:00:00Z',
      '0001-02-29T00:00:00Z',
    ])
      expect([bad, realTimestamp(bad)]).toEqual([bad, false]);
  });
});

describe('secret search', () => {
  const sizes = Object.entries({ ...REPORT_SIZE_CAPS, quota: 512 * 1024 });
  for (const [name, bytes] of sizes)
    it(`takes well under a second on hostile text at the ${name} cap (${bytes / 1024} KB)`, () => {
      for (const unit of ['a', 'a@', 'a@a.', 'eyJ-', '.eyJaaaaaaaa', 'a.', '@']) {
        const text = unit.repeat(Math.ceil(bytes / unit.length)).slice(0, bytes);
        const started = performance.now();
        findSecret(text);
        expect([unit, performance.now() - started < 1000]).toEqual([unit, true]);
      }
    });

  for (const [name, bytes] of sizes)
    it(`the whole scan takes well under a second on hostile strings at the ${name} cap (${bytes / 1024} KB)`, () => {
      // inside a parsed value: runs of backslashes, escapes that make escapes, JSON-ish text that never parses
      for (const unit of ['\\', '\\u', '\\u005c', '\\\\n', '{"a":"', '["', '"\\"', 'a\\']) {
        const value = { note: unit.repeat(Math.ceil(bytes / 2 / unit.length)).slice(0, bytes / 2) };
        const source = JSON.stringify(value);
        const started = performance.now();
        findSecretIn(source, value);
        expect([unit, performance.now() - started < 1000]).toEqual([unit, true]);
      }
      // many short strings shaped like JSON that don't parse (each failed parse throws)
      for (const unit of ['{x}', '[x]', '"\\x"', '{"a":}']) {
        const value = {
          items: Array.from({ length: Math.floor(bytes / (unit.length + 3)) }, () => unit),
        };
        const started = performance.now();
        findSecretIn(JSON.stringify(value), value);
        expect([unit, performance.now() - started < 1000]).toEqual([unit, true]);
      }
    });

  it('sees through JSON escapes in strings and keys', () => {
    const token = `sk-${'A1b2C3d4'.repeat(4)}`;
    for (const json of [
      `{"note":"\\u0073${token.slice(1)}"}`,
      `{"note":"${token.slice(0, 3)}\\u0041${token.slice(4)}"}`,
      `{"pass\\u0077ord":"hunter2-hunter2"}`,
      `{"x":"jared\\u0040mail.example.com"}`,
    ]) {
      expect(findSecret(json)).toBeNull();
      const parsed = jsonFromBytes(Buffer.from(json), 64 * 1024);
      expect([json, parsed.ok]).toEqual([json, false]);
    }
    // the same file without the secret still reads
    expect(jsonFromBytes(Buffer.from('{"note":"\\u0073ome words"}'), 64 * 1024).ok).toBe(true);
  });

  it('sees a token after a newline or tab, JSON text inside a string, and a quoted password', () => {
    const token = `sk-${'A1b2C3d4'.repeat(4)}`;
    const jwt = 'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.abcdefghijklmnop';
    for (const value of [
      { title: `Here is the key:\n${token}` },
      { title: `tab\t${token}` },
      { title: `line\n${jwt}` },
      { title: `Bearer\t${'x'.repeat(24)}` },
      { title: `line\nAKIA${'A'.repeat(16)}` },
      { note: JSON.stringify({ password: 'hunter2-hunter2' }) },
      { password: '"quoted-secret"' },
      // nested JSON text, one or two levels down
      { note: JSON.stringify({ a: `x\n${token}` }) },
      { note: JSON.stringify(JSON.stringify({ password: 'hunter2-hunter2' })) },
      // anything long enough under a credential key
      { password: ['hunter2hunter2'] },
      { password: { value: 'hunter2hunter2' } },
      { api_key: 123456789 },
      // a credential behind 256 decoys that fail to parse
      {
        items: ['{"password":["hunter2222"]}', ...Array.from({ length: 300 }, () => '[x]')],
      },
      // escaped keys and token prefixes inside JSON text inside a string
      { note: '{"pass\\u0077ord":"hunter2-hunter2"}' },
      { note: `{"x":"\\u0073${token.slice(1)}"}` },
      { note: JSON.stringify({ inner: '{"pass\\u0077ord":"hunter2-hunter2"}' }) },
      // double-encoded text and a backslash made by an escape
      { note: JSON.stringify(JSON.stringify({ password: ['hunter2222'] })) },
      { note: `x \\u005cu0073${token.slice(1)}` },
      // credential JSON three levels down, under tool output that ends in [] (no level cap stops the walk)
      {
        note: JSON.stringify({
          result: JSON.stringify({
            content: JSON.stringify([{ text: JSON.stringify({ password: ['hunter2222'] }) }]),
          }),
        }),
      },
    ]) {
      const parsed = jsonFromBytes(Buffer.from(JSON.stringify(value)), 64 * 1024);
      expect([value, parsed.ok]).toEqual([value, false]);
    }
  });

  it('legitimate notes with JSON-looking text read', () => {
    for (const note of [
      '"done"',
      '""',
      '"',
      '"[1,2]"',
      '[link](url)',
      '{not json}',
      JSON.stringify({ result: JSON.stringify({ content: JSON.stringify([{ text: '[]' }]) }) }),
    ]) {
      const parsed = jsonFromBytes(Buffer.from(JSON.stringify({ note })), 64 * 1024);
      expect([note, parsed.ok]).toEqual([note, true]);
    }
  });

  it('a deep card file is named as a problem and the other cards still read', async () => {
    const root = dirs.make();
    writeCard(root, cardDoc('vm.lead', 'Q1'));
    writeReport(
      root,
      'vm.lead',
      'cards/vm.lead--Q2.json',
      Buffer.from(`${'['.repeat(5000)}${']'.repeat(5000)}`)
    );
    const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
    await reader.scan();
    const snap = reader.snapshot();
    expect(snap.state).not.toBe('error');
    expect(snap.cards.map((c) => c.doc.id)).toEqual(['vm.lead--Q1']);
    expect(snap.problems.map((p) => p.file)).toEqual(['writers/vm.lead/cards/vm.lead--Q2.json']);
  });

  it('a file nested thousands of levels deep is refused or read, never a crash of the scan', () => {
    for (const depth of [1_000, 5_000, 100_000]) {
      const deep = Buffer.from(`${'['.repeat(depth)}${']'.repeat(depth)}`);
      expect(() => jsonFromBytes(deep, 512 * 1024)).not.toThrow();
      const quota = parseQuotaReport(deep);
      expect(quota.ok).toBe(false);
    }
  });

  it('still finds real emails and tokens, and only those', () => {
    const jwt = 'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.abcdefghijklmnop';
    for (const [text, kind] of [
      ['write to jared.smith+x@mail.example.com now', 'an email address'],
      ['a@b.co', 'an email address'],
      ['"from":"x%y@host-1.example.org"', 'an email address'],
      [jwt, 'a JSON web token'],
      [`token ${jwt} end`, 'a JSON web token'],
      [`-${jwt}`, 'a JSON web token'],
    ] as const)
      expect([text, findSecret(text)]).toEqual([text, kind]);
    for (const text of [
      'a@b',
      '@example.com',
      'see @ home.example.com',
      'user@host_name',
      'a@b.c',
      'a@b.com_x',
      'xeyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.abcdefghijklmnop',
      'eyJshort.eyJzdWIiOiIxMjM0NTYifQ.abcdefghijklmnop',
      'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ',
    ])
      expect([text, findSecret(text)]).toEqual([text, null]);
  });

  it('agrees with the old patterns on a mix of text', () => {
    const email = /[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b/;
    const jwt = /\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/;
    const pieces = ['a', 'eyJ', '.', '@', '-', '_', 'bcdefghij', ' ', 'com', 'x.y', '"', '%'];
    let state = 12345;
    const next = () => {
      state = (state * 1103515245 + 12345) % 2147483648;
      return state;
    };
    for (let round = 0; round < 3000; round += 1) {
      let text = '';
      for (let i = next() % 14; i >= 0; i -= 1) text += pieces[next() % pieces.length];
      const kind = findSecret(text);
      expect([text, kind === 'an email address' || (kind !== null && email.test(text))]).toEqual([
        text,
        email.test(text),
      ]);
      if (!email.test(text))
        expect([text, kind === 'a JSON web token']).toEqual([text, jwt.test(text)]);
    }
  });

  it('jsonFromBytes refuses a 512 KB run of `a` in fixed words, quickly', () => {
    const started = performance.now();
    expect(jsonFromBytes(Buffer.alloc(512 * 1024, 'a'), 512 * 1024).ok).toBe(false);
    expect(performance.now() - started).toBeLessThan(1000);
  });
});

describe('an older answer cannot replace a newer one', () => {
  const answerFile = (root: string) => path.join(root, 'v1', 'answers', `${CARD}.json`);
  const onShare = (root: string) => JSON.parse(fs.readFileSync(answerFile(root), 'utf8')).cardSeq;

  it('skips the retry of a record a newer answer has replaced, and says so', async () => {
    const { root, answers, rings } = setup();
    const first = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const blocker = path.join(root, 'v1', 'answers');
    fs.writeFileSync(blocker, 'blocker');
    await answers.submitSet([entry(CARD, first)], 'Mac');
    await settle();
    fs.rmSync(blocker);
    const second = writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 2 }));
    await answers.submitSet([entry(CARD, second, { cardSeq: 2 })], 'Mac');
    await settle();
    expect(onShare(root)).toBe(2);
    await answers.retryPending();
    expect(onShare(root)).toBe(2);
    const old = answers.list()[0];
    expect(old.share.state).toBe('skipped');
    expect(old.doorbell.state).toBe('skipped');
    const shown = deliveryView(old);
    expect(shown.state).toBe('failed');
    expect(shown.detail).toContain('newer answer');
    expect(rings).toHaveLength(1);
    // A later pass and a restart change nothing.
    const attempts = old.share.attempts;
    await answers.retryPending();
    expect(answers.list()[0].share.attempts).toBe(attempts);
  });

  it('survives a restart: the skipped step is read back from the store', async () => {
    const { root, answers, make } = setup();
    const first = writeCard(root, cardDoc('vm.lead', 'Q1'));
    fs.writeFileSync(path.join(root, 'v1', 'answers'), 'blocker');
    await answers.submitSet([entry(CARD, first)], 'Mac');
    fs.rmSync(path.join(root, 'v1', 'answers'));
    const second = writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 2 }));
    await answers.submitSet([entry(CARD, second, { cardSeq: 2 })], 'Mac');
    await answers.retryPending();
    const again = make();
    await again.retryPending();
    expect(onShare(root)).toBe(2);
    expect(again.list()[0].share.state).toBe('skipped');
  });

  it('drops a write that wakes up after its deadline, before its rename', async () => {
    const { root, answers } = setup({ timing: { shareMs: 40 } });
    const first = writeCard(root, cardDoc('vm.lead', 'Q1'));
    let release = () => undefined as void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const realOpen = fs.promises.open;
    let held = 0;
    const open = spyOn(fs.promises, 'open').mockImplementation((async (
      ...args: Parameters<typeof fs.promises.open>
    ) => {
      if (String(args[0]).includes(`.${CARD}.json.tmp-`) && held === 0) {
        held += 1;
        await gate;
      }
      return realOpen(...args);
    }) as typeof fs.promises.open);
    try {
      await answers.submitSet([entry(CARD, first)], 'Mac');
      expect(answers.list()[0].share.state).toBe('failed');
      const second = writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 2 }));
      await answers.submitSet([entry(CARD, second, { cardSeq: 2 })], 'Mac');
      expect(onShare(root)).toBe(2);
      release();
      await settle();
      expect(onShare(root)).toBe(2);
      expect(
        fs.readdirSync(path.join(root, 'v1', 'answers')).filter((f) => f.startsWith('.'))
      ).toEqual([]);
    } finally {
      open.mockRestore();
    }
  });

  it('never skips a note: each has its own file', async () => {
    const { root, answers } = setup();
    fs.writeFileSync(path.join(root, 'v1', 'answers'), 'blocker');
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    await answers.submitSet([entry(CARD, sha, { choice: [], note: 'first' })], 'Mac');
    await answers.submitSet([entry(CARD, sha)], 'Mac');
    fs.rmSync(path.join(root, 'v1', 'answers'));
    await answers.retryPending();
    expect(answers.list().map((e) => e.share.state)).toEqual(['done', 'done']);
  });
});

describe('special files on the share', () => {
  it('reads past a FIFO where the quota report belongs and names it', async () => {
    const root = dirs.make();
    fifo(path.join(root, 'v1', 'quota', 'report.json'));
    const reader = new QuotaReportReader({ root: () => root, now: () => NOW });
    await soon(reader.scan());
    expect(reader.snapshot().problem?.reason).toBe('is not a regular file');
  });

  it('does not open a FIFO a card file names', async () => {
    const root = dirs.make();
    fifo(path.join(root, 'v1', 'writers', 'vm.lead', 'images', 'a.png'));
    expect(await soon(openCardFile(root, 'vm.lead', 'images/a.png'))).toEqual({
      ok: false,
      reason: 'not_found',
    });
  });

  it('names a FIFO where a report belongs, and refuses one if it appears after the stat', async () => {
    const root = dirs.make();
    fifo(path.join(root, 'v1', 'writers', 'vm.lead', 'thread.json'));
    const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
    await soon(reader.scan());
    expect(reader.snapshot().problems.map((p) => p.reason)).toEqual(['is not a regular file']);
    // The share's own reader, on a file swapped for a FIFO between the stat and the read.
    const { nodeReaderFs } = await import('../../../../src/web-server/status/agent-reports-reader');
    await expect(
      soon(nodeReaderFs.readFile(path.join(root, 'v1', 'writers', 'vm.lead', 'thread.json'), 1024))
    ).rejects.toThrow('not a regular file');
  });

  it('refuses a link through the share reader', async () => {
    const root = dirs.make();
    const outside = path.join(dirs.make(), 'secret.json');
    fs.writeFileSync(outside, JSON.stringify(threadDoc('vm.lead')));
    const folder = path.join(root, 'v1', 'writers', 'vm.lead');
    fs.mkdirSync(folder, { recursive: true });
    fs.symlinkSync(outside, path.join(folder, 'thread.json'));
    const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
    await reader.scan();
    expect(reader.snapshot().threads).toEqual([]);
    expect(reader.snapshot().problems.map((p) => p.reason)).toEqual(['could not be read']);
  });

  it('holds no more than its cap of a file that grew after the stat', async () => {
    const { nodeReaderFs } = await import('../../../../src/web-server/status/agent-reports-reader');
    const file = path.join(dirs.make(), 'big.json');
    fs.writeFileSync(file, Buffer.alloc(100_000, 'x'));
    expect((await nodeReaderFs.readFile(file, 1000)).length).toBe(1001);
  });

  it('does not hang on a FIFO in the schemas, the keys or the registry', async () => {
    const root = dirs.make();
    const source = dirs.make();
    fs.writeFileSync(path.join(source, 'card.json'), '{}');
    fifo(path.join(root, 'v1', 'schemas', 'card.json'));
    await soon(publishSchemas(root, source));
    expect(fs.readFileSync(path.join(root, 'v1', 'schemas', 'card.json'), 'utf8')).toBe('{}');

    const registry = dirs.make();
    fifo(path.join(registry, 'x.json'));
    expect((await soon(new PmMailRegistryReader(() => registry).scan())).threads).toEqual([]);

    const { root: shared, answers } = setup();
    const sha = writeCard(shared, cardDoc('vm.lead', 'Q1'));
    fifo(path.join(shared, 'v1', 'keys', 'aac-answers.pub'));
    await soon(answers.submitSet([entry(CARD, sha)], 'Mac'));
    expect(answers.list()[0].share.state).toBe('done');
  });
});

describe('media confinement', () => {
  function media(root: string, writer: string, name = 'a.png', bytes = PNG) {
    const dir = path.join(root, 'v1', 'writers', writer, 'images');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, name), bytes);
  }

  it('refuses a writer folder that is a link, wherever it points', async () => {
    const root = dirs.make();
    media(root, 'vm.real');
    const outside = dirs.make();
    fs.writeFileSync(path.join(outside, 'secret.png'), PNG);
    const writers = path.join(root, 'v1', 'writers');
    fs.symlinkSync(outside, path.join(writers, 'vm.evil'));
    fs.symlinkSync(path.join(writers, 'vm.real'), path.join(writers, 'vm.alias'));
    expect(await openCardFile(root, 'vm.evil', 'secret.png')).toEqual({
      ok: false,
      reason: 'not_found',
    });
    expect(await openCardFile(root, 'vm.alias', 'images/a.png')).toEqual({
      ok: false,
      reason: 'not_found',
    });
    const real = await openCardFile(root, 'vm.real', 'images/a.png');
    expect(real.ok).toBe(true);
    if (real.ok) await real.handle.close();
  });

  it('serves a root that is itself reached through a link', async () => {
    const real = dirs.make();
    media(real, 'vm.real');
    const link = path.join(dirs.make(), 'share');
    fs.symlinkSync(real, link);
    const opened = await openCardFile(link, 'vm.real', 'images/a.png');
    expect(opened.ok).toBe(true);
    if (opened.ok) await opened.handle.close();
  });

  it('sniffs and sends one file: a swap after the check serves what was checked', async () => {
    const root = dirs.make();
    media(root, 'vm.real');
    const file = path.join(root, 'v1', 'writers', 'vm.real', 'images', 'a.png');
    const opened = await openCardFile(root, 'vm.real', 'images/a.png');
    if (!opened.ok) throw new Error('not opened');
    const swapped = `${file}.new`;
    fs.writeFileSync(swapped, '<script>not media</script>');
    fs.renameSync(swapped, file);
    const chunks: Buffer[] = [];
    const out = new Writable({
      write(chunk, _encoding, done) {
        chunks.push(chunk);
        done();
      },
    });
    await sendFileBytes(opened.handle, out, 0, opened.size - 1);
    await opened.handle.close();
    expect(Buffer.concat(chunks).equals(PNG)).toBe(true);
  });

  it('destroys the response when the file shrank under it', async () => {
    const file = path.join(dirs.make(), 'a.png');
    fs.writeFileSync(file, PNG);
    const handle = await fs.promises.open(file, 'r');
    fs.truncateSync(file, 50);
    let written = 0;
    let finished = false;
    const out = new Writable({
      write(chunk, _encoding, done) {
        written += chunk.length;
        done();
      },
      final(done) {
        finished = true;
        done();
      },
    });
    out.on('error', () => undefined);
    await sendFileBytes(handle, out, 0, PNG.length - 1);
    await handle.close();
    expect(written).toBe(50);
    expect(out.destroyed).toBe(true);
    expect(finished).toBe(false);
  });
});

describe('an unavailable share', () => {
  it('grows no local folder when the share is not mounted, and says so', async () => {
    const parent = dirs.make();
    const root = path.join(parent, 'agent-reports');
    const store = path.join(dirs.make(), 'status');
    const card = cardDoc('vm.lead', 'Q1');
    const sha = 'a'.repeat(64);
    const rings: string[] = [];
    const answers = new StatusAnswers({
      readCard: async () => ({ doc: card as never, sha256: sha }) as never,
      storeDir: () => store,
      reportsRoot: () => root,
      doorbell: async (to) => {
        rings.push(to);
        return { ok: true, detail: 'Rang.' };
      },
      now: () => NOW,
    });
    const results = await answers.submitSet([entry(CARD, sha)], 'Mac');
    expect(results[0].outcome).toBe('recorded');
    await settle();
    expect(fs.existsSync(root)).toBe(false);
    const [record] = answers.list();
    expect(record.share.state).toBe('failed');
    expect(deliveryView(record).detail).toContain('share is not available');
    expect(rings).toEqual([]);
    // Mounted again: the retry writes it.
    fs.mkdirSync(path.join(root, 'v1'), { recursive: true });
    await answers.retryPending();
    expect(answers.list()[0].share.state).toBe('done');
    expect(fs.existsSync(path.join(root, 'v1', 'answers', `${CARD}.json`))).toBe(true);
  });

  it('a hung card read refuses the entry and frees the queue', async () => {
    const { root, make } = setup({ timing: { readMs: 20 } });
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const answers = make({ readCard: () => new Promise(() => undefined) });
    const results = await soon(
      answers.submitSet([entry(CARD, sha), entry(CARD, sha, { note: 'x' })], 'Mac')
    );
    expect(results.map((r) => r.code)).toEqual(['share_unavailable', 'share_unavailable']);
    expect(results[0].outcome).toBe('refused');
    expect(answers.list()).toEqual([]);
    expect((await soon(answers.submitSet([entry(CARD, sha)], 'Mac')))[0].code).toBe(
      'share_unavailable'
    );
  });

  it('a send on a hung share ends inside its budget; what is stored is retried', async () => {
    const { root, answers } = setup({ timing: { shareMs: 30, sendBudgetMs: 60 } });
    const cards = ['Q1', 'Q2', 'Q3', 'Q4'].map((id) => {
      const sha = writeCard(root, cardDoc('vm.lead', id));
      return entry(`vm.lead--${id}`, sha);
    });
    const stat = hangShare();
    let results;
    const started = Date.now();
    try {
      results = await soon(answers.submitSet(cards, 'Mac'));
    } finally {
      stat.mockRestore();
    }
    expect(Date.now() - started).toBeLessThan(1000);
    // Recorded while the budget lasted; the entries after it were not checked, so not recorded.
    const outcomes = results.map((r) => r.code ?? r.outcome);
    expect(outcomes).toContain('recorded');
    expect(outcomes.at(-1)).toBe('share_unavailable');
    const stored = outcomes.filter((outcome) => outcome === 'recorded').length;
    expect(answers.list()).toHaveLength(stored);
    expect(answers.list().every((e) => e.share.state === 'failed')).toBe(true);
    expect(answers.sendList()[0].summary.state).toBe('pending');
    await answers.retryPending();
    expect(answers.list().every((e) => e.share.state === 'done')).toBe(true);
    expect(answers.sendList()[0].summary.state).toBe('done');
  });

  it('a retry pass on a hung share ends inside its budget, and the flag clears', async () => {
    const { root, answers } = setup({ timing: { shareMs: 20, retryBudgetMs: 50 } });
    fs.writeFileSync(path.join(root, 'v1', 'answers'), 'blocker');
    const cards = ['Q1', 'Q2', 'Q3', 'Q4', 'Q5', 'Q6', 'Q7', 'Q8'].map((id) =>
      entry(`vm.lead--${id}`, writeCard(root, cardDoc('vm.lead', id)))
    );
    await answers.submitSet(cards, 'Mac');
    fs.rmSync(path.join(root, 'v1', 'answers'));
    const attempts = () => answers.list().reduce((sum, e) => sum + e.share.attempts, 0);
    const before = attempts();
    const share = hangShare();
    try {
      await soon(answers.retryPending(), 1000);
    } finally {
      share.mockRestore();
    }
    // Out of budget before the last record: what is left waits, its attempts uncounted.
    expect(attempts() - before).toBeLessThan(cards.length);
    await answers.retryPending();
    expect(answers.list().every((e) => e.share.state === 'done')).toBe(true);
  });

  it('a doorbell that never answers still settles and the record can ring again', async () => {
    let calls = 0;
    const { root, answers } = setup({
      timing: { ringMs: 20 },
      doorbell: (to) => {
        calls += 1;
        return calls === 1
          ? new Promise(() => undefined)
          : Promise.resolve({ ok: true, detail: `Rang ${to}.` });
      },
    });
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    await answers.submitSet([entry(CARD, sha)], 'Mac');
    await new Promise((resolve) => setTimeout(resolve, 60));
    expect(answers.list()[0].doorbell.state).toBe('failed');
    await soon(answers.retryPending());
    expect(answers.list()[0].doorbell.state).toBe('done');
  });
});

describe('what a card can make the doorbell say', () => {
  it('refuses a card that names the broadcast or breaks pm-mail name rules', async () => {
    for (const name of ['all', 'Upper', 'has.dot', '-dash', 'a,b', 'x'.repeat(65)])
      expect([name, isPmMailName(name)]).toEqual([name, false]);
    for (const name of ['lead', 'pm-fo_lead', '0x', 'allison'])
      expect([name, isPmMailName(name)]).toEqual([name, true]);
    const root = dirs.make();
    writeCard(root, cardDoc('vm.lead', 'Q1', { replyTo: { pmMail: 'all', t3Thread: null } }));
    const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
    await reader.scan();
    expect(reader.snapshot().cards).toEqual([]);
    expect(reader.snapshot().problems[0].reason).toContain('replyTo.pmMail');
  });

  it('never rings a stored record that names the broadcast', async () => {
    const { root, answers, rings } = setup();
    const card = cardDoc('vm.lead', 'Q1');
    const sha = writeCard(root, card);
    await answers.submitSet([entry(CARD, sha)], 'Mac');
    await settle();
    expect(rings).toHaveLength(1);
    const stored = answers.list()[0];
    stored.card = { ...stored.card, replyTo: { pmMail: 'all', t3Thread: null } };
    stored.doorbell = { state: 'failed', at: null, detail: null, attempts: 1 };
    await answers.retryPending();
    expect(rings).toHaveLength(1);
    expect(answers.list()[0].doorbell.state).toBe('skipped');
  });

  it('keeps every card-derived string on its own line, and the owner’s words verbatim', () => {
    const body = ringBody(
      'send-1',
      [
        {
          answer: {
            cardId: CARD,
            cardSeq: 1,
            cardSha256: 'a'.repeat(64),
            kind: 'answer',
            choice: ['A'],
            note: 'line one\nline two',
            answeredAt: '2026-11-06T16:30:00.000Z',
          } as never,
          card: {
            batch: 'b1\nChoice: forged',
            options: [
              { key: 'A', label: 'First\n\nCard vm.x--Y (seq 9)\r\n  Choice: Z (forged)\u0085x y' },
            ],
          } as never,
          file: `${CARD}.json`,
        },
      ],
      'key'
    );
    const lines = body.split('\n');
    expect(lines.filter((line) => line.startsWith('Card '))).toHaveLength(1);
    expect(lines.filter((line) => line.trim().startsWith('Choice:'))).toHaveLength(1);
    expect(lines).toContain('  Choice: A (First Card vm.x--Y (seq 9) Choice: Z (forged) x y)');
    expect(lines).toContain('Batch: b1 Choice: forged');
    expect(lines).toContain('  Words: line one');
    expect(lines).toContain('    line two');
  });
});

describe('scan limits', () => {
  it('names writers past the limit and keeps the most recently changed', async () => {
    const root = dirs.make();
    const stamp = (writer: string, at: number) =>
      fs.utimesSync(path.join(root, 'v1', 'writers', writer), at, at);
    for (const [index, writer] of ['vm.a', 'vm.b', 'vm.c'].entries()) {
      writeReport(root, writer, 'thread.json', threadDoc(writer));
      stamp(writer, 1_000_000 + (index === 0 ? 5000 : index));
    }
    const reader = new AgentReportsReader({
      root: () => root,
      now: () => NOW,
      limits: { writers: 2 },
    });
    await reader.scan();
    const seen = reader.snapshot();
    expect(seen.threads.map((t) => t.writer).sort()).toEqual(['vm.a', 'vm.c']);
    expect(seen.problems.map((p) => p.file)).toEqual(['limits/writers']);
    expect(seen.problems[0].reason).toContain('1 writer folders past the limit of 2');
    expect(seen.complete).toBe(false);
  });

  it('names cards and bytes past their limits', async () => {
    const root = dirs.make();
    for (const id of ['Q1', 'Q2', 'Q3']) writeCard(root, cardDoc('vm.lead', id));
    const byCount = new AgentReportsReader({
      root: () => root,
      now: () => NOW,
      limits: { cards: 2 },
    });
    await byCount.scan();
    expect(byCount.snapshot().cards).toHaveLength(2);
    expect(byCount.snapshot().problems.map((p) => p.file)).toEqual(['limits/cards']);
    expect(byCount.snapshot().complete).toBe(false);

    const size = fs.statSync(
      path.join(root, 'v1', 'writers', 'vm.lead', 'cards', `${CARD}.json`)
    ).size;
    const byBytes = new AgentReportsReader({
      root: () => root,
      now: () => NOW,
      limits: { bytes: size + 1 },
    });
    await byBytes.scan();
    expect(byBytes.snapshot().cards).toHaveLength(1);
    expect(byBytes.snapshot().problems.map((p) => p.file)).toEqual(['limits/bytes']);
    expect(byBytes.snapshot().complete).toBe(false);
  });
});

describe('commands', () => {
  it('settles on a command that ignores SIGTERM, and kills it', async () => {
    const started = Date.now();
    const result = await soon(
      runCommand('sh', ['-c', "trap '' TERM; exec sleep 30"], {
        timeoutMs: 50,
        maxBytes: 1024,
        graceMs: 50,
      })
    );
    expect(Date.now() - started).toBeLessThan(2000);
    expect(result).toMatchObject({ code: null, timedOut: true, missing: false });
  });

  it('still answers normally', async () => {
    const result = await runCommand('sh', ['-c', 'cat'], {
      timeoutMs: 5000,
      maxBytes: 1024,
      input: 'hello',
    });
    expect(result).toMatchObject({ code: 0, stdout: 'hello', timedOut: false });
  });
});

describe('lower seq on disk', () => {
  it('names a problem and makes the card unanswerable until a higher seq', async () => {
    const root = dirs.make();
    writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 5 }));
    const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
    await reader.scan();
    writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 4, title: 'Older' }));
    await reader.scan();
    const view = () =>
      buildStatusView({
        now: NOW,
        timeZone: 'UTC',
        reports: reader.snapshot(),
        registry: { data: [], asOf: NOW, error: null },
        machines: Object.fromEntries(
          ['vm', 'mac', 'windows', 'nas1'].map((id) => [
            id,
            { data: null, asOf: null, error: null },
          ])
        ) as never,
        browsers: { data: null, asOf: null, error: null },
        simlock: { data: null, asOf: null, error: null },
        usage: null,
        answers: [],
        hiddenThreadNames: [],
        quota: { state: 'missing', scannedAt: null, good: null, problem: null },
      });
    const card = view().cards.open[0];
    expect(card.seq).toBe(5);
    expect(card.answerable).toBe(false);
    expect(card.problem).toContain('seq');
    writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 6 }));
    await reader.scan();
    expect(view().cards.open[0].answerable).toBe(true);
  });
});

describe('writer names', () => {
  it('forbids `--`, so a card id has one reading', async () => {
    expect(isWriterName('vm.a--b')).toBe(false);
    expect(isWriterName('vm.a-b')).toBe(true);
    // The first `--` ends the writer, and now no writer can hold one.
    expect(isCardId('vm.a--b--Q1')).toBe(true);
    const root = dirs.make();
    writeReport(root, 'vm.a--b', 'thread.json', threadDoc('vm.a--b'));
    const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
    await reader.scan();
    expect(reader.snapshot().problems.map((p) => p.file)).toEqual(['writers/vm.a--b']);
  });
});

describe('double ring', () => {
  it('a retry during the first ring does not ring a later group too', async () => {
    let release = () => undefined as void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const rung: string[] = [];
    const { root, answers } = setup({
      doorbell: async (to) => {
        rung.push(to);
        if (rung.length === 1) await gate;
        return { ok: true, detail: 'Rang.' };
      },
    });
    const a = writeCard(root, cardDoc('vm.one', 'Q1'));
    const b = writeCard(root, cardDoc('vm.two', 'Q1'));
    await answers.submitSet([entry('vm.one--Q1', a), entry('vm.two--Q1', b)], 'Mac');
    await settle();
    expect(rung).toEqual(['one']);
    await answers.retryPending();
    expect(rung).toEqual(['one']);
    release();
    await settle();
    expect(rung).toEqual(['one', 'two']);
  });
});

describe('the client send id', () => {
  const ID = '0123456789abcdef0123456789abcdef';

  it('answers a repeat from the first results: nothing recorded, written or rung again', async () => {
    const { root, answers, rings } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const note = [entry(CARD, sha, { choice: [], note: 'hello' }), { nonsense: true }];
    const first = await answers.submitSet(note, 'Mac', ID);
    await settle();
    expect(first.map((r) => r.outcome)).toEqual(['recorded', 'refused']);
    const files = fs.readdirSync(path.join(root, 'v1', 'answers'), { recursive: true }).sort();
    const again = await answers.submitSet(note, 'Mac', ID);
    await settle();
    expect(again).toEqual(first);
    expect(answers.list()).toHaveLength(1);
    expect(answers.sendList()).toHaveLength(1);
    expect(rings).toHaveLength(1);
    expect(fs.readdirSync(path.join(root, 'v1', 'answers'), { recursive: true }).sort()).toEqual(
      files
    );
  });

  it('keeps the server’s batch id apart from the client id', async () => {
    const { root, answers, rings } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    await answers.submitSet([entry(CARD, sha)], 'Mac', ID);
    await settle();
    const [send] = answers.sendList();
    expect(send.sendId).not.toBe(ID);
    expect(send.sendId).toMatch(/^[0-9a-f-]{36}$/);
    expect(fs.readdirSync(path.join(root, 'v1', 'answers', 'batches'))).toEqual([
      `${send.sendId}.json`,
    ]);
    expect(rings[0].body).toContain(`Send: ${send.sendId}`);
    expect(rings[0].body).not.toContain(ID);
  });

  it('processes an overlapping duplicate in turn and answers it from the first', async () => {
    const { root, make, rings } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    let release = () => undefined as void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const { reader } = setup();
    let reads = 0;
    const answers = make({
      readCard: async (id) => {
        reads += 1;
        await gate;
        return (
          new AgentReportsReader({ root: () => root, now: () => NOW }).readCardNow(id) ??
          reader.readCardNow(id)
        );
      },
    });
    const body = [entry(CARD, sha, { choice: [], note: 'once only' })];
    const first = answers.submitSet(body, 'Mac', ID);
    const second = answers.submitSet(body, 'Mac', ID);
    await settle();
    expect(reads).toBe(1);
    release();
    const [a, b] = await Promise.all([first, second]);
    await settle();
    expect(a[0].outcome).toBe('recorded');
    expect(b).toEqual(a);
    expect(reads).toBe(1);
    expect(answers.list()).toHaveLength(1);
    expect(rings).toHaveLength(1);
  });

  it('remembers across a restart', async () => {
    const { root, answers, make, rings } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const body = [entry(CARD, sha, { choice: [], note: 'once only' })];
    const first = await answers.submitSet(body, 'Mac', ID);
    await settle();
    const restarted = make();
    expect(await restarted.submitSet(body, 'Mac', ID)).toEqual(first);
    await settle();
    expect(restarted.list()).toHaveLength(1);
    expect(rings).toHaveLength(1);
  });

  it('forgets after a day and past 200 ids, and never remembers a send that recorded nothing', async () => {
    const root = dirs.make();
    fs.mkdirSync(path.join(root, 'v1'), { recursive: true });
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
    let now = NOW;
    const answers = new StatusAnswers({
      readCard: (id) => reader.readCardNow(id),
      storeDir: () => path.join(root, 'store'),
      reportsRoot: () => root,
      doorbell: async () => ({ ok: true, detail: 'Rang.' }),
      now: () => now,
    });
    const note = (text: string) => [entry(CARD, sha, { choice: [], note: text })];
    const refusedOnly = await answers.submitSet([{ nonsense: true }], 'Mac', ID);
    expect(refusedOnly[0].outcome).toBe('refused');
    expect((await answers.submitSet(note('real'), 'Mac', ID))[0].outcome).toBe('recorded');
    expect(answers.list()).toHaveLength(1);
    now += 24 * 60 * 60 * 1000;
    await answers.submitSet(note('real'), 'Mac', ID);
    expect(answers.list()).toHaveLength(2);
    for (let i = 0; i < 200; i += 1)
      await answers.submitSet(note(`n${i}`), 'Mac', `other-id-${String(i).padStart(8, '0')}`);
    const count = answers.list().length;
    await answers.submitSet(note('real'), 'Mac', ID);
    expect(answers.list()).toHaveLength(count + 1);
  }, 20_000);

  it('a throw after a record costs no results', async () => {
    const { root, make } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const answers = make({
      onChange: () => {
        throw new Error('listener failed');
      },
    });
    const results = await answers.submitSet([entry(CARD, sha)], 'Mac');
    expect(results[0].outcome).toBe('recorded');
    expect(answers.list()).toHaveLength(1);
  });

  it('a card read that throws refuses that entry and answers the rest', async () => {
    const { root, make } = setup();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const answers = make({
      readCard: () => {
        throw new Error('boom');
      },
    });
    const results = await answers.submitSet([entry(CARD, sha), { nonsense: true }], 'Mac');
    expect(results.map((r) => r.code)).toEqual(['share_unavailable', 'invalid_entry']);
  });
});

describe('cards.complete', () => {
  function faulty(fail: (dir: string) => boolean, code: string): ReaderFs {
    return {
      readdir: async (dir) => {
        if (fail(dir)) throw Object.assign(new Error('fault'), { code });
        return fs.promises.readdir(dir, { withFileTypes: true });
      },
      stat: (file) => fs.promises.stat(file),
      readFile: (file) => fs.promises.readFile(file),
    };
  }
  const empty = { data: null, asOf: null, error: null };
  const complete = (reader: AgentReportsReader, now = NOW) =>
    buildStatusView({
      now,
      timeZone: 'UTC',
      reports: reader.snapshot(),
      registry: { data: [], asOf: NOW, error: null },
      machines: { vm: empty, mac: empty, windows: empty, nas1: empty },
      browsers: empty,
      simlock: empty,
      usage: null,
      answers: [],
      hiddenThreadNames: [],
      quota: { state: 'missing', scannedAt: null, good: null, problem: null },
    }).cards.complete;

  it('is false before the first scan and true after a clean one', async () => {
    const root = dirs.make();
    writeCard(root, cardDoc('vm.lead', 'Q1'));
    const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
    expect(complete(reader)).toBe(false);
    await reader.scan();
    expect(complete(reader)).toBe(true);
  });

  it('is false with no reports folder', async () => {
    const reader = new AgentReportsReader({ root: () => dirs.make(), now: () => NOW });
    await reader.scan();
    expect(reader.snapshot().state).toBe('missing');
    expect(complete(reader)).toBe(false);
  });

  it('is false after a restart while the share returns EIO, and recovers', async () => {
    const root = dirs.make();
    writeCard(root, cardDoc('vm.lead', 'Q1'));
    let broken = true;
    const reader = new AgentReportsReader({
      root: () => root,
      now: () => NOW,
      fs: faulty(() => broken, 'EIO'),
    });
    await reader.scan();
    expect(reader.snapshot().state).toBe('error');
    expect(reader.snapshot().scannedAt).toBe(NOW);
    expect(complete(reader)).toBe(false);
    broken = false;
    await reader.scan();
    expect(complete(reader)).toBe(true);
  });

  it('turns false again when a whole share goes wrong later, and on a stale handle', async () => {
    const root = dirs.make();
    writeCard(root, cardDoc('vm.lead', 'Q1'));
    let code: string | null = null;
    const reader = new AgentReportsReader({
      root: () => root,
      now: () => NOW,
      fs: {
        ...faulty(() => false, ''),
        readdir: async (dir) => {
          if (code) throw Object.assign(new Error('fault'), { code });
          return fs.promises.readdir(dir, { withFileTypes: true });
        },
      },
    });
    await reader.scan();
    expect(complete(reader)).toBe(true);
    code = 'ESTALE';
    await reader.scan();
    expect(complete(reader)).toBe(false);
    code = null;
    await reader.scan();
    expect(complete(reader)).toBe(true);
    code = 'ETIMEDOUT';
    await reader.scan();
    expect(complete(reader)).toBe(false);
  });

  for (const code of ['EIO', 'EACCES', 'ETIMEDOUT', 'ESTALE', 'ENOENT'])
    it(`is false when one writer folder fails with ${code}`, async () => {
      const root = dirs.make();
      writeCard(root, cardDoc('vm.good', 'Q1'));
      writeCard(root, cardDoc('vm.bad', 'Q1'));
      const reader = new AgentReportsReader({
        root: () => root,
        now: () => NOW,
        fs: faulty((dir) => dir.endsWith(`${path.sep}vm.bad`), code),
      });
      await reader.scan();
      expect(reader.snapshot().cards.map((c) => c.writer)).toEqual(['vm.good']);
      expect(complete(reader)).toBe(false);
    });

  it('is false when a cards folder cannot be listed', async () => {
    const root = dirs.make();
    writeCard(root, cardDoc('vm.lead', 'Q1'));
    const reader = new AgentReportsReader({
      root: () => root,
      now: () => NOW,
      fs: faulty((dir) => dir.endsWith(`${path.sep}cards`), 'EIO'),
    });
    await reader.scan();
    expect(complete(reader)).toBe(false);
  });

  for (const code of ['EIO', 'ESTALE'])
    it(`is false when a card file fails to read with ${code}`, async () => {
      const root = dirs.make();
      writeCard(root, cardDoc('vm.lead', 'Q1'));
      const reader = new AgentReportsReader({
        root: () => root,
        now: () => NOW,
        fs: {
          ...faulty(() => false, ''),
          readFile: async () => {
            throw Object.assign(new Error('fault'), { code });
          },
        },
      });
      await reader.scan();
      expect(complete(reader)).toBe(false);
    });

  it('is false while a card refused for its content has no good copy, and true when it keeps one', async () => {
    const root = dirs.make();
    writeCard(root, cardDoc('vm.lead', 'Q1'));
    writeCard(root, cardDoc('vm.lead', 'Q2'));
    const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
    await reader.scan();
    expect(complete(reader)).toBe(true);
    // a damaged rewrite of a card already read: the last good copy is still listed, so the list is whole
    writeReport(root, 'vm.lead', `cards/vm.lead--Q2.json`, Buffer.from('not json'));
    await reader.scan();
    expect(reader.snapshot().problems).toHaveLength(1);
    expect(complete(reader)).toBe(true);
    // after a restart the same file has no good copy: Q2 may be open but isn't listed, so nothing is proved
    const restarted = new AgentReportsReader({ root: () => root, now: () => NOW });
    await restarted.scan();
    expect(restarted.snapshot().problems).toHaveLength(1);
    expect(complete(restarted)).toBe(false);
  });

  it('is false when the last scan is stale, however whole it was', async () => {
    const root = dirs.make();
    writeCard(root, cardDoc('vm.lead', 'Q1'));
    const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
    await reader.scan();
    expect(complete(reader, NOW + 10 * 60_000)).toBe(false);
  });
});

describe('the routes', () => {
  async function start(answersOverride?: { submitSet: () => Promise<never> }) {
    const { root, answers, reader } = setup();
    const service = new StatusService({
      reader,
      answers: (answersOverride ?? answers) as StatusAnswers,
      registryReader: new PmMailRegistryReader(() => path.join(root, 'none')),
      run: async () => ({ code: null, stdout: '', timedOut: false, missing: true }),
      readLocalMachine: async () => {
        throw new Error('not in tests');
      },
      readRemoteMachine: async () => {
        throw new Error('not in tests');
      },
      timeZone: () => 'UTC',
      notify: () => undefined,
    });
    const app = express();
    app.use(express.json());
    app.use((req, _res, next) => {
      Object.assign(req, { session: { authenticated: true } });
      next();
    });
    app.use(
      '/api/status',
      createStatusRouter(() => service)
    );
    server = await new Promise<Server>((resolve) => {
      const s = app.listen(0, '127.0.0.1', () => resolve(s));
    });
    return {
      root,
      answers,
      base: `http://127.0.0.1:${(server.address() as AddressInfo).port}`,
    };
  }

  /** `fetch` normalises some paths and refuses others; this sends the bytes as written. */
  function raw(base: string, url: string, headers: Record<string, string> = {}) {
    return new Promise<{ status: number; type: string; cache: string; body: string }>(
      (resolve, reject) => {
        const { hostname, port } = new URL(base);
        http
          .get({ hostname, port, path: url, headers }, (res) => {
            let body = '';
            res.on('data', (chunk) => {
              body += chunk;
            });
            res.on('end', () =>
              resolve({
                status: res.statusCode ?? 0,
                type: String(res.headers['content-type']),
                cache: String(res.headers['cache-control']),
                body,
              })
            );
          })
          .on('error', reject);
      }
    );
  }

  it('answers a bad range as JSON, typed as JSON and not cached as media', async () => {
    const { base, root } = await start();
    const dir = path.join(root, 'v1', 'writers', 'vm.lead', 'c');
    fs.mkdirSync(dir, { recursive: true });
    const mp4 = Buffer.concat([
      Buffer.from([0, 0, 0, 0x18]),
      Buffer.from('ftypisom'),
      Buffer.alloc(100),
    ]);
    fs.writeFileSync(path.join(dir, 'v.mp4'), mp4);
    const res = await raw(base, '/api/status/files/vm.lead/c/v.mp4', { range: 'bytes=9999-' });
    expect(res.status).toBe(416);
    expect(res.type).toContain('application/json');
    expect(res.cache).toBe('no-store');
    expect(JSON.parse(res.body).code).toBe('range_not_satisfiable');
    const ok = await raw(base, '/api/status/files/vm.lead/c/v.mp4', { range: 'bytes=0-11' });
    expect(ok.status).toBe(206);
    expect(ok.type).toBe('video/mp4');
    expect(ok.cache).toBe('private, max-age=300');
  });

  it('answers an undecodable path 404, as the contract says', async () => {
    const { base } = await start();
    for (const url of ['/api/status/files/vm.lead/%zz', '/api/status/files/%zz/a.png']) {
      const res = await raw(base, url);
      expect([url, res.status]).toEqual([url, 404]);
      expect(JSON.parse(res.body).code).toBe('not_found');
    }
  });

  it('takes a valid sendId and refuses a bad one, without echoing it', async () => {
    const { base, root, answers } = await start();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const post = (body: unknown) =>
      fetch(`${base}/api/status/answers`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', origin: base },
        body: JSON.stringify(body),
      });
    const one = entry(CARD, sha, { choice: [], note: 'once' });
    for (const bad of [
      'short',
      'x'.repeat(65),
      'has space in it here',
      'dots.not.allowed.here',
      7,
      null,
    ]) {
      const res = await post({ sendId: bad, answers: [one] });
      expect([bad, res.status]).toEqual([bad, 400]);
      expect((await res.json()).code).toBe('invalid_body');
    }
    const id = 'a'.repeat(32);
    const first = await (await post({ sendId: id, answers: [one] })).json();
    const again = await (await post({ sendId: id, answers: [one] })).json();
    expect(first.results[0].outcome).toBe('recorded');
    expect(again).toEqual(first);
    expect(answers.list()).toHaveLength(1);
  });

  it('answers a stable 500 if the send itself throws', async () => {
    const { base } = await start({
      submitSet: () => Promise.reject(new Error('secret words')),
    });
    const res = await fetch(`${base}/api/status/answers`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', origin: base },
      body: JSON.stringify({ answers: [{}] }),
    });
    expect(res.status).toBe(500);
    const body = await res.json();
    expect(body.code).toBe('answers_failed');
    expect(body.error).toContain('may have been recorded');
    expect(JSON.stringify(body)).not.toContain('secret words');
  });
});

describe('the machine rows', () => {
  it('maps this computer by its real name', () => {
    expect(localMachineId('ubuntuserver', 'linux')).toBe('vm');
    expect(localMachineId('Jareds-MacBook.local', 'darwin')).toBe('mac');
    expect(localMachineId('DESKTOP-1', 'win32')).toBe('windows');
    expect(localMachineId('nas1', 'linux')).toBe('nas1');
    expect(localMachineId('anything', 'linux')).toBe('vm');
  });

  it('labels the local reading with this computer, and never rings ssh for it', async () => {
    const sample = {
      cores: 4,
      load1: 1,
      busyPct: null,
      memAvailableBytes: 1024 ** 3,
      memTotalBytes: 2 * 1024 ** 3,
      disk: null,
      cpuTimes: null,
    };
    const remote: string[] = [];
    const service = new StatusService({
      reader: new AgentReportsReader({ root: () => dirs.make(), now: () => NOW }),
      registryReader: new PmMailRegistryReader(() => dirs.make()),
      run: async () => ({ code: null, stdout: '', timedOut: false, missing: true }),
      localMachine: () => 'mac',
      readLocalMachine: async () => sample,
      readRemoteMachine: async (machine) => {
        remote.push(machine);
        throw new Error('no answer');
      },
      timeZone: () => 'UTC',
      notify: () => undefined,
      now: () => NOW,
    });
    await service.collectMachines();
    const rows = service.view().machines.rows;
    const row = (id: string) => rows.find((machine) => machine.id === id);
    expect(row('mac')?.cpu.cores).toBe(4);
    expect(row('vm')?.cpu?.cores ?? null).toBeNull();
    expect([...new Set(remote)].sort()).toEqual(['nas1', 'windows']);
  });
});

describe('round two', () => {
  function gated() {
    let release = () => undefined as void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    return { gate, release: () => release() };
  }

  describe('a retry during a send', () => {
    it('does not ring records of a send that is still running', async () => {
      const { root, reader, make, rings } = setup();
      const a = writeCard(root, cardDoc('vm.lead', 'Q1'));
      const b = writeCard(root, cardDoc('vm.lead', 'Q2'));
      const { gate, release } = gated();
      let reads = 0;
      const answers = make({
        readCard: async (id) => {
          reads += 1;
          if (reads === 2) await gate;
          return reader.readCardNow(id);
        },
      });
      const send = answers.submitSet([entry(CARD, a), entry('vm.lead--Q2', b)], 'Mac');
      while (reads < 2) await settle();
      await soon(answers.retryPending());
      expect(rings).toEqual([]);
      release();
      await send;
      await settle();
      expect(rings.map((ring) => ring.subject)).toEqual(['2 answers from the owner']);
      await answers.retryPending();
      expect(rings).toHaveLength(1);
    });

    it('never rings a record whose doorbell is done or skipped', async () => {
      const { root, answers, rings } = setup();
      const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
      await answers.submitSet([entry(CARD, sha)], 'Mac');
      await settle();
      expect(rings).toHaveLength(1);
      const ring = (answers as unknown as { ringGroups: (e: unknown[]) => Promise<void> })
        .ringGroups;
      await ring.call(answers, answers.list());
      const [record] = answers.list();
      record.doorbell = { state: 'skipped', at: null, detail: 'x', attempts: 1 };
      await ring.call(answers, answers.list());
      expect(rings).toHaveLength(1);
    });

    it('asks again in the queue: a summary the send wrote meanwhile is not written twice', async () => {
      const { root, answers } = setup();
      const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
      const { gate, release } = gated();
      const realOpen = fs.promises.open;
      let held = false;
      const open = spyOn(fs.promises, 'open').mockImplementation((async (
        ...args: Parameters<typeof fs.promises.open>
      ) => {
        if (String(args[0]).includes(`${path.sep}batches${path.sep}`)) {
          held = true;
          await gate;
        }
        return realOpen(...args);
      }) as typeof fs.promises.open);
      try {
        const send = answers.submitSet([entry(CARD, sha)], 'Mac');
        while (!held) await settle();
        const retry = answers.retryPending();
        await settle();
        release();
        await soon(Promise.all([send, retry]));
        expect(answers.sendList()[0].summary.attempts).toBe(1);
      } finally {
        open.mockRestore();
      }
    });
  });

  describe('a client that left', () => {
    it('sendFileBytes returns at once for a response already gone, writing nothing', async () => {
      const file = path.join(dirs.make(), 'a.png');
      fs.writeFileSync(file, Buffer.alloc(300_000, 1));
      const handle = await fs.promises.open(file, 'r');
      let writes = 0;
      const out = new Writable({
        write(_chunk, _encoding, done) {
          writes += 1;
          done();
        },
      });
      out.on('error', () => undefined);
      out.destroy();
      await new Promise((resolve) => setTimeout(resolve, 5));
      await soon(sendFileBytes(handle, out, 0, 299_999), 1000);
      await handle.close();
      expect(writes).toBe(0);
    });

    it('stops waiting when the response goes away during a wait', async () => {
      const file = path.join(dirs.make(), 'a.png');
      fs.writeFileSync(file, Buffer.alloc(300_000, 1));
      const handle = await fs.promises.open(file, 'r');
      const out = new Writable({ highWaterMark: 1, write() {} });
      out.on('error', () => undefined);
      setTimeout(() => out.destroy(), 20);
      await soon(sendFileBytes(handle, out, 0, 299_999), 1000);
      await handle.close();
    });

    it('a client that stops taking bytes gives up the wait', async () => {
      const file = path.join(dirs.make(), 'a.png');
      fs.writeFileSync(file, Buffer.alloc(300_000, 1));
      const handle = await fs.promises.open(file, 'r');
      const out = new Writable({ highWaterMark: 1, write() {} });
      out.on('error', () => undefined);
      await soon(sendFileBytes(handle, out, 0, 299_999, 20), 1000);
      await handle.close();
      expect(out.destroyed).toBe(true);
    });

    // Node, as production runs it: Bun's http reports an aborted response differently.
    it('under Node, open files return to their baseline after many aborted requests', async () => {
      const root = dirs.make();
      const bundle = dirs.make();
      const dir = path.join(root, 'v1', 'writers', 'vm.lead', 'c');
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(
        path.join(dir, 'v.mp4'),
        Buffer.concat([
          Buffer.from([0, 0, 0, 0x18]),
          Buffer.from('ftypisom'),
          Buffer.alloc(3 * 1024 * 1024, 1),
        ])
      );
      const built = await Bun.build({
        entrypoints: [
          path.resolve(import.meta.dir, '../../../../src/web-server/status/status-files.ts'),
        ],
        target: 'node',
        format: 'esm',
        outdir: bundle,
        naming: 'status-files.mjs',
      });
      expect(built.success).toBe(true);
      const harness = path.join(bundle, 'harness.mjs');
      fs.writeFileSync(
        harness,
        `import fs from 'fs';
import http from 'http';
import { openCardFile, sendFileBytes } from './status-files.mjs';
const count = () => fs.readdirSync('/proc/self/fd').length;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const server = http.createServer(async (req, res) => {
  // As the route does, with the client gone by the time the file is open.
  const media = await openCardFile(process.argv[2], 'vm.lead', 'c/v.mp4');
  await sleep(20);
  try {
    res.setHeader('Content-Length', String(media.size));
    await sendFileBytes(media.handle, res, 0, media.size - 1);
  } finally {
    await media.handle.close().catch(() => undefined);
  }
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const { port } = server.address();
const baseline = count();
await Promise.all(
  Array.from({ length: 60 }, () =>
    new Promise((resolve) => {
      const request = http.get({ host: '127.0.0.1', port, agent: false });
      request.on('error', resolve);
      request.on('close', resolve);
      // After the request is out, so the server has it, and before the file is open plus 20 ms.
      request.once('finish', () => setTimeout(() => request.destroy(), 5));
    })
  )
);
await sleep(400);
console.log(JSON.stringify({ baseline, after: count() }));
process.exit(0);
`
      );
      const output = execFileSync('node', [harness, root], { encoding: 'utf8', timeout: 15_000 });
      const { baseline, after } = JSON.parse(output);
      expect(after).toBeLessThanOrEqual(baseline + 5);
    }, 25_000);
  });

  describe('a share error is not a changed card', () => {
    const errno = (code: string) => Object.assign(new Error(code), { code });
    const realFs = {
      readdir: (dir: string) => fs.promises.readdir(dir, { withFileTypes: true }),
      stat: (file: string) => fs.promises.stat(file),
      readFile: (file: string) => fs.promises.readFile(file),
    };

    async function outcome(root: string, overrides: Partial<ReaderFs>, sha = 'a'.repeat(64)) {
      const reader = new AgentReportsReader({
        root: () => root,
        now: () => NOW,
        fs: { ...realFs, ...overrides },
      });
      const answers = new StatusAnswers({
        readCard: (id) => reader.readCardNow(id),
        storeDir: () => path.join(dirs.make(), 'status'),
        reportsRoot: () => root,
        doorbell: async () => ({ ok: true, detail: 'Rang.' }),
        now: () => NOW,
      });
      return (await answers.submitSet([entry(CARD, sha)], 'Mac'))[0].code;
    }

    it('EIO on the listing is share_unavailable', async () => {
      const root = dirs.make();
      writeCard(root, cardDoc('vm.lead', 'Q1'));
      expect(
        await outcome(root, {
          readdir: async () => {
            throw errno('EIO');
          },
        })
      ).toBe('share_unavailable');
    });

    it('an unmounted root is share_unavailable', async () => {
      expect(await outcome(path.join(dirs.make(), 'gone'), {})).toBe('share_unavailable');
    });

    it('a stat or read failure on a listed card is share_unavailable', async () => {
      const root = dirs.make();
      writeCard(root, cardDoc('vm.lead', 'Q1'));
      for (const overrides of [
        {
          stat: async () => {
            throw errno('EIO');
          },
        },
        {
          readFile: async () => {
            throw errno('ESTALE');
          },
        },
      ])
        expect(await outcome(root, overrides)).toBe('share_unavailable');
    });

    it('a share that answers "not there" is still card_changed', async () => {
      const root = dirs.make();
      fs.mkdirSync(path.join(root, 'v1'), { recursive: true });
      expect(await outcome(root, {})).toBe('card_changed');
      writeCard(root, cardDoc('vm.lead', 'Q2'));
      expect(await outcome(root, {})).toBe('card_changed');
      writeReport(root, 'vm.lead', `cards/${CARD}.json`, Buffer.from('not json'));
      expect(await outcome(root, {})).toBe('card_changed');
    });
  });

  describe('one oversized file', () => {
    it('takes nothing from the byte budget of the other writers', async () => {
      const root = dirs.make();
      writeReport(root, 'vm.big', 'thread.json', Buffer.from('{}'));
      fs.truncateSync(path.join(root, 'v1', 'writers', 'vm.big', 'thread.json'), 63 * 1024 * 1024);
      writeCard(root, cardDoc('vm.lead', 'Q1'));
      const reader = new AgentReportsReader({
        root: () => root,
        now: () => NOW,
        limits: { bytes: 1024 * 1024 },
      });
      await reader.scan();
      const seen = reader.snapshot();
      expect(seen.cards).toHaveLength(1);
      expect(seen.problems.map((p) => p.file)).toEqual(['writers/vm.big/thread.json']);
      expect(seen.problems[0].reason).toContain('cap');
    });
  });

  describe('special files in cards/', () => {
    for (const [what, make] of [
      ['a FIFO', (file: string) => execFileSync('mkfifo', [file])],
      ['a link', (file: string) => fs.symlinkSync('/etc/hostname', file)],
    ] as const)
      it(`names ${what} and makes the list not complete`, async () => {
        const root = dirs.make();
        writeCard(root, cardDoc('vm.lead', 'Q1'));
        make(path.join(root, 'v1', 'writers', 'vm.lead', 'cards', 'vm.lead--Q2.json'));
        const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
        await reader.scan();
        const seen = reader.snapshot();
        expect(seen.problems.map((p) => [p.file, p.reason])).toEqual([
          ['writers/vm.lead/cards/vm.lead--Q2.json', 'is not a regular file'],
        ]);
        expect(seen.complete).toBe(false);
      });
  });

  it('newestFolders keeps the newest and bounds the stats running at once', async () => {
    let running = 0;
    let most = 0;
    const names = Array.from({ length: 60 }, (_, i) => `w${i}`);
    const kept = await newestFolders(names, 2, async (name) => {
      running += 1;
      most = Math.max(most, running);
      await new Promise((resolve) => setTimeout(resolve, 1));
      running -= 1;
      if (name === 'w7') throw new Error('gone');
      return { mtimeMs: Number(name.slice(1)) } as fs.Stats;
    });
    expect(kept).toEqual(['w59', 'w58']);
    expect(most).toBeLessThanOrEqual(16);
  });

  describe('sendId is bound to its entries', () => {
    const ID = '0123456789abcdef0123456789abcdef';

    it('the same id with different entries is a new send', async () => {
      const { root, answers } = setup();
      const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
      const first = await answers.submitSet(
        [entry(CARD, sha, { choice: [], note: 'one' })],
        'Mac',
        ID
      );
      const second = await answers.submitSet(
        [entry(CARD, sha, { choice: [], note: 'two' })],
        'Mac',
        ID
      );
      expect([first[0].outcome, second[0].outcome]).toEqual(['recorded', 'recorded']);
      expect(answers.list().map((e) => e.answer.note)).toEqual(['one', 'two']);
    });

    it('binding survives a restart, and key order does not matter', async () => {
      const { root, answers, make } = setup();
      const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
      const { via, ...rest } = entry(CARD, sha, { choice: [], note: 'one' });
      await answers.submitSet([{ ...rest, via }], 'Mac', ID);
      const again = make();
      await again.submitSet([{ via, ...rest }], 'Mac', ID);
      expect(again.list()).toHaveLength(1);
      await again.submitSet([entry(CARD, sha, { choice: [], note: 'two' })], 'Mac', ID);
      expect(again.list()).toHaveLength(2);
    });
  });

  describe('the entry id', () => {
    const NOTE_ID = 'a'.repeat(32);
    const ANSWER_ID = 'b'.repeat(32);

    /** The answer card closes and the note card stays: the sequence the client recheck found. */
    async function lostResponse() {
      const made = setup();
      const answerSha = writeCard(made.root, cardDoc('vm.lead', 'Q1'));
      const noteSha = writeCard(made.root, cardDoc('vm.lead', 'Q2'));
      const answer = entry(CARD, answerSha, { entryId: ANSWER_ID });
      const note = entry('vm.lead--Q2', noteSha, {
        choice: [],
        note: 'the words',
        entryId: NOTE_ID,
      });
      const first = await made.answers.submitSet([answer, note], 'Mac', 'x'.repeat(32));
      await settle();
      return { ...made, answer, note, first };
    }
    const noteFiles = (root: string) =>
      fs.readdirSync(path.join(root, 'v1', 'answers')).filter((f) => f.includes('.note-'));

    it('a note resent alone after its send lost the response is one record, one file, one ring', async () => {
      const { root, answers, rings, note, first } = await lostResponse();
      expect(first.map((r) => r.outcome)).toEqual(['recorded', 'recorded']);
      const again = await answers.submitSet([note], 'Mac', 'y'.repeat(32));
      await settle();
      expect(again).toEqual([
        { cardId: 'vm.lead--Q2', outcome: 'recorded', code: null, error: null },
      ]);
      expect(answers.list()).toHaveLength(2);
      expect(noteFiles(root)).toHaveLength(1);
      expect(rings).toHaveLength(1);
      expect(answers.sendList()).toHaveLength(1);
      expect(fs.readdirSync(path.join(root, 'v1', 'answers', 'batches'))).toHaveLength(1);
    });

    it('an answer whose card has closed answers recorded, not card_changed', async () => {
      const { answers, rings, answer } = await lostResponse();
      const again = await answers.submitSet([answer], 'Mac');
      await settle();
      expect(again[0]).toMatchObject({ outcome: 'recorded', code: null });
      expect(answers.list()).toHaveLength(2);
      expect(rings).toHaveLength(1);
    });

    it('a repeat mixed with a new entry rings and records only the new one', async () => {
      const { root, answers, rings, note } = await lostResponse();
      const sha = writeCard(root, cardDoc('vm.lead', 'Q3'));
      const results = await answers.submitSet(
        [note, entry('vm.lead--Q3', sha, { entryId: 'c'.repeat(32) })],
        'Mac'
      );
      await settle();
      expect(results.map((r) => r.outcome)).toEqual(['recorded', 'recorded']);
      expect(answers.list()).toHaveLength(3);
      expect(rings).toHaveLength(2);
      expect(rings[1].body).toContain('vm.lead--Q3');
      expect(rings[1].body).not.toContain('the words');
    });

    it('the same id with different content is refused, and nothing is recorded', async () => {
      const { answers, rings, note } = await lostResponse();
      const results = await answers.submitSet([{ ...note, note: 'other words' }], 'Mac');
      await settle();
      expect(results[0]).toMatchObject({ outcome: 'refused', code: 'invalid_entry' });
      expect(answers.list()).toHaveLength(2);
      expect(rings).toHaveLength(1);
    });

    it('the same draft after an agent replied on the card is still one record', async () => {
      const { root, answers, note } = await lostResponse();
      const sha = writeCard(root, cardDoc('vm.lead', 'Q2', { seq: 2 }));
      const results = await answers.submitSet([{ ...note, cardSeq: 2, cardSha256: sha }], 'Mac');
      expect(results[0].outcome).toBe('recorded');
      expect(answers.list()).toHaveLength(2);
    });

    it('two fresh drafts with the same words and different ids are both recorded', async () => {
      const { root, answers } = setup();
      const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
      const draft = (entryId: string) => [entry(CARD, sha, { choice: [], note: 'same', entryId })];
      const sameSend = 'z'.repeat(32);
      await answers.submitSet(draft('1'.repeat(32)), 'Mac', sameSend);
      await answers.submitSet(draft('2'.repeat(32)), 'Mac', sameSend);
      expect(answers.list()).toHaveLength(2);
    });

    it('an entry that was refused is not remembered: its retry can be recorded', async () => {
      const { root, make } = setup({ timing: { readMs: 20 } });
      const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
      let hung = true;
      const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
      const answers = make({
        readCard: (id) => (hung ? new Promise(() => undefined) : reader.readCardNow(id)),
      });
      const draft = [entry(CARD, sha, { choice: [], note: 'x', entryId: NOTE_ID })];
      expect((await answers.submitSet(draft, 'Mac'))[0].code).toBe('share_unavailable');
      hung = false;
      expect((await answers.submitSet(draft, 'Mac'))[0].outcome).toBe('recorded');
    });

    it('remembers across a restart, and never lets the id reach the share or the mail', async () => {
      const { root, make, rings, note } = await lostResponse();
      const restarted = make();
      expect((await restarted.submitSet([note], 'Mac'))[0].outcome).toBe('recorded');
      await settle();
      expect(restarted.list()).toHaveLength(2);
      expect(rings).toHaveLength(1);
      expect(rings[0].body).not.toContain(NOTE_ID);
      const signed = noteFiles(root).map((f) =>
        fs.readFileSync(path.join(root, 'v1', 'answers', f), 'utf8')
      );
      expect(signed.join('')).not.toContain(NOTE_ID);
    });

    it('a batch replay agrees with the entry records', async () => {
      const { answers, answer, note, first, rings } = await lostResponse();
      expect(await answers.submitSet([answer, note], 'Mac', 'x'.repeat(32))).toEqual(first);
      expect(await answers.submitSet([answer, note], 'Mac', 'w'.repeat(32))).toEqual(first);
      await settle();
      expect(answers.list()).toHaveLength(2);
      expect(rings).toHaveLength(1);
    });

    it('keeps every id from the last day and the newest 1,000 past that', () => {
      const { answers } = setup();
      const remember = (id: string, at: number) =>
        (
          answers as unknown as { rememberDraft: (a: string, b: number, c: string) => void }
        ).rememberDraft(id, at, 'h');
      const drafts = (answers as unknown as { drafts: Map<string, unknown> }).drafts;
      const old = NOW - 2 * 24 * 60 * 60 * 1000;
      for (let i = 0; i < 1005; i += 1) remember(`old${i}`, old);
      expect(drafts.size).toBe(1000);
      for (let i = 0; i < 1500; i += 1) remember(`new${i}`, NOW);
      expect(drafts.has('new0')).toBe(true);
      expect(drafts.size).toBe(1500);
    });

    it('the route refuses a bad or repeated entryId with invalid_body', async () => {
      const root = dirs.make();
      const service = new StatusService({
        reader: new AgentReportsReader({ root: () => root, now: () => NOW }),
        registryReader: new PmMailRegistryReader(() => path.join(root, 'none')),
        run: async () => ({ code: null, stdout: '', timedOut: false, missing: true }),
        timeZone: () => 'UTC',
        notify: () => undefined,
      });
      const app = express();
      app.use(express.json());
      app.use((req, _res, next) => {
        Object.assign(req, { session: { authenticated: true } });
        next();
      });
      app.use(
        '/api/status',
        createStatusRouter(() => service)
      );
      server = await new Promise<Server>((resolve) => {
        const s = app.listen(0, '127.0.0.1', () => resolve(s));
      });
      const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
      const one = (entryId: unknown) => entry(CARD, 'a'.repeat(64), { entryId });
      for (const answers of [
        [one('short')],
        [one('A'.repeat(32))],
        [one('g'.repeat(32))],
        [one('a'.repeat(33))],
        [one(null)],
        [one(7)],
        [one('a'.repeat(32)), one('a'.repeat(32))],
      ]) {
        const res = await fetch(`${base}/api/status/answers`, {
          method: 'POST',
          headers: { 'content-type': 'application/json', origin: base },
          body: JSON.stringify({ answers }),
        });
        expect(res.status).toBe(400);
        expect((await res.json()).code).toBe('invalid_body');
      }
    });
  });
});

describe('round three', () => {
  const ID = 'd'.repeat(32);

  it('a store that takes part of a write still records the whole answer, and a note with a secret is refused', async () => {
    const { root, make } = setup();
    const q1 = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const q2 = writeCard(root, cardDoc('vm.lead', 'Q2'));
    // every write takes at most 7 bytes (a file-size limit or a slow disk): the line is still completed
    const realWrite = fs.writeSync;
    const write = spyOn(fs, 'writeSync').mockImplementation(((
      fd: number,
      data: unknown,
      ...rest: unknown[]
    ) => {
      if (Buffer.isBuffer(data)) {
        const offset = (rest[0] as number | undefined) ?? 0;
        const length = Math.min((rest[1] as number | undefined) ?? data.length - offset, 7);
        return realWrite(fd, data, offset, length);
      }
      return (realWrite as (...a: unknown[]) => number)(fd, data, ...rest);
    }) as typeof fs.writeSync);
    try {
      expect((await make().submitSet([entry(CARD, q1)], 'Mac'))[0].outcome).toBe('recorded');
    } finally {
      write.mockRestore();
    }
    await settle();
    expect(
      make()
        .list()
        .map((r) => r.card.id)
    ).toEqual([CARD]);
    // a note holding a token is refused before anything is stored, signed, written or rung
    const token = `sk-${'A1b2C3d4'.repeat(4)}`;
    const answers = make();
    const [result] = await answers.submitSet(
      [entry('vm.lead--Q2', q2, { choice: [], note: `here is the key ${token}` })],
      'Mac'
    );
    expect(result).toMatchObject({ outcome: 'refused', code: 'note_secret' });
    expect(result.error).not.toContain(token);
    expect(make().list()).toHaveLength(1);
  });

  it('typed secrets: env-style names are caught, ordinary values with digits are not', () => {
    for (const text of [
      'DB_PASSWORD=hunter2isbad',
      'OPENAI_API_KEY=abc123def456',
      'GITHUB_TOKEN=ghx1234567890ab',
      'client_secret=abcd1234efgh',
      'refresh_token=xyz12345678',
      'AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI1K7MDENG',
      'DB_PASS=hunter2isbad',
      'pass=hunter2isbad',
      'token: "abc123def456"',
    ])
      expect([text, findTypedSecret(text)]).toEqual([text, 'a password or key']);
    for (const text of [
      'token: refresh it later',
      'token_count=128000000',
      'pwd: /home/me/src/v2',
      'token: 2026-11-07',
      'token: 1,000,000',
      'token: https://example.org/v2',
      'passing: 12345678 tests',
    ])
      expect([text, findTypedSecret(text)]).toEqual([text, null]);
  });

  it('typed secrets: glued, suffixed, code, flag, bearer and URL forms are caught', () => {
    for (const text of [
      // environment names with the secret word glued on or followed by a suffix
      'PGPASSWORD=s3cretpass1 psql -h db',
      'DB_PASSWORD_PROD=hunter2isbad',
      'API_KEY_2=abc123def456',
      'PRIVATE_KEY=abc123def456ghi',
      'ACCESS_KEY=abc123def456ghi',
      'SIGNING_KEY=abc123def456ghi',
      'MYSQL_PWD=hunter2isbad',
      'export PASSWORD="hunter2isbad"',
      '$env:DB_PASSWORD = "hunter2isbad"',
      // code: Ruby arrows, Python dicts, camelCase, Go assignment, HTTP headers
      "password => 'hunter2isbad'",
      "{'password': 'hunter2isbad'}",
      'clientSecret: "abcd1234efgh5678"',
      'apiToken := "abcd1234efgh5678"',
      'x-api-key: abcd1234efgh5678',
      'spring.datasource.password=hunter2isbad',
      // prose
      'the api key: abc123def456',
      'my password is hunter2isbad',
      'password: hunter2',
      'note: password=hunter2isbad',
      'Password:\n  hunter2isbad',
      // flags
      'run it with --password hunter2isbad',
      'run it with --token=abcd1234efgh5678',
      // a bearer token too short for the bearer pattern, a URL's password, a value that starts with a slash
      'Authorization: Bearer abc123def456',
      'postgres://admin:hunter2isbad@db:5432/app',
      'secret: /K7MDENGbPxRfiCY2EXAMPLEKEY',
      // a strong value with slashes that is not a path
      'AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY',
      'secret: /K7MDENG/bPxRfiCYEXAMPLEKEY',
      // human passwords: a word and a year or number, judged whole after a password's name
      'password: Summer_2026',
      'password: Winter-2025!',
      'password: Spring.2026',
      'password: Pa55-word',
      'password: qwerty-123456',
      'DB_PASSWORD=my-secret-pass-42',
      'password: abc_123_xyz',
      'password: 12345678',
      // a bare pass, pw or pwd, and one in a short phrase
      'pass: Hunter2xyz',
      'pw: Hunter2xyz',
      'pwd: Hunter2xyz',
      'PWD=Hunter2xyz',
      'db pass: Hunter2xyz',
      // code and command lines
      'config["password"] = "Hunter2xyz"',
      'os.environ["API_KEY"] = "abcd1234efgh5678"',
      "ENV['SECRET'] = 'abcd1234efgh5678'",
      'setenv("PASSWORD", "Hunter2xyz")',
      'setx API_KEY abcd1234efgh5678',
      'password -> Hunter2xyz',
      'mysql -pS3cret99 -h db',
      'curl -u admin:Passw0rd123 https://example.org',
      'redis://:Hunter2xyz@cache:6379',
      'Authorization: Bearer abc1234',
      // a bare name with a human password: an = separator, or a value with a capital or a symbol
      'pass: Summer_2026',
      'pass=Summer_2026',
      'PASS=Summer_2026',
      'pw: Summer_2026',
      'pwd=summer_2026',
      'wifi pass: Summer_2026',
      'user=jared pass=Summer_2026',
      // -p with a separate value after a login or sshpass
      'docker login -u jared -p Summer_2026',
      'sshpass -p Summer_2026 ssh host',
    ])
      expect([text, findTypedSecret(text)]).toEqual([text, 'a password or key']);
  });

  it('typed secrets: test results, paths, numbers, versions and model names are not refused', () => {
    for (const text of [
      'PASS: test_auth_2',
      'PASS: tests/unit/foo2bar.test.ts',
      'pass: 1200/1200',
      'pwd: ./src/v2-beta',
      'PWD=/home/me/src/v2',
      'token: 128000000',
      'token: claude-opus-5-5',
      'token: claude-sonnet-4-5[1m]',
      'model token: gpt-6.1-sol',
      'secret: rotated-2026',
      'token: v1.2.3.4-rc1',
      'token: 2026-11-07T10:00:00Z',
      'pwd: C:\\Users\\me\\v2',
      'secret: ~/keys/v2/id_ed25519',
      'TOKEN_LIMIT=200k',
      'PASSWORD_FILE=/run/secrets/db2',
      'PASSWORD_FILE=secrets/db2-prod.json',
      '--password-file ~/x/db2.txt',
      'max_tokens: 4096',
      'password: $DB_PASSWORD',
      'api_key: <your-key-here>',
      'cache_key: user_123',
      'the token is claude-opus-5-5',
      'password: see the vault entry',
      'OLDPWD=/home/me/x2',
      '--- PASS: TestLogin2FA (0.00s)',
      'Password manager: 1Password',
      'password hash: bcrypt12',
      'GPG key: 4AEE18F83AFDEB23',
      'ssh key: SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8',
      'credentials: config/v2/creds.json',
      'secret scanning: gitleaks8',
      'token is p95=320ms',
      'password: v2.3.1',
      'password: v10.2.3-rc1',
      'deploy key: SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8',
      'password_min_length: 12345678',
      'docker run -u 1000:1000 -p8080:80 img',
      'gcc -pthread -O2 -print0',
      'first pass: test_auth_2',
      'pass: v2_final_2026',
      'secret_ref: vault/aac/prod2',
      'CACHE_KEY=ubuntu2404node20',
      'cacheKey: ubuntu2404node20',
      'idempotency_key=order12345678',
      'sort_key=createdAt2026',
      'token endpoint: /oauth2/token',
      'docker build --secret id=mysecret,src=secret.txt .',
      'docker build --secret id=npmrc,src=$HOME/.npmrc .',
      'the password was wrong3 times',
      'docker run -p 8080:80 img && ssh -p 2222 host',
    ])
      expect([text, findTypedSecret(text)]).toEqual([text, null]);
  });

  it('typed secrets: a long note is searched in linear time', () => {
    for (const filler of [
      'a',
      'a ',
      'password ',
      'token: ',
      'A_',
      '--token ',
      'x://',
      '-pa',
      '-u a:',
      'setx A ',
      '"a", ',
      'a"] = ',
      'a is ',
      'login ',
      'sshpass -x ',
    ])
      for (const text of [filler.repeat(4000 / filler.length), `${filler.repeat(1000)}x`]) {
        const started = performance.now();
        findTypedSecret(text);
        expect([filler, performance.now() - started < 200]).toEqual([filler, true]);
      }
  });

  it('notes: typed secrets and token prefixes are refused by kind; ordinary prose is not', async () => {
    const { root, make } = setup();
    const answers = make();
    const cards = ['Q1', 'Q2', 'Q3', 'Q4', 'Q5', 'Q6', 'Q7', 'Q8', 'Q9'].map((id) => ({
      id: `vm.lead--${id}`,
      sha: writeCard(root, cardDoc('vm.lead', id)),
    }));
    const send = async (i: number, note: string) =>
      (await answers.submitSet([entry(cards[i].id, cards[i].sha, { choice: [], note })], 'Mac'))[0];
    for (const [i, note] of [
      [0, 'password: hunter2isbad'],
      [1, 'api_key=abcdef1234567890'],
      [2, `use glpat-${'A1b2C3'.repeat(4)}`],
      [3, `npm_${'a1B2c3D4e5'.repeat(4).slice(0, 36)}`],
    ] as const) {
      const result = await send(i, note);
      expect([note, result.code]).toEqual([note, 'note_secret']);
      expect(result.error).toMatch(
        /It looks like (a password or key|a GitLab token|an npm token)\./
      );
    }
    for (const [i, note] of [
      [4, 'the token: refresh it later'],
      [5, 'see https://example.org/path/to/page and /home/me/x.py'],
      [6, 'Bearer tokens are fine to mention in prose'],
      [7, 'pin react@18.3.1 and keep sha 1dbdea02'],
    ] as const) {
      const result = await send(i, note);
      expect([note, result.outcome]).toEqual([note, 'recorded']);
    }
  });

  it('a record whose fsync fails is cut back out of the store and never comes back', async () => {
    const { root, store, make } = setup();
    const q1 = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const q2 = writeCard(root, cardDoc('vm.lead', 'Q2'));
    // one good answer first: the key and the store exist, so only the store's own fsync is failed below
    expect((await make().submitSet([entry(CARD, q1)], 'Mac'))[0].outcome).toBe('recorded');
    await settle();
    const storeFile = path.join(store, 'answers.jsonl');
    const before = fs.statSync(storeFile).size;
    const storeIno = fs.statSync(storeFile).ino;
    const realFsync = fs.fsyncSync;
    const fsync = spyOn(fs, 'fsyncSync').mockImplementation(((fd: number) => {
      if (fs.fstatSync(fd).ino === storeIno)
        throw Object.assign(new Error('disk gone'), { code: 'EIO' });
      return realFsync(fd);
    }) as typeof fs.fsyncSync);
    let result;
    try {
      [result] = await make().submitSet([entry('vm.lead--Q2', q2)], 'Mac');
    } finally {
      fsync.mockRestore();
    }
    expect(result?.outcome).toBe('refused');
    // the refused record's bytes are gone, so a restart neither lists nor rings it
    expect(fs.statSync(storeFile).size).toBe(before);
    expect(
      make()
        .list()
        .map((r) => r.card.id)
    ).toEqual([CARD]);
  });

  it('the answer key: a key another process links first wins the race, and no temp file is left', () => {
    const dir = path.join(dirs.make('ccs-status-key-'), 'status');
    const otherDir = path.join(dirs.make('ccs-status-key-other-'), 'status');
    const other = loadOrCreateAnswerKey(otherDir);
    const otherPem = fs.readFileSync(path.join(otherDir, 'answer-key.pem'), 'utf8');
    // between our temp write and our link, another process links its key into place
    const target = path.join(dir, 'answer-key.pem');
    const realLink = fs.linkSync;
    const link = spyOn(fs, 'linkSync').mockImplementation(((from: fs.PathLike, to: fs.PathLike) => {
      if (String(to) === target && !fs.existsSync(target))
        fs.writeFileSync(target, otherPem, { mode: 0o600 });
      return realLink(from, to);
    }) as typeof fs.linkSync);
    let mine;
    try {
      mine = loadOrCreateAnswerKey(dir);
    } finally {
      link.mockRestore();
    }
    expect(mine.keyId).toBe(other.keyId);
    expect(fs.readdirSync(dir).filter((f) => f.startsWith('.answer-key'))).toEqual([]);
  });

  it('the answer key: a link that fails for any other reason falls back to an exclusive write', () => {
    const dir = path.join(dirs.make('ccs-status-key-eacces-'), 'status');
    const link = spyOn(fs, 'linkSync').mockImplementation((() => {
      throw Object.assign(new Error('no links here'), { code: 'EACCES' });
    }) as typeof fs.linkSync);
    let key;
    try {
      key = loadOrCreateAnswerKey(dir);
    } finally {
      link.mockRestore();
    }
    expect(loadOrCreateAnswerKey(dir).keyId).toBe(key.keyId);
    expect(fs.readdirSync(dir).filter((f) => f.startsWith('.answer-key'))).toEqual([]);
  });

  it('the answer key: an empty key file another process is still writing is read again', () => {
    const dir = path.join(dirs.make('ccs-status-key-empty-'), 'status');
    const otherDir = path.join(dirs.make('ccs-status-key-writer-'), 'status');
    const other = loadOrCreateAnswerKey(otherDir);
    const otherPem = fs.readFileSync(path.join(otherDir, 'answer-key.pem'), 'utf8');
    const target = path.join(dir, 'answer-key.pem');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(target, '', { mode: 0o600 });
    // the other process finishes its in-place write after our first read
    let reads = 0;
    const realRead = fs.readFileSync;
    const read = spyOn(fs, 'readFileSync').mockImplementation(((
      file: fs.PathOrFileDescriptor,
      options?: unknown
    ) => {
      if (String(file) === target && ++reads === 2) fs.writeFileSync(target, otherPem);
      return realRead(file, options as BufferEncoding);
    }) as typeof fs.readFileSync);
    let mine;
    try {
      mine = loadOrCreateAnswerKey(dir);
    } finally {
      read.mockRestore();
    }
    expect(mine.keyId).toBe(other.keyId);
  });

  it('the answer key: a broken file another process moves aside under us is read again, not thrown', () => {
    const dir = path.join(dirs.make('ccs-status-key-moved-'), 'status');
    const target = path.join(dir, 'answer-key.pem');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(target, '', { mode: 0o600 });
    // after our reads find it broken, another process moves it aside before we look at its age
    const realStat = fs.statSync;
    let moved = false;
    const stat = spyOn(fs, 'statSync').mockImplementation(((
      file: fs.PathLike,
      options?: unknown
    ) => {
      if (String(file) === target && !moved) {
        moved = true;
        fs.renameSync(target, path.join(dir, '.answer-key.broken.other.tmp'));
      }
      return realStat(file, options as fs.StatSyncOptions);
    }) as typeof fs.statSync);
    let key;
    try {
      key = loadOrCreateAnswerKey(dir);
    } finally {
      stat.mockRestore();
    }
    expect(moved).toBe(true);
    expect(loadOrCreateAnswerKey(dir).keyId).toBe(key.keyId);
  });

  it('the answer key: a key file still broken a minute after its last write is replaced', () => {
    const dir = path.join(dirs.make('ccs-status-key-broken-'), 'status');
    const target = path.join(dir, 'answer-key.pem');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(target, '-----BEGIN PRIVATE KEY-----\nMC4CAQ', { mode: 0o600 });
    // a fresh broken file may still be being written: refused for now, left in place
    expect(() => loadOrCreateAnswerKey(dir)).toThrow(/being written/);
    expect(fs.readFileSync(target, 'utf8')).toContain('MC4CAQ');
    const old = new Date(Date.now() - 2 * 60 * 1000);
    fs.utimesSync(target, old, old);
    const key = loadOrCreateAnswerKey(dir);
    expect(loadOrCreateAnswerKey(dir).keyId).toBe(key.keyId);
    expect(fs.readFileSync(target, 'utf8')).not.toContain('MC4CAQ\n');
  });

  it('a torn last store line does not swallow the next record after a restart', async () => {
    const { root, store, make } = setup();
    const q1 = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const q2 = writeCard(root, cardDoc('vm.lead', 'Q2'));
    expect((await make().submitSet([entry(CARD, q1)], 'Mac'))[0].outcome).toBe('recorded');
    await settle();
    // a crash mid-write: half a line, no newline
    fs.appendFileSync(path.join(store, 'answers.jsonl'), '{"kind":"record","cardId":"vm.le');
    const restarted = make();
    expect((await restarted.submitSet([entry('vm.lead--Q2', q2)], 'Mac'))[0].outcome).toBe(
      'recorded'
    );
    await settle();
    const again = make().list();
    expect(again.map((r) => r.card.id).sort()).toEqual([CARD, 'vm.lead--Q2']);
  });

  describe('the card version in the entry binding', () => {
    it('refuses an answer id replayed on another version of the card', async () => {
      const { root, answers, rings } = setup();
      const first = writeCard(root, cardDoc('vm.lead', 'Q1'));
      const body = entry(CARD, first, { entryId: ID });
      expect((await answers.submitSet([body], 'Mac'))[0].outcome).toBe('recorded');
      await settle();
      // The same version is still a retry, though the card has closed.
      expect((await answers.submitSet([body], 'Mac'))[0].outcome).toBe('recorded');
      const second = writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 2 }));
      const replayed = await answers.submitSet(
        [{ ...body, cardSeq: 2, cardSha256: second }],
        'Mac'
      );
      expect(replayed[0]).toMatchObject({ outcome: 'refused', code: 'invalid_entry' });
      expect(answers.list()).toHaveLength(1);
      await settle();
      expect(rings).toHaveLength(1);
    });

    it('binds the version when the kind is named too, and for a done', async () => {
      const { root, answers } = setup();
      const action = { options: [], recommendation: null, kind: 'action' };
      const first = writeCard(root, cardDoc('vm.lead', 'Q1', action));
      const body = entry(CARD, first, { choice: [], kind: 'done', entryId: ID });
      expect((await answers.submitSet([body], 'Mac'))[0].outcome).toBe('recorded');
      const second = writeCard(root, cardDoc('vm.lead', 'Q1', { ...action, seq: 2 }));
      const replayed = await answers.submitSet(
        [{ ...body, cardSeq: 2, cardSha256: second }],
        'Mac'
      );
      expect(replayed[0].code).toBe('invalid_entry');
      expect(answers.list()).toHaveLength(1);
    });

    it('binds no version for a note or an objection: the words stand', async () => {
      const { root, answers } = setup();
      const objection = { options: [], recommendation: null, kind: 'objection' };
      for (const [local, fields, extra] of [
        ['Q1', {}, { note: 'words' }],
        ['Q2', objection, { note: 'words', kind: 'object' }],
      ] as const) {
        const entryId = local === 'Q1' ? ID : 'e'.repeat(32);
        const cardId = `vm.lead--${local}`;
        const first = writeCard(root, cardDoc('vm.lead', local, fields));
        const body = entry(cardId, first, { choice: [], ...extra, entryId });
        expect((await answers.submitSet([body], 'Mac'))[0].outcome).toBe('recorded');
        const second = writeCard(root, cardDoc('vm.lead', local, { ...fields, seq: 2 }));
        const again = await answers.submitSet([{ ...body, cardSeq: 2, cardSha256: second }], 'Mac');
        expect(again[0]).toMatchObject({ outcome: 'recorded', code: null });
      }
      expect(answers.list()).toHaveLength(2);
    });
  });

  describe('a crash before the send line', () => {
    /** The store as a crash leaves it: the record is there, its send and later steps are not. */
    function crash(store: string): void {
      const file = path.join(store, 'answers.jsonl');
      const kept = fs
        .readFileSync(file, 'utf8')
        .split('\n')
        .filter((line) => line && JSON.parse(line).type === 'answer');
      fs.writeFileSync(file, `${kept.join('\n')}\n`);
    }

    it('rebuilds the summary, so the file the ring names exists when it rings', async () => {
      const { root, store, answers, make, rings } = setup();
      const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
      const body = entry(CARD, sha, { choice: [], note: 'words', entryId: ID });
      await answers.submitSet([body], 'Mac');
      await settle();
      const batches = path.join(root, 'v1', 'answers', 'batches');
      fs.rmSync(batches, { recursive: true });
      crash(store);
      rings.length = 0;

      const restarted = make();
      // A retry of the draft is recognised and starts no send of its own.
      expect((await restarted.submitSet([body], 'Mac'))[0].outcome).toBe('recorded');
      expect(restarted.list()).toHaveLength(1);
      await restarted.retryPending();

      const [{ sendId }] = restarted.list();
      expect(fs.readdirSync(batches)).toEqual([`${sendId}.json`]);
      const summary = JSON.parse(fs.readFileSync(path.join(batches, `${sendId}.json`), 'utf8'));
      expect(summary.entries.map((row: { note: string }) => row.note)).toEqual(['words']);
      expect(rings).toHaveLength(1);
      expect(rings[0].body).toContain(`batches/${sendId}.json`);
    });

    it('a rebuilt summary is written once, across later passes and restarts', async () => {
      const { root, store, answers, make } = setup();
      const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
      await answers.submitSet([entry(CARD, sha)], 'Mac');
      await settle();
      crash(store);
      const restarted = make();
      await restarted.retryPending();
      await restarted.retryPending();
      expect(restarted.sendList()[0].summary.attempts).toBe(1);
      const again = make();
      await again.retryPending();
      expect(again.sendList()).toHaveLength(1);
      expect(again.sendList()[0].summary).toMatchObject({ state: 'done', attempts: 1 });
    });

    it('a finished send still reads back as one send with its own entries', async () => {
      const { root, answers, make } = setup();
      const a = writeCard(root, cardDoc('vm.lead', 'Q1'));
      const b = writeCard(root, cardDoc('vm.lead', 'Q2'));
      await answers.submitSet([entry(CARD, a), entry('vm.lead--Q2', b)], 'Mac');
      await settle();
      const [send] = make().sendList();
      expect(send.entryIds).toEqual(answers.list().map((e) => e.id));
      expect(make().sendList()).toHaveLength(1);
      expect(send.summary.state).toBe('done');
    });
  });
});

describe('round four', () => {
  const answerFile = (root: string) => path.join(root, 'v1', 'answers', `${CARD}.json`);
  const signedSeq = (root: string) => JSON.parse(fs.readFileSync(answerFile(root), 'utf8')).cardSeq;

  describe('an older card version', () => {
    /** Seq 2 answered, then an older seq 1 file lands on the share. */
    async function rolledBack() {
      const made = setup();
      const oldSha = writeCard(made.root, cardDoc('vm.lead', 'Q1'));
      await made.reader.scan();
      const newSha = writeCard(made.root, cardDoc('vm.lead', 'Q1', { seq: 2 }));
      await made.reader.scan();
      expect(
        (await made.answers.submitSet([entry(CARD, newSha, { cardSeq: 2 })], 'Mac'))[0].outcome
      ).toBe('recorded');
      await settle();
      writeCard(made.root, cardDoc('vm.lead', 'Q1'));
      await made.reader.scan();
      return { ...made, oldSha };
    }

    it('a stale answer to it is refused and the signed answer stays', async () => {
      const { root, answers, oldSha, rings } = await rolledBack();
      const results = await answers.submitSet([entry(CARD, oldSha, { choice: ['B'] })], 'Mac');
      await settle();
      expect(results[0]).toMatchObject({ outcome: 'card_changed', code: 'card_changed' });
      expect(signedSeq(root)).toBe(2);
      expect(answers.list()).toHaveLength(1);
      expect(rings).toHaveLength(1);
    });

    it('is refused by the records alone, after a restart with a reader that remembers nothing', async () => {
      const { root, make, oldSha } = await rolledBack();
      const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
      const restarted = make({ readCard: (id) => reader.readCardNow(id) });
      const results = await restarted.submitSet([entry(CARD, oldSha)], 'Mac');
      expect(results[0].code).toBe('card_changed');
      expect(signedSeq(root)).toBe(2);
      expect(restarted.list()).toHaveLength(1);
    });

    it('is refused by the reader too, when nothing has been answered yet', async () => {
      const { root, reader, answers } = setup();
      const oldSha = writeCard(root, cardDoc('vm.lead', 'Q1'));
      await reader.scan();
      writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 2 }));
      await reader.scan();
      writeCard(root, cardDoc('vm.lead', 'Q1'));
      expect(await reader.readCardNow(CARD)).toBeNull();
      const results = await answers.submitSet([entry(CARD, oldSha)], 'Mac');
      expect(results[0].code).toBe('card_changed');
      expect(answers.list()).toEqual([]);
    });

    it('is refused after a note on a later version as well', async () => {
      const { root, answers } = setup();
      const oldSha = writeCard(root, cardDoc('vm.lead', 'Q1'));
      const newSha = writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 2 }));
      await answers.submitSet(
        [entry(CARD, newSha, { cardSeq: 2, choice: [], note: 'words' })],
        'Mac'
      );
      writeCard(root, cardDoc('vm.lead', 'Q1'));
      expect((await answers.submitSet([entry(CARD, oldSha)], 'Mac'))[0].code).toBe('card_changed');
    });

    it('a higher version is still answered, and the same version is unchanged', async () => {
      const { root, answers } = await rolledBack();
      const sha = writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 3 }));
      expect((await answers.submitSet([entry(CARD, sha, { cardSeq: 3 })], 'Mac'))[0].outcome).toBe(
        'recorded'
      );
      expect(signedSeq(root)).toBe(3);
    });

    it('after a restart the page shows it unanswerable, so the owner is not looped', async () => {
      const { root, answers } = await rolledBack();
      const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
      await reader.scan();
      const empty = { data: null, asOf: null, error: null };
      const view = buildStatusView({
        now: NOW,
        timeZone: 'UTC',
        reports: reader.snapshot(),
        registry: { data: [], asOf: NOW, error: null },
        machines: { vm: empty, mac: empty, windows: empty, nas1: empty },
        browsers: empty,
        simlock: empty,
        usage: null,
        answers: answers.list(),
        hiddenThreadNames: [],
        quota: { state: 'missing', scannedAt: null, good: null, problem: null },
      });
      const [card] = view.cards.open;
      expect(card.seq).toBe(1);
      expect(card.answerable).toBe(false);
      expect(card.problem).toContain('seq');
    });
  });

  describe('publishing', () => {
    const unsigned = (cardSeq: number, sha: string): UnsignedAnswer => ({
      schema: 'agent-reports/answer@1',
      cardId: CARD,
      cardSeq,
      cardSha256: sha,
      kind: 'answer',
      choice: ['A'],
      note: '',
      via: 'deck',
      answeredAt: new Date(NOW).toISOString(),
      answeredBy: 'owner',
      device: 'Mac',
    });

    /** A record whose share write failed, then a higher signed answer the store knows nothing of. */
    async function behind(forged = false) {
      const made = setup();
      const sha = writeCard(made.root, cardDoc('vm.lead', 'Q1'));
      const blocker = path.join(made.root, 'v1', 'answers');
      fs.writeFileSync(blocker, 'blocker');
      await made.answers.submitSet([entry(CARD, sha)], 'Mac');
      await settle();
      expect(made.answers.list()[0].share.state).toBe('failed');
      fs.rmSync(blocker);
      fs.mkdirSync(blocker);
      const key = forged
        ? loadOrCreateAnswerKey(path.join(dirs.make(), 'other'))
        : loadOrCreateAnswerKey(made.store);
      fs.writeFileSync(
        answerFile(made.root),
        JSON.stringify(signAnswer(unsigned(9, 'b'.repeat(64)), key))
      );
      return made;
    }

    it('never renames an answer over a signed one with a higher cardSeq', async () => {
      const { root, answers, rings } = await behind();
      await answers.retryPending();
      expect(signedSeq(root)).toBe(9);
      const [record] = answers.list();
      expect(record.share.state).toBe('skipped');
      expect(record.doorbell.state).toBe('skipped');
      expect(deliveryView(record).detail).toContain('newer answer');
      expect(rings).toEqual([]);
      expect(
        fs.readdirSync(path.join(root, 'v1', 'answers')).filter((f) => f.startsWith('.'))
      ).toEqual([]);
    });

    it('holds after a restart: the retry from a fresh start does the same', async () => {
      const { root, make } = await behind();
      const restarted = make();
      await restarted.retryPending();
      expect(signedSeq(root)).toBe(9);
      expect(restarted.list()[0].share.state).toBe('skipped');
      const again = make();
      expect(again.list()[0].share.state).toBe('skipped');
    });

    it('a signed answer copied from another card, or a signed note, blocks nothing', async () => {
      for (const held of [
        { ...unsigned(50, 'c'.repeat(64)), cardId: 'vm.lead--Q9' },
        { ...unsigned(50, 'c'.repeat(64)), kind: 'note' as const, choice: [], note: 'words' },
      ]) {
        const { root, store, answers, rings } = await behind();
        fs.writeFileSync(
          answerFile(root),
          JSON.stringify(signAnswer(held, loadOrCreateAnswerKey(store)))
        );
        await answers.retryPending();
        expect(signedSeq(root)).toBe(1);
        const [record] = answers.list();
        expect(record.share.state).toBe('done');
        expect(deliveryView(record).detail ?? '').not.toContain('newer answer');
        expect(rings).toHaveLength(1);
      }
    });

    it('a file this server cannot read is replaced, not a reason to stop delivery', async () => {
      const { root, answers } = await behind();
      const realOpen = fs.promises.open;
      const open = spyOn(fs.promises, 'open').mockImplementation((async (
        ...args: Parameters<typeof fs.promises.open>
      ) => {
        if (String(args[0]) === answerFile(root))
          throw Object.assign(new Error('denied'), { code: 'EACCES' });
        return realOpen(...args);
      }) as typeof fs.promises.open);
      try {
        await answers.retryPending();
      } finally {
        open.mockRestore();
      }
      expect(signedSeq(root)).toBe(1);
      expect(answers.list()[0].share.state).toBe('done');
    });

    it('a higher seq not signed by this server blocks nothing', async () => {
      const { root, answers } = await behind(true);
      await answers.retryPending();
      expect(signedSeq(root)).toBe(1);
      expect(answers.list()[0].share.state).toBe('done');
    });

    it('a garbled file is replaced, and a read error fails the write to be retried', async () => {
      const { root, answers } = await behind();
      fs.writeFileSync(answerFile(root), 'not json');
      const realOpen = fs.promises.open;
      const open = spyOn(fs.promises, 'open').mockImplementation((async (
        ...args: Parameters<typeof fs.promises.open>
      ) => {
        if (String(args[0]) === answerFile(root))
          throw Object.assign(new Error('fault'), { code: 'EIO' });
        return realOpen(...args);
      }) as typeof fs.promises.open);
      try {
        await answers.retryPending();
      } finally {
        open.mockRestore();
      }
      expect(answers.list()[0].share.state).toBe('failed');
      await answers.retryPending();
      expect(signedSeq(root)).toBe(1);
      expect(answers.list()[0].share.state).toBe('done');
    });

    it('notes keep their own files, whatever the answer file says', async () => {
      const { root, answers } = await behind();
      const sha = writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 10 }));
      const results = await answers.submitSet(
        [entry(CARD, sha, { cardSeq: 10, choice: [], note: 'words' })],
        'Mac'
      );
      expect(results[0].outcome).toBe('recorded');
      expect(answers.list()[1].share.state).toBe('done');
    });
  });

  describe('a folder swapped during the open', () => {
    const PNG_HEAD = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

    function fixture() {
      const root = dirs.make();
      const writer = path.join(root, 'v1', 'writers', 'vm.lead');
      const media = path.join(writer, 'media');
      const outside = dirs.make();
      fs.mkdirSync(media, { recursive: true });
      fs.writeFileSync(path.join(media, 'image.png'), Buffer.concat([PNG_HEAD, Buffer.from('in')]));
      fs.writeFileSync(
        path.join(outside, 'image.png'),
        Buffer.concat([PNG_HEAD, Buffer.from('out')])
      );
      return { root, writer, media, outside, target: path.join(media, 'image.png') };
    }

    /** Runs `swap` once, at the moment the checked path is opened: after every realpath. */
    function swapAtOpen(target: string, swap: () => void) {
      const realOpen = fs.promises.open;
      let swapped = false;
      return spyOn(fs.promises, 'open').mockImplementation((async (
        ...args: Parameters<typeof fs.promises.open>
      ) => {
        if (!swapped && String(args[0]) === target) {
          swapped = true;
          swap();
        }
        return realOpen(...args);
      }) as typeof fs.promises.open);
    }

    it('refuses a parent folder swapped for a link to somewhere else', async () => {
      const { root, media, outside, target } = fixture();
      const open = swapAtOpen(target, () => {
        fs.renameSync(media, `${media}-original`);
        fs.symlinkSync(outside, media);
      });
      try {
        expect(await openCardFile(root, 'vm.lead', 'media/image.png')).toEqual({
          ok: false,
          reason: 'not_found',
        });
      } finally {
        open.mockRestore();
      }
    });

    it('refuses the writer folder itself swapped for a link', async () => {
      const { root, writer, outside, target } = fixture();
      fs.mkdirSync(path.join(outside, 'media'));
      fs.writeFileSync(
        path.join(outside, 'media', 'image.png'),
        Buffer.concat([PNG_HEAD, Buffer.from('out')])
      );
      const open = swapAtOpen(target, () => {
        fs.renameSync(writer, `${writer}-original`);
        fs.symlinkSync(outside, writer);
      });
      try {
        expect((await openCardFile(root, 'vm.lead', 'media/image.png')).ok).toBe(false);
      } finally {
        open.mockRestore();
      }
    });

    it('still serves a plain file, and a link that stays inside the folder', async () => {
      const { root, writer, target } = fixture();
      fs.mkdirSync(path.join(writer, 'other'));
      fs.symlinkSync(target, path.join(writer, 'other', 'link.png'));
      for (const file of ['media/image.png', 'other/link.png']) {
        const opened = await openCardFile(root, 'vm.lead', file);
        expect([file, opened.ok]).toEqual([file, true]);
        if (opened.ok) await opened.handle.close();
      }
    });

    it('without /proc, the walk catches the swap and accepts the plain file', async () => {
      const { writer, media, outside, target } = fixture();
      const noProc = path.join(dirs.make(), 'no-such-dir');
      const ask = async (file: string) => {
        const handle = await fs.promises.open(file, 'r');
        try {
          return await openedFileIsUnder(handle, await handle.stat(), writer, target, noProc);
        } finally {
          await handle.close();
        }
      };
      expect(await ask(target)).toBe(true);
      expect(await ask(path.join(outside, 'image.png'))).toBe(false);
      const handle = await fs.promises.open(path.join(outside, 'image.png'), 'r');
      const stat = await handle.stat();
      fs.renameSync(media, `${media}-original`);
      fs.symlinkSync(outside, media);
      expect(await openedFileIsUnder(handle, stat, writer, target, noProc)).toBe(false);
      await handle.close();
    });

    // Node, as production runs it; the probe's swap, patched in the same way.
    it('under Node, outside bytes are never served', async () => {
      const { root, media, outside } = fixture();
      const bundle = dirs.make();
      const built = await Bun.build({
        entrypoints: [
          path.resolve(import.meta.dir, '../../../../src/web-server/status/status-files.ts'),
        ],
        target: 'node',
        format: 'esm',
        outdir: bundle,
        naming: 'status-files.mjs',
      });
      expect(built.success).toBe(true);
      const harness = path.join(bundle, 'harness.mjs');
      fs.writeFileSync(
        harness,
        `import fs from 'fs';
import path from 'path';
import { openCardFile } from './status-files.mjs';
const [root, media, outside] = process.argv.slice(2);
const open = fs.promises.open;
let swapped = false;
fs.promises.open = async function (file, ...args) {
  if (!swapped && file === path.join(media, 'image.png')) {
    swapped = true;
    fs.renameSync(media, media + '-original');
    fs.symlinkSync(outside, media);
  }
  return open.call(this, file, ...args);
};
const result = await openCardFile(root, 'vm.lead', 'media/image.png');
console.log(JSON.stringify({ swapped, ok: result.ok, reason: result.reason ?? null }));
process.exit(0);
`
      );
      const output = execFileSync('node', [harness, root, media, outside], {
        encoding: 'utf8',
        timeout: 15_000,
      });
      expect(JSON.parse(output)).toEqual({ swapped: true, ok: false, reason: 'not_found' });
    }, 25_000);
  });
});
