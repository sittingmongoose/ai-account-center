import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { zstdCompressSync } from 'zlib';
import { runWithScopedCcsHome } from '../../../src/utils/config-manager';
import { classifyCompressedCodexRollout } from '../../../src/web-server/usage/experiment-usage-signatures';
import {
  collectAccountActivity,
  collectCodexPartition,
  type AccountActivityScanOptions,
} from '../../../src/web-server/usage/account-activity-collector';
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

  it('reports a replay-only deadline cut, persists it, and retries that file first', async () => {
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
    let finished: Awaited<ReturnType<typeof collect>>;
    try {
      finished = await collect();
    } finally {
      reads.mockRestore();
    }
    expect(opened[0]).toBe(`${file}.zst`);
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
