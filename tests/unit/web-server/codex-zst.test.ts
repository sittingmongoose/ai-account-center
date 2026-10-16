import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { createHash } from 'crypto';
import { zstdCompressSync } from 'zlib';
import { codexZstdChunks } from '../../../src/web-server/usage/codex-rollout-files';
import { runWithScopedCcsHome } from '../../../src/utils/config-manager';
import { classifyCompressedCodexRollout } from '../../../src/web-server/usage/experiment-usage-signatures';
import {
  collectAccountActivity,
  collectCodexPartition,
  readLargeCodexCheckpoint,
  type AccountActivityScanOptions,
} from '../../../src/web-server/usage/account-activity-collector';
import {
  LargeCodexReadQueue,
  spawnLargeCodexReader,
  type LargeCodexReadSpec,
} from '../../../src/web-server/usage/worker-client';
import {
  runExperimentRootsSlice,
  readExperimentRoots,
} from '../../../src/web-server/usage/experiment-usage-roots';

const NOW = Date.parse('2026-10-10T16:00:00Z');
const UUID = '01234567-1234-1234-1234-0123456789ab';
const NAME = `rollout-2026-10-01T15-00-00-${UUID}.jsonl`;
const record = (n: number) => ({
  timestamp: new Date(Date.parse('2026-10-01T15:05:00Z') + n * 1000).toISOString(),
  type: 'event_msg',
  payload: {
    type: 'token_count',
    info: {
      total_token_usage: {
        input_tokens: n * 100,
        cached_input_tokens: n * 20,
        output_tokens: n * 10,
      },
    },
  },
});
function rollout(count = 3): string {
  return (
    [
      {
        type: 'session_meta',
        payload: { id: UUID, originator: 'codex_cli_rs', cli_version: '0.162.0' },
      },
      { type: 'turn_context', payload: { model: 'gpt-5.4' } },
      record(1),
      record(1),
      ...Array.from({ length: count - 1 }, (_, i) => record(i + 2)),
    ]
      .map((line) => JSON.stringify(line))
      .join('\n') + '\n'
  );
}

describe('Codex compressed rollouts', () => {
  let root: string;
  let file: string;
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-codex-zst-'));
    file = path.join(root, 'codex', 'sessions', NAME);
    fs.mkdirSync(path.dirname(file), { recursive: true });
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));
  const collect = (options: Partial<AccountActivityScanOptions> = {}) =>
    runWithScopedCcsHome(root, () =>
      collectAccountActivity(
        { kind: 'codex', codexHome: path.join(root, 'codex'), cacheDir: path.join(root, 'cache') },
        { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache'), ...options }
      )
    );
  const compress = (body = rollout()) =>
    fs.writeFileSync(`${file}.zst`, zstdCompressSync(Buffer.from(body)));
  const checkpointPath = (target = `${file}.zst`) => {
    const directory = fs.readdirSync(path.join(root, 'cache', 'account-activity-v1'))[0];
    return path.join(
      root,
      'cache',
      'account-activity-v1',
      directory,
      `${createHash('sha256').update(target).digest('hex')}.json`
    );
  };
  const checkpoint = (target = `${file}.zst`) =>
    JSON.parse(fs.readFileSync(checkpointPath(target), 'utf8'));
  const updateCheckpoint = (target: string, changes: Record<string, unknown>) =>
    fs.writeFileSync(checkpointPath(target), JSON.stringify({ ...checkpoint(target), ...changes }));
  // Charge only decoder work, so traversal speed and host load cannot decide the outcome.
  const cutReplay = async <T>(run: () => Promise<T>, ticks = 4, starts?: number[]): Promise<T> => {
    let decoding = false;
    let calls = 0;
    const stream = fs.createReadStream;
    const reads = spyOn(fs, 'createReadStream').mockImplementation((target, options) => {
      if (String(target).endsWith('.zst')) {
        decoding = true;
        starts?.push((options as { start: number }).start);
      }
      return stream(target, options);
    });
    const clock = spyOn(Date, 'now').mockImplementation(() =>
      decoding && ++calls > ticks ? NOW + 2000 : NOW
    );
    try {
      return await run();
    } finally {
      clock.mockRestore();
      reads.mockRestore();
    }
  };

  const waitFor = async (ready: () => boolean) => {
    const until = performance.now() + 10000;
    while (!ready() && performance.now() < until)
      await new Promise((resolve) => setTimeout(resolve, 5));
    expect(ready()).toBe(true);
  };
  const pauseLargeFile = async () => {
    await collect({ maxBytesPerFile: 2 * 1024 * 1024 });
    let paused: Awaited<ReturnType<typeof collect>> | undefined;
    for (let attempt = 0; attempt < 3; attempt++) {
      for (let skipped = checkpoint().compressedReplay?.skipPasses ?? 0; skipped > 0; skipped--)
        await collect({ budgetMs: 1000 });
      paused = await cutReplay(() => collect({ budgetMs: 1000 }));
    }
    expect(checkpoint().compressedReplay.zeroProgress).toBe(3);
    expect(paused?.scan?.largeCodexReads).toHaveLength(1);
    return paused!.scan!.largeCodexReads![0];
  };

  it('completes a paused single-frame file in a background worker, exactly matching plain usage', async () => {
    const body = rollout(12000);
    fs.writeFileSync(file, body);
    const plain = await collect({ cacheDir: path.join(root, 'plain-background') });
    compress(body);
    fs.unlinkSync(file);
    const spec = await pauseLargeFile();
    const paused = fs.readFileSync(spec.checkpoint, 'utf8');
    const queue = new LargeCodexReadQueue();
    try {
      queue.enqueue([spec]);
      await waitFor(() => fs.existsSync(`${spec.checkpoint}.large-complete`));
      // Full success is handed off, never published over a running normal pass.
      expect(fs.readFileSync(spec.checkpoint, 'utf8')).toBe(paused);
      const finished = await collect();
      expect(finished.scan?.complete).toBe(true);
      expect(finished.scan?.detail).toBeUndefined();
      expect(finished.hourly).toEqual(plain.hourly);
      expect(finished.session).toEqual(plain.session);
      expect(finished.eventCount).toBe(plain.eventCount);
      const warm = await collect();
      expect(warm.scan?.readBytes).toBe(0);
      expect(warm.eventCount).toBe(12000);
    } finally {
      await queue.shutdown();
    }
  });

  it('keeps the paused checkpoint and detail after a background timeout, including across queue restarts', async () => {
    compress(rollout(12000));
    const spec = await pauseLargeFile();
    const before = fs.readFileSync(spec.checkpoint, 'utf8');
    // A 1ms outer bound includes worker startup, so no timing race with decoder throughput.
    expect(await spawnLargeCodexReader(spec, new AbortController().signal, 1)).toBeUndefined();
    let started = false;
    const queue = new LargeCodexReadQueue((...args) => {
      started = true;
      return spawnLargeCodexReader(...args);
    }, 1);
    try {
      queue.enqueue([spec]);
      await waitFor(() => started && !queue.isRunning([spec]));
      expect(fs.readFileSync(spec.checkpoint, 'utf8')).toBe(before);
      expect(fs.existsSync(`${spec.checkpoint}.large-complete`)).toBe(false);
      const clock = spyOn(Date, 'now').mockReturnValue(NOW);
      try {
        expect((await collect()).scan?.detail).toContain('earlier usage is kept');
      } finally {
        clock.mockRestore();
      }
    } finally {
      await queue.shutdown();
    }
    let calls = 0;
    const restarted = new LargeCodexReadQueue(async () => {
      calls++;
      return undefined;
    });
    try {
      restarted.enqueue([spec]);
      // Let the persisted retry marker be read before stopping the queue.
      await new Promise((resolve) => setTimeout(resolve, 30));
      expect(calls).toBe(0);
    } finally {
      await restarted.shutdown();
    }
  });

  it.each(['size', 'mtime', 'identity'] as const)(
    'discards a background result when the file changes (%s)',
    async (change) => {
      compress(rollout(12000));
      const spec = await pauseLargeFile();
      const before = fs.readFileSync(spec.checkpoint, 'utf8');
      const stream = fs.createReadStream;
      let changed = false;
      const reads = spyOn(fs, 'createReadStream').mockImplementation((target, options) => {
        if (String(target) === spec.file && !changed) {
          changed = true;
          if (change === 'size') fs.appendFileSync(spec.file, zstdCompressSync(Buffer.from('\n')));
          else if (change === 'mtime') {
            const time = new Date(fs.statSync(spec.file).mtimeMs + 5000);
            fs.utimesSync(spec.file, time, time);
          } else {
            fs.renameSync(spec.file, `${spec.file}.old`);
            fs.copyFileSync(`${spec.file}.old`, spec.file);
          }
        }
        return stream(target, options);
      });
      try {
        expect(await readLargeCodexCheckpoint(spec, Date.now() + 10000)).toBeUndefined();
        expect(changed).toBe(true);
        expect(fs.readFileSync(spec.checkpoint, 'utf8')).toBe(before);
      } finally {
        reads.mockRestore();
      }
    }
  );

  it('runs one background read at a time, coalesces duplicate offers, and cancels on shutdown', async () => {
    compress(rollout(12000));
    const first = await pauseLargeFile();
    const secondFile = first.file.replace(UUID, 'abcdef01-1234-1234-1234-0123456789ab');
    fs.copyFileSync(first.file, secondFile);
    const { codexFileRevision } = await import('../../../src/web-server/usage/worker-client');
    const second: LargeCodexReadSpec = {
      ...first,
      file: secondFile,
      checkpoint: `${first.checkpoint}-other`,
      revision: codexFileRevision(fs.statSync(secondFile)),
    };
    const starts: string[] = [];
    let release: (() => void) | undefined;
    let aborted = false;
    const queue = new LargeCodexReadQueue(async (spec, signal) => {
      starts.push(spec.file);
      await new Promise<void>((resolve) => {
        release = resolve;
        signal.addEventListener(
          'abort',
          () => {
            aborted = true;
            resolve();
          },
          { once: true }
        );
      });
      return undefined;
    });
    try {
      queue.enqueue([first, second, first]);
      await waitFor(() => starts.length === 1);
      queue.enqueue([first, second]);
      expect(starts).toEqual([first.file]);
      release!();
      await waitFor(() => starts.length === 2);
      expect(starts).toEqual([first.file, second.file]);
      await queue.shutdown();
      expect(aborted).toBe(true);
      expect(queue.isRunning([first, second])).toBe(false);
      queue.enqueue([first]);
      expect(starts).toHaveLength(2);
    } finally {
      await queue.shutdown();
    }
  });

  it('terminates a real background worker when cancelled', async () => {
    compress(rollout(12000));
    const spec = await pauseLargeFile();
    const before = fs.readFileSync(spec.checkpoint, 'utf8');
    const controller = new AbortController();
    const reading = spawnLargeCodexReader(spec, controller.signal);
    controller.abort();
    expect(await reading).toBeUndefined();
    expect(fs.readFileSync(spec.checkpoint, 'utf8')).toBe(before);
    expect(fs.existsSync(`${spec.checkpoint}.large-complete`)).toBe(false);
  });

  it('rejects a completed handoff if a normal pass has changed the checkpoint', async () => {
    compress(rollout(12000));
    const spec = await pauseLargeFile();
    const queue = new LargeCodexReadQueue();
    try {
      queue.enqueue([spec]);
      await waitFor(() => fs.existsSync(`${spec.checkpoint}.large-complete`));
      const replay = checkpoint().compressedReplay;
      updateCheckpoint(spec.file, {
        compressedReplay: { ...replay, lastAttempt: replay.lastAttempt + 1 },
      });
      const before = fs.readFileSync(spec.checkpoint, 'utf8');
      const clock = spyOn(Date, 'now').mockReturnValue(NOW);
      try {
        const result = await collect();
        expect(result.scan?.complete).toBe(false);
        expect(result.eventCount).toBeLessThan(12000);
      } finally {
        clock.mockRestore();
      }
      expect(fs.readFileSync(spec.checkpoint, 'utf8')).toBe(before);
      expect(fs.existsSync(`${spec.checkpoint}.large-complete`)).toBe(false);
    } finally {
      await queue.shutdown();
    }
  });

  it.each([
    ['single', 0],
    ['single', 13],
    ['multi', 0],
    ['multi', 13],
  ] as const)(
    'keeps a cap inside the final decoded chunk incomplete (%s frame, %i tail bytes)',
    async (framing, tailBytes) => {
      const prefix = rollout(1);
      const second = JSON.stringify(record(2)) + '\n';
      const third = JSON.stringify(record(3)) + '\n';
      const body = prefix + second + third;
      fs.writeFileSync(file, body);
      const plain = await collect({ cacheDir: path.join(root, 'plain-eof-cap') });
      fs.unlinkSync(file);
      const frames = framing === 'single' ? [body] : [prefix, second + third];
      const encoded = frames.map((frame) => zstdCompressSync(Buffer.from(frame)));
      fs.writeFileSync(`${file}.zst`, Buffer.concat(encoded));

      const consumed = Buffer.byteLength(prefix + second);
      const partial = await collect({ maxBytesPerFile: consumed + tailBytes });
      expect(partial.scan?.complete).toBe(false);
      expect(partial.eventCount).toBe(2);
      expect(checkpoint()).toMatchObject({ complete: false, offset: 0, decodedOffset: consumed });
      expect(checkpoint().compressedFrame?.compressed ?? 0).toBe(
        framing === 'single' ? 0 : encoded[0].length
      );
      const finished = await collect();
      expect(finished.scan?.complete).toBe(true);
      expect(finished.eventCount).toBe(3);
      expect(finished.hourly).toEqual(plain.hourly);
      expect(finished.session).toEqual(plain.session);
      expect(checkpoint().offset).toBe(fs.statSync(`${file}.zst`).size);
      expect((await collect()).scan?.readBytes).toBe(0);
    }
  );

  it.each(['single', 'multi'] as const)(
    'resumes a capped final chunk without losing the 200 -> 300 cumulative snapshot (%s frame)',
    async (framing) => {
      const prefix = rollout(2);
      const padding = (
        JSON.stringify({ type: 'response_item', payload: 'x'.repeat(1024) }) + '\n'
      ).repeat(128);
      const suffix = JSON.stringify(record(3)) + '\n';
      const body = prefix + padding + suffix;
      fs.writeFileSync(file, body);
      const plain = await collect({ cacheDir: path.join(root, 'plain-resumed-eof-cap') });
      fs.unlinkSync(file);
      const frames = framing === 'single' ? [body] : [prefix, padding + suffix];
      const encoded = frames.map((frame) => zstdCompressSync(Buffer.from(frame)));
      fs.writeFileSync(`${file}.zst`, Buffer.concat(encoded));

      const first = await collect({ maxBytesPerFile: Buffer.byteLength(prefix) });
      expect(first.scan?.complete).toBe(false);
      expect(first.eventCount).toBe(2);
      const resumed = await collect({ maxBytesPerFile: Buffer.byteLength(padding) });
      expect(resumed.scan?.complete).toBe(false);
      expect(resumed.hourly).toEqual(first.hourly);
      expect(resumed.eventCount).toBe(2);
      expect(checkpoint()).toMatchObject({
        complete: false,
        offset: 0,
        decodedOffset: Buffer.byteLength(prefix + padding),
      });
      expect(checkpoint().compressedFrame?.compressed ?? 0).toBe(
        framing === 'single' ? 0 : encoded[0].length
      );
      const finished = await collect();
      expect(finished.scan?.complete).toBe(true);
      expect(finished.hourly).toEqual(plain.hourly);
      expect(finished.session).toEqual(plain.session);
      expect(finished.eventCount).toBe(3);
    }
  );

  it.each([
    ['single', true],
    ['single', false],
    ['multi', true],
    ['multi', false],
  ] as const)(
    'completes only after an exact EOF cap is consumed (%s frame, newline %s)',
    async (framing, newline) => {
      const prefix = rollout(2);
      const suffix = JSON.stringify(record(3)) + (newline ? '\n' : '');
      const body = prefix + suffix;
      const frames = framing === 'single' ? [body] : [prefix, suffix];
      fs.writeFileSync(
        `${file}.zst`,
        Buffer.concat(frames.map((frame) => zstdCompressSync(Buffer.from(frame))))
      );
      const result = await collect({ maxBytesPerFile: Buffer.byteLength(body) });
      expect(result.scan?.complete).toBe(true);
      expect(result.eventCount).toBe(3);
      expect(checkpoint()).toMatchObject({
        complete: true,
        offset: fs.statSync(`${file}.zst`).size,
        decodedOffset: Buffer.byteLength(body),
      });
      expect((await collect()).scan?.readBytes).toBe(0);
    }
  );

  it('rotates two stalled files so the recoverable sibling finishes before another replay', async () => {
    compress(rollout(12000));
    await collect({ maxBytesPerFile: 128 * 1024 });
    const other = `${file.replace(UUID, 'abcdef01-1234-1234-1234-0123456789ab')}.zst`;
    fs.writeFileSync(other, zstdCompressSync(Buffer.from(rollout().replaceAll(UUID, 'other'))));
    await collect({ maxBytesPerFile: 300 });
    for (const [target, lastAttempt] of [
      [`${file}.zst`, 1],
      [other, 2],
    ] as const) {
      updateCheckpoint(target, {
        compressedReplayStalled: true,
        compressedReplay: { lastAttempt, zeroProgress: 0, skipPasses: 0, terminalUntil: 0 },
      });
    }
    const smallerOffset = checkpoint(other).decodedOffset;
    const first = await cutReplay(() => collect({ budgetMs: 1000 }));
    expect(first.scan?.complete).toBe(false);
    expect(checkpoint(other).decodedOffset).toBe(smallerOffset);
    expect(checkpoint().compressedReplay.zeroProgress).toBe(1);
    // Disable just the back-off to prove least-recently-attempted order on its own.
    updateCheckpoint(`${file}.zst`, {
      compressedReplay: { ...checkpoint().compressedReplay, skipPasses: 0 },
    });
    await cutReplay(() => collect({ budgetMs: 1000 }));
    expect(checkpoint(other).complete).toBe(true);
    expect(
      checkpoint(other).rows.reduce((n: number, row: { events: number }) => n + row.events, 0)
    ).toBe(3);
    expect(checkpoint().complete).toBe(false);
  });

  it.each(['inline', 'partition'] as const)(
    'backs off zero-progress replays and reaches an honest terminal state (%s)',
    async (reader) => {
      compress(rollout(12000));
      const earlier = await collect({ maxBytesPerFile: 128 * 1024 });
      const initial = checkpoint();
      const pass = async () => {
        if (reader === 'inline') return collect({ budgetMs: 1000 });
        const outcomes = await collectCodexPartition(
          {
            files: [`${file}.zst`],
            checkpointDir: path.dirname(checkpointPath()),
            deadline: NOW + 1000,
          },
          { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache') }
        );
        expect(outcomes[0].readBytes).toBe(0);
        return undefined;
      };
      await cutReplay(pass);
      expect(checkpoint().compressedReplay).toMatchObject({ zeroProgress: 1, skipPasses: 1 });
      const stream = fs.createReadStream;
      let opens = 0;
      const reads = spyOn(fs, 'createReadStream').mockImplementation((target, options) => {
        opens++;
        return stream(target, options);
      });
      try {
        await pass();
        expect(opens).toBe(0);
      } finally {
        reads.mockRestore();
      }
      expect(checkpoint().compressedReplay.skipPasses).toBe(0);
      await cutReplay(pass);
      expect(checkpoint().compressedReplay).toMatchObject({ zeroProgress: 2, skipPasses: 3 });
      for (let skipped = 0; skipped < 3; skipped++) await pass();
      await cutReplay(pass);
      expect(checkpoint().compressedReplay.zeroProgress).toBe(3);
      expect(checkpoint().compressedReplay.terminalUntil).toBe(NOW + 86400000);
      const stable = spyOn(Date, 'now').mockReturnValue(NOW);
      let terminal: Awaited<ReturnType<typeof collect>>;
      try {
        terminal = await collect();
        expect((await collect()).scan?.readBytes).toBe(0);
      } finally {
        stable.mockRestore();
      }
      expect(terminal.scan).toMatchObject({ complete: false, readBytes: 0 });
      expect(terminal.scan?.detail).toContain('1 compressed Codex log is too large to read here');
      expect(terminal.scan?.detail).toContain('earlier usage is kept');
      expect(terminal.scan?.detail).toContain('measurement remains partial');
      expect(terminal.hourly).toEqual(earlier.hourly);
      expect(terminal.eventCount).toBe(earlier.eventCount);
      expect(checkpoint().decodedOffset).toBe(initial.decodedOffset);
      const clock = spyOn(Date, 'now').mockReturnValue(NOW + 86400001);
      try {
        expect((await collect()).scan?.complete).toBe(true);
      } finally {
        clock.mockRestore();
      }
    }
  );

  it.each(['size', 'mtime', 'identity'] as const)(
    're-enables a terminal file on %s change',
    async (change) => {
      compress(rollout(12000));
      await collect({ maxBytesPerFile: 128 * 1024 });
      updateCheckpoint(`${file}.zst`, {
        compressedReplayStalled: true,
        compressedReplay: {
          lastAttempt: NOW,
          zeroProgress: 3,
          skipPasses: 0,
          terminalUntil: Date.now() + 86400000,
        },
      });
      if (change === 'size') {
        fs.appendFileSync(
          `${file}.zst`,
          zstdCompressSync(Buffer.from(JSON.stringify(record(12001)) + '\n'))
        );
      } else if (change === 'mtime') {
        const changed = new Date(fs.statSync(`${file}.zst`).mtimeMs + 5000);
        fs.utimesSync(`${file}.zst`, changed, changed);
      } else {
        fs.renameSync(`${file}.zst`, `${file}.old`);
        fs.copyFileSync(`${file}.old`, `${file}.zst`);
      }
      const result = await collect();
      expect(result.scan?.complete).toBe(true);
      expect(result.scan?.detail).toBeUndefined();
      expect(result.eventCount).toBe(change === 'size' ? 12001 : 12000);
    }
  );

  it.each(['bytes', 'deadline'] as const)(
    'resumes independent frames without prefix replay, including split records (%s)',
    async (budget) => {
      const body = rollout(4000);
      fs.writeFileSync(file, body);
      const plain = await collect({ cacheDir: path.join(root, 'plain-frames') });
      fs.unlinkSync(file);
      const bytes = Buffer.from(body);
      const frameBytes = 32 * 1024;
      const frames = Array.from({ length: Math.ceil(bytes.length / frameBytes) }, (_, i) =>
        zstdCompressSync(bytes.subarray(i * frameBytes, (i + 1) * frameBytes))
      );
      fs.writeFileSync(`${file}.zst`, Buffer.concat(frames));
      const starts: number[] = [];
      const pass = () =>
        budget === 'bytes'
          ? collect({ maxBytesPerFile: frameBytes * 2 })
          : cutReplay(() => collect({ budgetMs: 1000 }), 24, starts);
      let result = await pass();
      expect(result.scan?.complete).toBe(false);
      expect(checkpoint().compressedFrame.compressed).toBeGreaterThan(0);
      let expectedStart = checkpoint().compressedFrame.compressed;
      const stream = fs.createReadStream;
      const reads =
        budget === 'bytes'
          ? spyOn(fs, 'createReadStream').mockImplementation((target, options) => {
              if (String(target).endsWith('.zst'))
                starts.push((options as { start: number }).start);
              return stream(target, options);
            })
          : undefined;
      try {
        for (let attempt = 0; attempt < 30 && !result.scan?.complete; attempt++) {
          starts.length = 0;
          result = await pass();
          expect(starts[0]).toBe(expectedStart);
          expect(starts[0]).toBeGreaterThan(0);
          const next = checkpoint().compressedFrame.compressed;
          expect(next).toBeGreaterThan(expectedStart);
          expectedStart = next;
        }
      } finally {
        reads?.mockRestore();
      }
      expect(result.scan?.complete).toBe(true);
      expect(result.hourly).toEqual(plain.hourly);
      expect(result.session).toEqual(plain.session);
      expect(result.eventCount).toBe(plain.eventCount);
    }
  );

  it('walks skippable frames and rejects truncated framing without accepting false boundaries', async () => {
    const metadata = Buffer.alloc(12);
    metadata.writeUInt32LE(0x184d2a50);
    metadata.writeUInt32LE(4, 4);
    fs.writeFileSync(
      `${file}.zst`,
      Buffer.concat([metadata, zstdCompressSync(Buffer.from(rollout()))])
    );
    expect((await collect()).eventCount).toBe(3);
    fs.truncateSync(`${file}.zst`, fs.statSync(`${file}.zst`).size - 2);
    let rejected = false;
    try {
      for await (const _chunk of codexZstdChunks(`${file}.zst`, { onFrame: () => {} })) {
        /* consume */
      }
    } catch {
      rejected = true;
    }
    expect(rejected).toBe(true);
  });

  it('matches the uncompressed cumulative deltas, including duplicate token snapshots', async () => {
    fs.writeFileSync(file, rollout());
    const plain = await collect({ cacheDir: path.join(root, 'plain-cache') });
    compress();
    fs.unlinkSync(file);
    const compressed = await collect();
    expect(compressed.scan?.complete).toBe(true);
    expect(compressed.eventCount).toBe(3);
    expect(compressed.hourly).toEqual(plain.hourly);
    expect(compressed.session).toEqual(plain.session);
    const warm = await collect();
    expect(warm.scan?.readBytes).toBe(0);
    expect(warm.hourly).toEqual(plain.hourly);
  });

  it('prefers the growing plain file when both forms name the same UUID', async () => {
    compress(rollout(1));
    // Different timestamps still name one conversation; suffix stripping alone is insufficient.
    fs.writeFileSync(file.replace('15-00-00', '16-00-00'), rollout(3));
    const data = await collect();
    expect(data.scan?.totalFiles).toBe(1);
    expect(data.eventCount).toBe(3);
    expect(data.hourly[0].inputTokens).toBe(240);
  });

  it('keeps the completed result while a plain-to-zstd replacement catches up', async () => {
    fs.writeFileSync(file, rollout());
    const plain = await collect();
    compress();
    fs.unlinkSync(file);
    const partial = await collect({ maxBytesPerFile: 200 });
    expect(partial.scan?.complete).toBe(false);
    expect(partial.hourly).toEqual(plain.hourly);
    expect(partial.eventCount).toBe(plain.eventCount);
    const finished = await collect();
    expect(finished.scan?.complete).toBe(true);
    expect(finished.hourly).toEqual(plain.hourly);
    expect(finished.session).toEqual(plain.session);
    expect(finished.eventCount).toBe(plain.eventCount);
  });

  it('checkpoints a deadline cut as partial and resumes without duplicate deltas', async () => {
    compress(rollout(4000));
    let calls = 0;
    const clock = spyOn(Date, 'now').mockImplementation(() => (++calls > 50 ? NOW + 2000 : NOW));
    let partial: Awaited<ReturnType<typeof collect>>;
    try {
      partial = await collect({ budgetMs: 1000 });
    } finally {
      clock.mockRestore();
    }
    expect(partial.scan?.complete).toBe(false);
    expect(partial.scan?.readBytes).toBeGreaterThan(0);
    expect(partial.eventCount).toBeGreaterThan(0);
    expect(partial.eventCount).toBeLessThan(4000);
    const finished = await collect();
    expect(finished.scan?.complete).toBe(true);
    expect(finished.eventCount).toBe(4000);
    expect(finished.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(320000);
    expect(finished.hourly.reduce((sum, hour) => sum + hour.outputTokens, 0)).toBe(40000);
    expect((await collect()).scan?.readBytes).toBe(0);
  });

  it.each(['inline', 'partition'] as const)(
    'keeps plain checkpoint rows when the compressed replacement fails while a sibling grows (%s)',
    async (reader) => {
      fs.writeFileSync(file, rollout());
      const sibling = file.replace(UUID, 'abcdef01-1234-1234-1234-0123456789ab');
      fs.writeFileSync(sibling, rollout(1).replaceAll(UUID, 'sibling'));
      expect((await collect()).hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(320);
      compress();
      fs.unlinkSync(file);
      fs.appendFileSync(sibling, JSON.stringify(record(2)) + '\n');
      const open = fs.openSync;
      const blocked = spyOn(fs, 'openSync').mockImplementation(((target, ...args) => {
        if (target === `${file}.zst`) throw new Error('fixture unreadable compressed replacement');
        return open(target, ...args);
      }) as typeof fs.openSync);
      const stream = fs.createReadStream;
      const blockedStream = spyOn(fs, 'createReadStream').mockImplementation((target, options) => {
        if (String(target) === `${file}.zst`)
          throw new Error('fixture unreadable compressed replacement');
        return stream(target, options);
      });
      try {
        for (let pass = 0; pass < 2; pass++) {
          if (reader === 'inline') {
            const result = await collect();
            expect(result.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(400);
            expect(result.scan).toMatchObject({ complete: false, failedFiles: 1 });
          } else {
            const directory = fs.readdirSync(path.join(root, 'cache', 'account-activity-v1'))[0];
            const outcomes = await collectCodexPartition(
              {
                files: [`${file}.zst`, sibling],
                checkpointDir: path.join(root, 'cache', 'account-activity-v1', directory),
                deadline: Date.now() + 12_000,
              },
              { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache') }
            );
            expect(outcomes[0]).toMatchObject({ failed: true, complete: false });
            expect(
              outcomes
                .flatMap((outcome) => outcome.rows)
                .reduce((sum, row) => sum + row.entry.inputTokens, 0)
            ).toBe(400);
          }
        }
      } finally {
        blocked.mockRestore();
        blockedStream.mockRestore();
      }
      const recovered = await collect();
      expect(recovered.scan?.complete).toBe(true);
      expect(recovered.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(400);
    }
  );

  it('converges on a large single-frame decoded rollout over bounded byte-capped passes', async () => {
    const padding = JSON.stringify({ type: 'response_item', payload: 'x'.repeat(8192) }) + '\n';
    const body = rollout(1) + padding.repeat(4096) + JSON.stringify(record(3)) + '\n';
    fs.writeFileSync(file, body);
    const plain = await collect({ cacheDir: path.join(root, 'plain-large-cache') });
    compress(body);
    fs.unlinkSync(file);
    expect(fs.statSync(`${file}.zst`).size).toBeLessThan(64 * 1024);
    let data: Awaited<ReturnType<typeof collect>> | undefined;
    let passes = 0;
    for (; passes < 10; passes++) {
      data = await collect({ maxBytesPerFile: 4 * 1024 * 1024 });
      expect(data.scan?.detail).toBeUndefined();
      expect(data.scan?.readBytes).toBeGreaterThan(0);
      if (data.scan?.complete) break;
    }
    expect(passes).toBeLessThan(10);
    expect(data?.scan?.complete).toBe(true);
    expect(data?.hourly).toEqual(plain.hourly);
    expect(data?.session).toEqual(plain.session);
    expect(data?.eventCount).toBe(2);
    expect((await collect()).scan?.readBytes).toBe(0);
  });

  it('reports a replay-only deadline cut and reads fresh siblings before retrying a stalled file', async () => {
    compress(rollout(12000));
    const partial = await collect({ maxBytesPerFile: 2 * 1024 * 1024 });
    expect(partial.scan?.complete).toBe(false);
    expect(partial.scan?.readBytes).toBeGreaterThan(1024 * 1024);
    for (let attempt = 0; attempt < 2; attempt++) {
      let calls = 0;
      const clock = spyOn(Date, 'now').mockImplementation(() => (++calls > 50 ? NOW + 2000 : NOW));
      let stalled: Awaited<ReturnType<typeof collect>>;
      try {
        stalled = await collect({ budgetMs: 1000 });
      } finally {
        clock.mockRestore();
      }
      expect(stalled.scan).toMatchObject({ complete: false, readBytes: 0 });
      expect(stalled.scan?.detail).toContain('1 compressed Codex log is too large to finish');
      expect(stalled.scan?.detail).toContain('decoded prefix exhausted the read budget');
      expect(stalled.hourly).toEqual(partial.hourly);
      expect(stalled.eventCount).toBe(partial.eventCount);
    }
    const other = `${file.replace(UUID, 'abcdef01-1234-1234-1234-0123456789ab')}.zst`;
    fs.writeFileSync(other, zstdCompressSync(Buffer.from(rollout().replaceAll(UUID, 'other'))));
    fs.utimesSync(other, new Date(Date.now() + 1000), new Date(Date.now() + 1000));
    const opened: string[] = [];
    const createReadStream = fs.createReadStream;
    const reads = spyOn(fs, 'createReadStream').mockImplementation((filename, options) => {
      opened.push(String(filename));
      return createReadStream(filename, options);
    });
    let calls = 0;
    const clock = spyOn(Date, 'now').mockImplementation(() => (++calls > 50 ? NOW + 2000 : NOW));
    try {
      const withSibling = await collect({ budgetMs: 1000 });
      expect(opened[0]).toBe(other);
      expect(withSibling.scan?.complete).toBe(false);
      expect(withSibling.scan?.detail).toContain('1 compressed Codex log is too large to finish');
      expect(withSibling.eventCount).toBe(partial.eventCount! + 3);
      expect(withSibling.scan?.readBytes).toBe(
        Buffer.byteLength(rollout().replaceAll(UUID, 'other'))
      );
    } finally {
      clock.mockRestore();
    }
    let finished = partial;
    try {
      for (let pass = 0; pass < 6; pass++) {
        finished = await collect();
        if (finished.scan?.complete) break;
      }
    } finally {
      reads.mockRestore();
    }
    expect(opened[0]).toBe(other);
    expect(finished.scan?.complete).toBe(true);
    expect(finished.scan?.detail).toBeUndefined();
    expect(finished.eventCount).toBe(12003);
    expect(finished.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(960240);
  });

  it('applies the mtime margin before opening a decoder', async () => {
    fs.writeFileSync(`${file}.zst`, 'invalid zstd bytes');
    const cutoff = NOW - 31 * 86400000;
    fs.utimesSync(`${file}.zst`, new Date(cutoff - 2 * 86400000), new Date(cutoff - 2 * 86400000));
    expect((await collect()).scan).toMatchObject({ complete: true, totalFiles: 0 });
    fs.writeFileSync(`${file}.zst`, zstdCompressSync(Buffer.from(rollout())));
    fs.utimesSync(`${file}.zst`, new Date(cutoff - 12 * 3600000), new Date(cutoff - 12 * 3600000));
    expect((await collect()).eventCount).toBe(3);
  });

  it('reads compressed files in partition workers with the same totals as inline', async () => {
    compress();
    const other = file.replace(UUID, 'abcdef01-1234-1234-1234-0123456789ab');
    fs.writeFileSync(
      `${other}.zst`,
      zstdCompressSync(Buffer.from(rollout().replaceAll(UUID, 'other-session')))
    );
    const inline = await collect({ fanoutThresholdBytes: Number.MAX_SAFE_INTEGER });
    let partitions = 0;
    const fanout = await runWithScopedCcsHome(root, () =>
      collectAccountActivity(
        { kind: 'codex', codexHome: path.join(root, 'codex'), cacheDir: path.join(root, 'fanout') },
        {
          minDate: NOW - 31 * 86400000,
          cacheDir: path.join(root, 'fanout'),
          fanoutThresholdBytes: 1,
        },
        {
          partitionRunner: async (_request, options, spec) => {
            partitions++;
            return collectCodexPartition(spec, options);
          },
        }
      )
    );
    expect(partitions).toBe(2);
    expect(fanout.scan?.complete).toBe(true);
    expect(fanout.hourly).toEqual(inline.hourly);
    expect(fanout.eventCount).toBe(6);
  });

  it('invalidates compressed-byte fingerprints when a finished file is rewritten', async () => {
    compress(rollout(1));
    expect((await collect()).eventCount).toBe(1);
    compress(rollout(2));
    expect((await collect()).eventCount).toBe(2);
  });

  it('discovers and reads compressed experiment logs, matching default session references', async () => {
    const experimentFile = path.join(
      root,
      'PM-Experiments',
      'job',
      'sessions',
      '2026',
      '10',
      '01',
      NAME
    );
    fs.mkdirSync(path.dirname(experimentFile), { recursive: true });
    fs.writeFileSync(`${experimentFile}.zst`, zstdCompressSync(Buffer.from(rollout())));
    const cacheDir = path.join(root, 'cache');
    while (await runExperimentRootsSlice({ homeDir: root, cacheDir })) {
      /* finish discovery */
    }
    const roots = readExperimentRoots(cacheDir, { homeDir: root }).roots.codex;
    expect(roots).toEqual([path.join(root, 'PM-Experiments', 'job', 'sessions')]);
    const request = {
      kind: 'codex' as const,
      codexHome: path.join(root, 'codex'),
      cacheDir,
      experimentRoots: roots,
    };
    const options = { minDate: NOW - 31 * 86400000, cacheDir };
    const experiment = await runWithScopedCcsHome(root, () =>
      collectAccountActivity(request, options)
    );
    expect(experiment.scan?.complete).toBe(true);
    expect(experiment.eventCount).toBe(3);
    // A compressed default reference excludes every copied record in the experiment root.
    compress();
    const duplicate = await runWithScopedCcsHome(root, () =>
      collectAccountActivity(request, options)
    );
    expect(duplicate.scan?.complete).toBe(true);
    expect(duplicate.eventCount).toBe(0);
  });

  it('discovers a compressed rollout whose session metadata exceeds the bounded sniff', async () => {
    compress(
      JSON.stringify({
        type: 'session_meta',
        payload: {
          id: UUID,
          originator: 'codex_cli_rs',
          instructions: 'x'.repeat(256 * 1024),
        },
      }) + '\n'
    );
    expect(await classifyCompressedCodexRollout(`${file}.zst`, Date.now() + 1000)).toBe('codex');
  });
});
