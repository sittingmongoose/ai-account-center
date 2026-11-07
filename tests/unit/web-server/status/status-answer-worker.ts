import fs from 'node:fs';
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
    fs.appendFileSync(
      path.join(root, 'rings.jsonl'),
      JSON.stringify({ pid: process.pid, to }) + '\n'
    );
    return { ok: true, detail: 'Local stub rang.' };
  },
});
createInterface({ input: process.stdin }).on('line', async (line) => {
  const cmd = JSON.parse(line);
  try {
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
        entries: answers
          .list()
          .map((e: any) => ({
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
