/**
 * One process of the batch-identity regressions (status-server-review.test.ts): the real clock, a real store
 * and a local doorbell stub. `<root> a <fault>` records one note with the send line's write or fsync failing,
 * then dies (SIGKILL) right after the batch file's rename, or, for `ring-loss`, after its first ring.
 * `legacy-ring` is `ring-loss` with the batch signed as an earlier version did: the same key, its `at` the send's end.
 * `<root> b [code]` is the restarted dashboard: it runs one retry pass and exits. With a code, the first open of a
 * batch file fails with it (an NFS `EIO`/`ESTALE`, or an `ENOENT` the share's cached view gives for a file it holds).
 */
import fs from 'node:fs';
import path from 'node:path';
import {
  loadOrCreateAnswerKey,
  signDocument,
} from '../../../../src/web-server/status/answer-signing';
import { StatusAnswers } from '../../../../src/web-server/status/answers';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import { NOW } from './status-test-helpers';

const [root, phase, arg] = process.argv.slice(2);
const fault = phase === 'a' ? (arg ?? 'send-write') : 'none';
const store = path.join(root, 'store');
const reports = path.join(root, 'reports');
const batches = path.join(reports, 'v1', 'answers', 'batches');
const journal = path.join(store, 'answers.jsonl');

if (phase === 'a') {
  const journalFds = new Set<number>();
  const lastData = new Map<number, string>();
  const realOpen = fs.openSync;
  fs.openSync = ((file: any, ...args: any[]) => {
    const fd = (realOpen as any)(file, ...args);
    if (String(file) === journal) journalFds.add(fd);
    return fd;
  }) as typeof fs.openSync;
  const realWrite = fs.writeSync;
  fs.writeSync = ((fd: any, data: any, ...args: any[]) => {
    const text = Buffer.from(data).toString();
    if (journalFds.has(fd)) lastData.set(fd, text);
    if (fault !== 'send-fsync' && journalFds.has(fd) && text.includes('"type":"send"'))
      throw Object.assign(Error('Injected send line write EIO'), { code: 'EIO' });
    return (realWrite as any)(fd, data, ...args);
  }) as typeof fs.writeSync;
  const realFsync = fs.fsyncSync;
  let fsyncFailed = false;
  fs.fsyncSync = ((fd: number) => {
    if (
      fault === 'send-fsync' &&
      !fsyncFailed &&
      journalFds.has(fd) &&
      lastData.get(fd)?.includes('"type":"send"')
    ) {
      fsyncFailed = true;
      throw Object.assign(Error('Injected send line fsync EIO'), { code: 'EIO' });
    }
    return realFsync(fd);
  }) as typeof fs.fsyncSync;
  const realRename = fs.renameSync;
  fs.renameSync = ((from: any, to: any) => {
    if (fault === 'legacy-ring' && path.dirname(String(to)) === batches) {
      const { signature: _drop, ...doc } = JSON.parse(fs.readFileSync(from, 'utf8'));
      const legacy = { ...doc, at: new Date(Date.parse(doc.at) + 1500).toISOString() };
      const text =
        JSON.stringify(signDocument(legacy, loadOrCreateAnswerKey(store)), null, 2) + '\n';
      fs.writeFileSync(from, text);
    }
    realRename(from, to);
    if (path.dirname(String(to)) !== batches) return;
    // ring-loss: the batch is published but its acknowledgement is lost, so its summary reads failed and the
    // ring still goes out; otherwise the process dies right after the publication.
    if (fault === 'ring-loss' || fault === 'legacy-ring')
      throw Object.assign(Error('Injected lost batch rename acknowledgement'), { code: 'EIO' });
    process.kill(process.pid, 'SIGKILL');
  }) as typeof fs.renameSync;
  // The answer's share write waits a little on its real open, so the real clock moves on between the
  // record and the end of the send.
  const realPromiseOpen = fs.promises.open;
  let waited = false;
  fs.promises.open = (async (...args: any[]) => {
    if (!waited && String(args[0]).includes('.tmp-')) {
      waited = true;
      await new Promise((resolve) => setTimeout(resolve, 120));
    }
    return (realPromiseOpen as any)(...args);
  }) as typeof fs.promises.open;
}

if (phase === 'b' && arg) {
  const realPromiseOpen = fs.promises.open;
  let failed = false;
  fs.promises.open = (async (...args: any[]) => {
    const file = String(args[0]);
    if (!failed && path.dirname(file) === batches && file.endsWith('.json')) {
      failed = true;
      throw Object.assign(Error('Injected batch read ' + arg), { code: arg });
    }
    return (realPromiseOpen as any)(...args);
  }) as typeof fs.promises.open;
}

const reader = new AgentReportsReader({ root: () => reports, now: () => NOW });
const answers = new StatusAnswers({
  storeDir: () => store,
  reportsRoot: () => reports,
  now: Date.now,
  readCard: (id) => reader.readCardNow(id),
  doorbell: async () => {
    const file = fs.readdirSync(batches).find((name) => name.endsWith('.json'));
    const batch = file ? JSON.parse(fs.readFileSync(path.join(batches, file), 'utf8')) : null;
    fs.appendFileSync(
      path.join(root, 'rings.jsonl'),
      JSON.stringify({
        phase,
        file: file ?? null,
        keyId: batch?.signature?.keyId ?? null,
        signature: batch?.signature?.value ?? null,
      }) + '\n'
    );
    if (phase === 'a') process.kill(process.pid, 'SIGKILL');
    return { ok: true, detail: 'Local stub rang.' };
  },
});

if (phase === 'a') {
  const parsed = await reader.readCardNow('vm.first--Q1');
  if (!parsed) throw Error('fixture card missing');
  await answers.submitSet(
    [
      {
        cardId: parsed.doc.id,
        cardSeq: parsed.doc.seq,
        cardSha256: parsed.sha256,
        choice: [],
        note: 'Please keep this note.',
        via: 'deck',
      },
    ],
    'Linux',
    'batch-identity-1234'
  );
  // The fault plan kills this process before here.
  process.exit(3);
} else {
  await answers.retryPending();
  process.exit(0);
}
