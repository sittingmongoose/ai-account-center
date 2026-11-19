import { runLifecycleEffects } from './lifecycle-effect-pipe';

export type ClaudeWorkflow = 'add' | 'remove' | 'restore' | 'purge' | 'recheck';
const EFFECTS = [
  'apply-mac',
  'apply-windows',
  'undo-mac',
  'undo-windows',
  'commit',
  'session',
] as const;
export type ClaudeEffect = (typeof EFFECTS)[number];
type Perform = (effect: ClaudeEffect) => Promise<boolean | void>;

async function runFallback(workflow: ClaudeWorkflow, perform: Perform): Promise<void> {
  if (workflow === 'recheck') {
    if ((await perform('session')) !== false) await perform('commit');
    return;
  }
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

export async function runClaudeWorkflow(
  root: string,
  workflow: ClaudeWorkflow,
  perform: Perform
): Promise<void> {
  await runLifecycleEffects(
    root,
    ['--claude-workflow', workflow],
    EFFECTS,
    perform,
    async (execute) => {
      await runFallback(workflow, execute);
      return true;
    }
  );
}
