/** Max pair 2 M5: a single collector must publish a new retained machine snapshot. */
import { expect, it, spyOn } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { StatusService } from '../../../../src/web-server/status/status-service';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import { PmMailRegistryReader } from '../../../../src/web-server/status/status-collectors';
import { StatusAnswers } from '../../../../src/web-server/status/answers';
import { sameTo } from '../../../../src/web-server/status/status-feed';

it('invalidates a machine-only feed and preserves the previous CPU reading', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-machine-snapshot-'));
  const saved = { ...process.env };
  Object.assign(process.env, { CCS_HOME: root, CCS_DIR: path.join(root, '.ccs') });
  const homeSpy = spyOn(os, 'homedir').mockReturnValue(root);
  let blockedPrivateReads = 0;
  const reads = ['readFileSync', 'statSync', 'existsSync'].map((name) => {
    const method = name as 'readFileSync';
    const original = fs[method];
    return spyOn(fs, method).mockImplementation(((file: unknown, ...args: unknown[]) => {
      if (
        typeof file === 'string' &&
        /\/(?:\.ccs|\.claude|\.codex|\.t3)(?:\/|$)/.test(file) &&
        !path.resolve(file).startsWith(root + '/')
      ) {
        blockedPrivateReads++;
        throw new Error('private file outside fixture refused');
      }
      return (original as (...args: unknown[]) => unknown)(file, ...args);
    }) as never);
  });
  let busyPct = 12;
  let failed = false;
  const owner = new StatusService({
    reader: new AgentReportsReader({ root: () => root, now: () => 1790000000000 }),
    registryReader: new PmMailRegistryReader(() => root),
    historyStore: () => null,
    answers: new StatusAnswers({
      storeDir: () => path.join(root, 'answers'),
      reportsRoot: () => root,
      readCard: async () => null,
    }),
    spendNative: null,
    run: async () => ({ code: null, stdout: '', timedOut: false, missing: true }),
    localMachine: () => 'vm',
    readLocalMachine: async () => {
      if (failed) throw new Error('fixture unavailable');
      return {
        cores: 8,
        load1: busyPct === 12 ? 1.2 : 7.8,
        busyPct,
        memAvailableBytes: 1024 ** 3,
        memTotalBytes: 2 * 1024 ** 3,
        disk: null,
        cpuTimes: null,
      };
    },
    readRemoteMachine: async () => {
      throw new Error('fixture unavailable');
    },
    timeZone: () => 'UTC',
    now: () => 1790000000000,
    notify: () => undefined,
  });
  const collect = spyOn(owner, 'collect').mockResolvedValue(undefined);
  try {
    await owner.collectMachines();
    const before = owner.readInputs().machines;
    busyPct = 81;
    await owner.collectMachines();
    const after = owner.readInputs().machines;
    expect(blockedPrivateReads).toBe(0);
    expect(before?.vm.data?.busyPct).toBe(12);
    expect(after?.vm.data?.busyPct).toBe(81);
    expect(sameTo(before, after, 1)).toBe(false);
    failed = true;
    await owner.collectMachines();
    expect(sameTo(after, owner.readInputs().machines, 1)).toBe(false);
  } finally {
    collect.mockRestore();
    for (const read of reads) read.mockRestore();
    homeSpy.mockRestore();
    process.env = saved;
    fs.rmSync(root, { recursive: true, force: true });
  }
});
