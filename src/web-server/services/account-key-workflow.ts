import { runLifecycleEffects } from './lifecycle-effect-pipe';

const EFFECTS = ['store', 'publish', 'probe', 'unpublish', 'discard'] as const;
export type KeyEffect = (typeof EFFECTS)[number];
export type KeyWorkflow = 'add' | 'replace' | 'remove';
type Perform = (effect: KeyEffect) => Promise<boolean | void>;

async function runFallback(workflow: KeyWorkflow, perform: Perform): Promise<boolean> {
  if (workflow === 'add') {
    await perform('store');
    try {
      await perform('publish');
    } catch (error) {
      await perform('discard').catch(() => undefined);
      throw error;
    }
    const accepted = (await perform('probe')) !== false;
    if (!accepted) {
      await perform('unpublish').catch(() => undefined);
      await perform('discard').catch(() => undefined);
    }
    return accepted;
  }
  if (workflow === 'replace') {
    await perform('store');
    try {
      const accepted = (await perform('probe')) !== false;
      if (accepted) await perform('publish');
      return accepted;
    } finally {
      await perform('discard').catch(() => undefined);
    }
  }
  await perform('unpublish');
  try {
    await perform('discard');
  } catch (error) {
    await perform('publish').catch(() => undefined);
    throw error;
  }
  return true;
}

export function runKeyWorkflow(
  root: string,
  workflow: KeyWorkflow,
  perform: Perform
): Promise<boolean> {
  return runLifecycleEffects(root, ['--key-workflow', workflow], EFFECTS, perform, (execute) =>
    runFallback(workflow, execute)
  );
}
