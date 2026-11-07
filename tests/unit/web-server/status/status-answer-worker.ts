import fs from 'node:fs';
import express from 'express';
import { StatusService } from '../../../../src/web-server/status/status-service';
import { PmMailRegistryReader } from '../../../../src/web-server/status/status-collectors';
import { createStatusRouter } from '../../../../src/web-server/routes/status-routes';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { StatusAnswers } from '../../../../src/web-server/status/answers';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import { NOW } from './status-test-helpers';
const emit = (value: unknown) => process.stdout.write(JSON.stringify(value) + '\n');
const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const [root, mode] = process.argv.slice(2);
const store = path.join(root, 'store');
const reports = path.join(root, 'reports');
let faultCount = 0;
let faults = 0;
let faultCode = 'EIO';
let failCompletionAppends = false;
// One-shot journal faults: a close error after the real write, fsync and close succeeded, and a
// write error on the `type: "send"` line only. Only the journal's append descriptors are touched.
let closeFault: string | null = null;
let sendFault: string | null = null;
const journalFds = new Set<number>();
const realOpen = fs.openSync;
fs.openSync = ((file: any, ...args: any[]) => {
  const fd = (realOpen as any)(file, ...args);
  if (String(file) === path.join(store, 'answers.jsonl') && args[0] === 'a+') journalFds.add(fd);
  return fd;
}) as typeof fs.openSync;
const realClose = fs.closeSync;
fs.closeSync = ((fd: number) => {
  const journal = journalFds.delete(fd);
  realClose(fd);
  if (journal && closeFault) {
    const code = closeFault;
    closeFault = null;
    throw Object.assign(Error('Injected journal close error after write and fsync'), { code });
  }
}) as typeof fs.closeSync;
const realWrite = fs.writeSync;
fs.writeSync = ((fd: any, data: any, ...args: any[]) => {
  if (sendFault && journalFds.has(fd) && String(data).includes('\"type\":\"send\"')) {
    const code = sendFault;
    sendFault = null;
    throw Object.assign(Error('Injected send line write error'), { code });
  }
  if (
    failCompletionAppends &&
    String(data).includes('\"step\":\"doorbell\"') &&
    String(data).includes('\"state\":\"done\"')
  )
    throw Object.assign(Error('Injected completion append failure'), { code: faultCode });
  return (realWrite as any)(fd, data, ...args);
}) as typeof fs.writeSync;
// A slow failing fsync on the next `type: "answer"` line: the line is on disk, unvoided, until `<root>/fsync-go`
// appears; then the fsync throws and appendLine writes its void line. Only the journal's descriptors are touched.
let fsyncFault = false;
const lastJournalData = new Map<number, string>();
const realWriteSync = fs.writeSync;
fs.writeSync = ((fd: any, data: any, ...args: any[]) => {
  if (journalFds.has(fd)) lastJournalData.set(fd, Buffer.from(data).toString());
  return (realWriteSync as any)(fd, data, ...args);
}) as typeof fs.writeSync;
const realFsync = fs.fsyncSync;
fs.fsyncSync = ((fd: number) => {
  if (fsyncFault && journalFds.has(fd) && lastJournalData.get(fd)?.includes('"type":"answer"')) {
    fsyncFault = false;
    emit({ event: 'fsync-paused' });
    const until = Date.now() + 15000;
    while (!fs.existsSync(path.join(root, 'fsync-go')) && Date.now() < until) {
      /* spin: a slow fsync, so another process can read the unvoided line */
    }
    throw Object.assign(Error('Injected slow journal fsync error'), { code: 'EIO' });
  }
  return realFsync(fd);
}) as typeof fs.fsyncSync;
let ringResume: (() => void) | null = null;
let gateRing = false;
const realRead = fs.readFileSync;
fs.readFileSync = ((file: any, ...args: any[]) => {
  if (String(file) === path.join(store, 'answers.jsonl') && faults > 0) {
    faults--;
    faultCount++;
    throw Object.assign(Error('Injected journal EIO'), { code: faultCode });
  }
  return (realRead as any)(file, ...args);
}) as typeof fs.readFileSync;
let resume: (() => void) | null = null;
if (mode === 'load-eio') {
  const read = fs.readFileSync;
  fs.readFileSync = ((file: any, ...args: any[]) => {
    if (String(file) === path.join(store, 'answers.jsonl') && faultCount++ === 0)
      throw Object.assign(Error('One transient journal read EIO'), { code: 'EIO' });
    return (read as any)(file, ...args);
  }) as typeof fs.readFileSync;
}
if (mode === 'share-gap') {
  const open = fs.promises.open;
  let gated = false;
  fs.promises.open = (async (file: any, ...args: any[]) => {
    try {
      return await (open as any)(file, ...args);
    } catch (error: any) {
      if (
        !gated &&
        String(file) === path.join(reports, 'v1', 'answers', 'vm.first--Q1.json') &&
        error.code === 'ENOENT'
      ) {
        gated = true;
        await new Promise<void>((r) => {
          resume = r;
          emit({ event: 'share-wait', pid: process.pid });
        });
      }
      throw error;
    }
  }) as typeof fs.promises.open;
}
const reader = new AgentReportsReader({ root: () => reports, now: () => NOW });
const answers = new StatusAnswers({
  storeDir: () => store,
  reportsRoot: () => reports,
  now: () => NOW,
  readCard: (id) => reader.readCardNow(id),
  doorbell: async (to: string) => {
    if (gateRing) {
      gateRing = false;
      await new Promise<void>((resolve) => {
        ringResume = resolve;
        emit({ event: 'ring-wait' });
      });
    }
    fs.appendFileSync(
      path.join(root, 'rings.jsonl'),
      JSON.stringify({ pid: process.pid, to }) + '\n'
    );
    return { ok: true, detail: 'Local stub rang.' };
  },
});
const service = new StatusService({
  reader,
  answers,
  now: () => NOW,
  notify: () => undefined,
  timeZone: () => 'UTC',
  registryReader: new PmMailRegistryReader(() => path.join(root, 'registry')),
  run: async () => ({ code: null, stdout: '', timedOut: false, missing: true }),
  readLocalMachine: async () => {
    throw Error('offline fixture');
  },
  readRemoteMachine: async () => {
    throw Error('offline fixture');
  },
});
const app = express();
app.use((req, _res, next) => {
  Object.assign(req, { session: { authenticated: true } });
  next();
});
app.use(
  '/api/status',
  createStatusRouter(() => service)
);
const server = app.listen(0, '127.0.0.1');
await new Promise<void>((resolve) => server.once('listening', resolve));
await reader.scan();
createInterface({ input: process.stdin }).on('line', async (line) => {
  const cmd = JSON.parse(line);
  try {
    if (cmd.cmd === 'routes') {
      const base = 'http://127.0.0.1:' + (server.address() as { port: number }).port;
      const view = await fetch(base + '/api/status');
      const history = await fetch(base + '/api/status/answers');
      emit({
        event: 'routes',
        viewStatus: view.status,
        view: await view.json(),
        historyStatus: history.status,
        history: await history.json(),
      });
      return;
    }
    if (cmd.cmd === 'append-fault') {
      failCompletionAppends = cmd.fail;
      faultCode = cmd.code ?? 'EIO';
      emit({ event: 'armed' });
      return;
    }
    if (cmd.cmd === 'close-fault') {
      closeFault = cmd.code ?? 'EIO';
      emit({ event: 'armed' });
      return;
    }
    if (cmd.cmd === 'fsync-fault') {
      fsyncFault = true;
      emit({ event: 'armed' });
      return;
    }
    if (cmd.cmd === 'send-fault') {
      sendFault = cmd.code ?? 'EIO';
      emit({ event: 'armed' });
      return;
    }
    if (cmd.cmd === 'close') {
      answers.flushCompletions();
      emit({ event: 'closed' });
      return;
    }
    if (cmd.cmd === 'arm') {
      faults = cmd.n ?? 1;
      faultCode = cmd.code ?? 'EIO';
      emit({ event: 'armed' });
      return;
    }
    if (cmd.cmd === 'ring-gate') {
      gateRing = true;
      emit({ event: 'gated' });
      return;
    }
    if (cmd.cmd === 'release-ring') {
      ringResume?.();
      await pause(30);
      emit({ event: 'ring-released', faultCount });
      return;
    }
    if (cmd.cmd === 'exit') process.exit(0);
    if (cmd.cmd === 'release') {
      resume?.();
      return;
    }
    if (cmd.cmd === 'submit') {
      const result = await answers.submitSet(cmd.entries, 'Linux', cmd.clientSendId ?? null);
      await pause(30);
      emit({ event: 'submitted', result, faultCount, pid: process.pid });
    }
    if (cmd.cmd === 'retry') {
      await answers.retryPending();
      emit({ event: 'retried' });
    }
    if (cmd.cmd === 'history')
      emit({
        event: 'history',
        entries: answers.list().map((e: any) => ({
          seq: e.answer.cardSeq,
          kind: e.answer.kind,
          share: e.share.state,
          doorbell: e.doorbell.state,
        })),
      });
  } catch (error) {
    emit({ event: 'error', error: String(error) });
  }
});
emit({ event: 'ready', pid: process.pid });
