import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { ConfigError } from '../../errors/error-types';
import { activeFileWriterBinary } from '../../shared/private-file-lease';

/**
 * Dispatch only a validated lifecycle workflow. The private pipe carries action
 * words and outcome flags. Provider state and credentials stay with the handler.
 */
export async function runLifecycleEffects<E extends string>(
  root: string,
  args: string[],
  accepted: readonly E[],
  perform: (effect: E) => Promise<boolean | void>,
  fallback: (perform: (effect: E) => Promise<boolean | void>) => Promise<boolean>
): Promise<boolean> {
  const binary = activeFileWriterBinary(root);
  if (!binary) return fallback(perform);
  const child = spawn(binary, args, {
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
    effect: E;
    result: { value: boolean | void } | { error: unknown };
  }> = [];
  let failed: { error: unknown } | undefined;
  try {
    for await (const line of lines) {
      if (line === 'ok' || line === 'rejected' || line === 'failed') {
        if (history.length && (await closed) === 0) {
          if (line === 'failed' && failed) throw failed.error;
          if (line !== 'failed') return line === 'ok';
        }
        break;
      }
      const effect = accepted.find((candidate) => candidate === line);
      if (!effect) break;
      clearTimeout(timer);
      const step = effect;
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
  return fallback(async (effect) => {
    const receipt = history[next++];
    if (!receipt) return perform(effect);
    if (receipt.effect !== effect)
      throw new ConfigError('The lifecycle workflow could not be confirmed.');
    if ('error' in receipt.result) throw receipt.result.error;
    return receipt.result.value;
  });
}
