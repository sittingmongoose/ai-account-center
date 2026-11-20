import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';

const STEPS = [
  'codex-staging',
  'orphan-keys',
  'antigravity-staging',
  'antigravity-orphans',
  'claude-trash',
] as const;
const DAY_MS = 24 * 60 * 60_000;
export type MaintenanceStep = (typeof STEPS)[number];

export function startLifecycleMaintenance(
  perform: (step: MaintenanceStep) => Promise<void>,
  binary?: string
): { stop(): void; native(): boolean } {
  let stopped = false;
  let native = false;
  let next = 0;
  let lastStart: number | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let active: Promise<void> = Promise.resolve();
  const performSafely = async (step: MaintenanceStep) => {
    if (stopped) return;
    try {
      await perform(step);
    } catch {
      // An unsuccessful sweep can be retried at the next interval.
    }
  };
  const schedule = () => {
    if (stopped) return;
    if (lastStart === null) {
      runFallback();
      return;
    }
    timer = setTimeout(runFallback, Math.max(0, DAY_MS - (Date.now() - lastStart)));
    timer.unref();
  };
  const runFallback = () => {
    if (stopped) return;
    lastStart = Date.now();
    active = active.then(async () => {
      for (const step of STEPS) await performSafely(step);
      schedule();
    });
  };

  if (!binary) {
    runFallback();
    return {
      native: () => false,
      stop: () => {
        stopped = true;
        clearTimeout(timer);
      },
    };
  }

  const child = spawn(binary, ['--lifecycle-maintenance'], {
    stdio: ['pipe', 'pipe', 'ignore'],
    windowsHide: true,
  });
  const lines = createInterface({ input: child.stdout });
  let retired = false;
  const retire = () => {
    if (retired) return;
    retired = true;
    clearTimeout(startup);
    lines.close();
    child.stdin.destroy();
    child.kill();
    native = false;
    // Never replay work handed to an older or failing child. Finish the other
    // sweeps in the current cycle, after the effect already in flight settles.
    if (!stopped && lastStart !== null && next > 0) {
      active = active.then(async () => {
        for (const step of STEPS.slice(next)) await performSafely(step);
      });
    }
    void active.then(schedule);
  };
  const startup = setTimeout(retire, 2000);
  startup.unref();
  let awaiting = false;
  lines.on('line', (line) => {
    if (stopped || retired) return;
    const step = STEPS[next];
    if (awaiting || line !== step) {
      retire();
      return;
    }
    clearTimeout(startup);
    native = true;
    if (next === 0) lastStart = Date.now();
    next = (next + 1) % STEPS.length;
    awaiting = true;
    active = performSafely(step).then(() => {
      awaiting = false;
      if (!stopped && !retired) child.stdin.write('\n');
    });
  });
  child.once('error', retire);
  child.once('exit', retire);
  child.stdin.on('error', retire);

  return {
    native: () => native,
    stop: () => {
      stopped = true;
      clearTimeout(timer);
      retire();
    },
  };
}
