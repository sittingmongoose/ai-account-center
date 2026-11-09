import { afterEach, describe, expect, it } from 'bun:test';
import path from 'path';
import { ScoreboardService } from '../../../../src/web-server/scoreboard/scoreboard-service';
import { composeByModel } from '../../../../src/web-server/scoreboard/by-model';
import type { NativeStatusHandle } from '../../../../src/status-native/native-loader';
import { NOW, tempDirs } from '../status/status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

/** A fake native handle: it records the view it was given and answers what the test wants. */
function fakeHandle(
  answer: () => unknown = () => ({ defaultView: 'pipeline', views: [] })
): { handle: NativeStatusHandle; views: unknown[] } {
  const views: unknown[] = [];
  const handle: NativeStatusHandle = {
    call: async (method, args) => {
      if (method !== 'composeByModel') throw new Error(`unexpected method ${method}`);
      views.push(JSON.parse(args) as unknown);
      return JSON.stringify(answer());
    },
  };
  return { handle, views };
}

const byModel = {
  defaultView: 'pipeline',
  intro: 'Which model to use for which job. ...',
  readNote: [],
  readLink: { label: 'What was read', anchor: 'sources' },
  bandSummary: '8 models with runs · 92 runs read at 4:30 PM',
  views: [],
} as const;

describe('byModel pass-through', () => {
  it('passes the module answer through untouched', async () => {
    const { handle } = fakeHandle(() => byModel);
    await expect(composeByModel({ version: 1 }, { handle })).resolves.toEqual(byModel);
  });

  it('is null when the module rejects, answers garbage, or is the wrong shape', async () => {
    const rejected = fakeHandle(() => {
      throw new Error('shape: a verdict row with no column');
    });
    await expect(composeByModel({}, { handle: rejected.handle })).resolves.toBeNull();
    const garbage = fakeHandle(() => undefined);
    await expect(composeByModel({}, { handle: garbage.handle })).resolves.toBeNull();
    const wrongShape = fakeHandle(() => ({ views: [] }));
    await expect(composeByModel({}, { handle: wrongShape.handle })).resolves.toBeNull();
  });

  it('the service attaches the field to the view, or serves none when it cannot be had', async () => {
    const base = dirs.make();
    const root = () => path.join(base, 'none');
    const composed = fakeHandle(() => byModel);
    const withField = new ScoreboardService({
      root,
      now: () => NOW,
      timeZone: () => 'UTC',
      native: composed.handle,
    });
    const view = await withField.view();
    expect(view.byModel).toEqual(byModel);
    // The composer saw the very view the field was attached to (without the field).
    expect(composed.views.length).toBe(1);
    const sent = composed.views[0] as { generatedAt: string; byModel?: unknown };
    expect(sent.generatedAt).toBe(view.generatedAt);
    expect(sent.byModel).toBeUndefined();

    const rejected = fakeHandle(() => {
      throw new Error('shape');
    });
    const withoutField = new ScoreboardService({
      root,
      now: () => NOW,
      timeZone: () => 'UTC',
      native: rejected.handle,
    });
    expect((await withoutField.view()).byModel).toBeUndefined();

    const noModule = new ScoreboardService({
      root,
      now: () => NOW,
      timeZone: () => 'UTC',
      native: null,
    });
    expect((await noModule.view()).byModel).toBeUndefined();
  });
});
