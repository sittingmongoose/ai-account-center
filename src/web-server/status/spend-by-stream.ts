/**
 * The Status view's `spendByStream` field through the aac-status native module
 * (native/aac-status/src/spend.rs): a pure function over the SpendView this server already
 * builds, every word decided there. Any failure — no module, a rejected call, an answer
 * that is not the composer's own shape — is the contract's fixed unavailable shape; nothing
 * here throws, guesses, or builds a figure of its own.
 */
import type { NativeStatusHandle } from '../../status-native/native-loader';
import type { SpendByStreamPanel, SpendByStreamUnavailable } from './status-view-types';

/** The contract's unavailable shape, the only one there is (CONTRACT-spend-bymodel.md). */
export const SPEND_BY_STREAM_UNAVAILABLE: SpendByStreamUnavailable = {
  state: 'unavailable',
  unavailable:
    'Spend by stream is unavailable: quota-watch has not written a report this server can read.',
  defaultPeriod: null,
  asOf: null,
  asOfText: null,
  periods: [],
};

/** The composer's input: the SpendView (`buildSpend`) plus the clock facts and the
 * snapshot's account order, exactly the shape the native module reads. */
export interface ComposeSpendInput {
  nowMs: number;
  utcOffsetMinutes: number;
  /** The SpendView's accounts by first appearance in the agent-usage snapshot. */
  accountOrder: string[];
  spend: unknown;
}

function isSpendByStreamPanel(value: unknown): value is SpendByStreamPanel {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const panel = value as Record<string, unknown>;
  if (panel.state !== 'ok' && panel.state !== 'unavailable') return false;
  return panel.state === 'unavailable' || Array.isArray(panel.periods);
}

/**
 * The composed `spendByStream` field. The answer is the module's own output, checked only
 * for its top shape, as the composer is the authority for every word; anything else reads
 * as unavailable.
 */
export async function composeSpendByStream(
  input: ComposeSpendInput,
  options: { handle: NativeStatusHandle }
): Promise<SpendByStreamPanel> {
  let answer: unknown;
  try {
    answer = JSON.parse(
      await options.handle.call('composeSpend', JSON.stringify(input))
    ) as unknown;
  } catch {
    return SPEND_BY_STREAM_UNAVAILABLE;
  }
  return isSpendByStreamPanel(answer) ? answer : SPEND_BY_STREAM_UNAVAILABLE;
}
