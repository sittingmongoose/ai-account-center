import { afterEach, describe, expect, it } from 'bun:test';
import path from 'path';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import { PmMailRegistryReader } from '../../../../src/web-server/status/status-collectors';
import {
  buildStatusView,
  type StatusInputs,
} from '../../../../src/web-server/status/status-view';
import {
  SPEND_BY_STREAM_UNAVAILABLE,
  composeSpendByStream,
} from '../../../../src/web-server/status/spend-by-stream';
import { StatusService } from '../../../../src/web-server/status/status-service';
import type { NativeStatusHandle } from '../../../../src/status-native/native-loader';
import { NOW, tempDirs } from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

/**
 * A fake native handle for the spend composer: it records the input it was given and answers
 * what the test wants, so what gets tested here is the pass-through, not the composer's own
 * words (those are the Rust crate's golden tests).
 */
function fakeHandle(
  answer: () => unknown = () => ({ state: 'ok', periods: [] })
): { handle: NativeStatusHandle; inputs: unknown[] } {
  const inputs: unknown[] = [];
  const handle: NativeStatusHandle = {
    call: async (method, args) => {
      if (method !== 'composeSpend') throw new Error(`unexpected method ${method}`);
      inputs.push(JSON.parse(args) as unknown);
      return JSON.stringify(answer());
    },
  };
  return { handle, inputs };
}

const panel = {
  state: 'ok',
  unavailable: null,
  defaultPeriod: 'week',
  asOf: '2026-11-08T18:02:00.000Z',
  asOfText: 'Read 1:02 PM',
  periods: [],
} as const;

describe('spendByStream pass-through', () => {
  it('passes the module answer through untouched', async () => {
    const { handle } = fakeHandle(() => panel);
    const view = await composeSpendByStream(
      { nowMs: NOW, utcOffsetMinutes: -300, accountOrder: [], spend: { periods: [] } },
      { handle }
    );
    expect(view).toEqual(panel);
  });

  it('sends the composer the input the contract names', async () => {
    const { handle, inputs } = fakeHandle(() => panel);
    await composeSpendByStream(
      { nowMs: 1_794_247_320_000, utcOffsetMinutes: -300, accountOrder: ['claude:me'], spend: { periods: [] } },
      { handle }
    );
    expect(inputs).toEqual([
      {
        nowMs: 1_794_247_320_000,
        utcOffsetMinutes: -300,
        accountOrder: ['claude:me'],
        spend: { periods: [] },
      },
    ]);
  });

  it('is the contract unavailable shape when the module rejects, answers garbage, or is gone', async () => {
    const rejected = fakeHandle(() => {
      throw new Error('invalid: missing field');
    });
    await expect(
      composeSpendByStream({ nowMs: NOW, utcOffsetMinutes: 0, accountOrder: [], spend: {} }, { handle: rejected.handle })
    ).resolves.toBe(SPEND_BY_STREAM_UNAVAILABLE);

    const garbage = fakeHandle(() => 'not json');
    await expect(
      composeSpendByStream({ nowMs: NOW, utcOffsetMinutes: 0, accountOrder: [], spend: {} }, { handle: garbage.handle })
    ).resolves.toBe(SPEND_BY_STREAM_UNAVAILABLE);

    const wrongShape = fakeHandle(() => ({ state: 'ok' }));
    await expect(
      composeSpendByStream({ nowMs: NOW, utcOffsetMinutes: 0, accountOrder: [], spend: {} }, {
        handle: wrongShape.handle,
      })
    ).resolves.toBe(SPEND_BY_STREAM_UNAVAILABLE);
  });

  it('is a field of the status view only when the service provides it', async () => {
    const root = path.join(dirs.make(), 'agent-reports');
    const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
    await reader.scan();
    const base: StatusInputs = {
      now: NOW,
      timeZone: 'UTC',
      reports: reader.snapshot(),
      registry: { data: [], asOf: NOW, error: null },
      machines: {
        vm: { data: null, asOf: null, error: null },
        mac: { data: null, asOf: null, error: null },
        windows: { data: null, asOf: null, error: null },
        nas1: { data: null, asOf: null, error: null },
      },
      browsers: { data: null, asOf: null, error: null },
      simlock: { data: null, asOf: null, error: null },
      usage: null,
      answers: [],
    };
    expect(buildStatusView(base).spendByStream).toBeUndefined();
    expect(buildStatusView({ ...base, spendByStream: panel }).spendByStream).toEqual(panel);
    expect(
      buildStatusView({ ...base, spendByStream: SPEND_BY_STREAM_UNAVAILABLE }).spendByStream
    ).toEqual(SPEND_BY_STREAM_UNAVAILABLE);
  });

  it('the service composes it over the quota scan and keeps the unavailable shape honestly', async () => {
    const { handle, inputs } = fakeHandle(() => panel);
    const root = path.join(dirs.make(), 'agent-reports');
    const service = new StatusService({
      reader: new AgentReportsReader({ root: () => root, now: () => NOW }),
      registryReader: new PmMailRegistryReader(() => path.join(root, 'none')),
      run: async () => ({ code: null, stdout: '', timedOut: false, missing: true }),
      readLocalMachine: async () => {
        throw new Error('not in tests');
      },
      readRemoteMachine: async () => {
        throw new Error('not in tests');
      },
      readRemotePins: async () => {
        throw new Error('not in tests');
      },
      timeZone: () => 'UTC',
      now: () => NOW,
      notify: () => undefined,
      spendNative: handle,
    });
    // Before any scan the field is the contract's unavailable shape, never absent.
    expect(service.view().spendByStream).toEqual(SPEND_BY_STREAM_UNAVAILABLE);
    await service.scanReports();
    expect(service.view().spendByStream).toEqual(panel);
    // The composer was called with the SpendView the server itself builds: an empty
    // reports root means no accepted quota report, so the spend view has no periods.
    expect(inputs.length).toBeGreaterThan(0);
    const input = inputs.at(-1) as { accountOrder: string[]; spend: { periods: unknown[] } };
    expect(input.accountOrder).toEqual([]);
    expect(input.spend.periods).toEqual([]);
  });
});
