import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { ConfigError } from '../../errors/error-types';
import { activeFileWriterBinary } from '../../shared/private-file-lease';

export type ClaudeWorkflow = 'add' | 'remove' | 'restore' | 'purge';
const HOST_EFFECTS = ['apply-mac', 'apply-windows', 'undo-mac', 'undo-windows'] as const;
export type ClaudeEffect = (typeof HOST_EFFECTS)[number] | 'commit';
type Perform = (effect: ClaudeEffect) => Promise<boolean | void>;

async function runFallback(workflow: ClaudeWorkflow, perform: Perform): Promise<void> {
  const applied: ClaudeEffect[] = [];
  let failed: { error: unknown } | undefined;
  for (const [apply, undo] of [
    ['apply-mac', 'undo-mac'],
    ['apply-windows', 'undo-windows'],
  ] as const) {
    try {
      if ((await perform(apply)) !== false) applied.push(undo);
    } catch (error) {
      failed ??= { error };
      if (workflow !== 'purge') break;
    }
  }
  const hostFailed = failed !== undefined;
  try {
    if (failed) throw failed.error;
    await perform('commit');
  } catch (error) {
    if (workflow === 'add' || workflow === 'remove' || (workflow === 'restore' && hostFailed)) {
      for (const undo of applied.reverse()) await perform(undo).catch(() => undefined);
    }
    throw error;
  }
}

/**
 * Dispatch only a validated provider workflow. The private pipe carries action
 * words and outcome flags. It never receives launchers, account ids or secrets.
 */
export async function runClaudeWorkflow(
  root: string,
  workflow: ClaudeWorkflow,
  perform: Perform
): Promise<void> {
  const binary = activeFileWriterBinary(root);
  if (!binary) return runFallback(workflow, perform);
  const child = spawn(binary, ['--claude-workflow', workflow], {
    stdio: ['pipe', 'pipe', 'ignore'],
    windowsHide: true,
  });
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  const closed = new Promise<number | null>((resolve) => {
    child.once('error', () => resolve(null));
    child.once('close', resolve);
  });
  child.stdin.on('error', () => lines.close());
  const deadline = () => {
    const timer = setTimeout(() => child.kill(), 2000);
    timer.unref();
    return timer;
  };
  let timer = deadline();
  const history: Array<{
    effect: ClaudeEffect;
    result: { value: boolean | void } | { error: unknown };
  }> = [];
  let failed: { error: unknown } | undefined;
  try {
    for await (const line of lines) {
      if (line === 'ok' || line === 'failed') {
        if (history.length && (await closed) === 0) {
          if (failed) throw failed.error;
          if (line === 'ok') return;
        }
        break;
      }
      const effect = HOST_EFFECTS.find((candidate) => candidate === line);
      if (!effect && line !== 'commit') break;
      clearTimeout(timer);
      const step = effect ?? 'commit';
      let reply = 'e';
      try {
        const value = await perform(step);
        history.push({ effect: step, result: { value } });
        reply = value === false ? 'n' : 'y';
      } catch (error) {
        const result = { error };
        failed ??= result;
        history.push({ effect: step, result });
      }
      child.stdin.write(reply);
      timer = deadline();
    }
  } finally {
    clearTimeout(timer);
    lines.close();
    child.stdin.destroy();
    child.kill();
  }
  // The caller performs every effect. If the scheduler exits mid-workflow,
  // replay the receipts through the local scheduler and run only missing steps.
  let next = 0;
  return runFallback(workflow, async (effect) => {
    const receipt = history[next++];
    if (!receipt) return perform(effect);
    if (receipt.effect !== effect)
      throw new ConfigError('The Claude host workflow could not be confirmed.');
    if ('error' in receipt.result) throw receipt.result.error;
    return receipt.result.value;
  });
}
