/**
 * The Scoreboard view's `byModel` field through the aac-status native module
 * (native/aac-status/src/scoreboard.rs): a pure function over the Scoreboard view v1 this
 * server already builds, every word decided there. Any failure — no module, a rejected
 * call, an answer that is not the composer's own shape — means no field: the page then
 * says nothing rather than guessing, and nothing here throws.
 */
import type { NativeStatusHandle } from '../../status-native/native-loader';
import type { ByModelView } from './scoreboard-view-types';

function isByModelView(value: unknown): value is ByModelView {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const view = value as Record<string, unknown>;
  return view.defaultView === 'pipeline' && Array.isArray(view.views);
}

/**
 * The composed `byModel` field, or null when it cannot be had. The answer is the module's
 * own output, checked only for its top shape, as the composer is the authority for every
 * word.
 */
export async function composeByModel(
  view: unknown,
  options: { handle: NativeStatusHandle }
): Promise<ByModelView | null> {
  let answer: unknown;
  try {
    answer = JSON.parse(
      await options.handle.call('composeByModel', JSON.stringify(view))
    ) as unknown;
  } catch {
    return null;
  }
  return isByModelView(answer) ? answer : null;
}
