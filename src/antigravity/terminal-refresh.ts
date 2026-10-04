import { isNativeVersion } from './native-version';
import { refreshRuntimeDescriptor, type RuntimeRefreshResult } from './runtime-refresh';
import { ownsAntigravityState } from './terminal-activate';

export interface TerminalRefreshIo {
  write(text: string): void;
}

export interface TerminalRefreshDeps {
  ccsDir: string;
  home: string;
  uid: number | null;
  io?: TerminalRefreshIo;
  ownsState?: (ccsDir: string, uid: number | null) => boolean;
  refresh?: (
    request: Parameters<typeof refreshRuntimeDescriptor>[0]
  ) => Promise<RuntimeRefreshResult>;
}

function defaultIo(): TerminalRefreshIo {
  return {
    write: (text) => {
      process.stdout.write(text);
    },
  };
}

/** `ai-account-center antigravity runtime refresh`; returns the exit code. */
export async function runAntigravityTerminalRefresh(deps: TerminalRefreshDeps): Promise<number> {
  const io = deps.io ?? defaultIo();
  const say = (line: string) => io.write(`${line}\n`);
  const fail = (line: string) => {
    say(`[X] ${line}`);
    return 1;
  };
  if (!(deps.ownsState ?? ownsAntigravityState)(deps.ccsDir, deps.uid))
    return fail(
      "Antigravity runtime refresh runs only as the user that owns this computer's Antigravity state."
    );
  const result = await (deps.refresh ?? refreshRuntimeDescriptor)({
    ccsDir: deps.ccsDir,
    home: deps.home,
  });
  if (result.status === 'current') {
    say(`[OK] Antigravity runtime is current (reviewed ${result.version} build).`);
    return 0;
  }
  if (result.status === 'refreshed') {
    say(`[OK] Antigravity runtime refreshed to the reviewed ${result.version} build.`);
    say('The previous pin is kept as a backup; switching proofs re-read the descriptor.');
    return 0;
  }
  if (result.status === 'unreviewed') {
    const version = isNativeVersion(result.installedVersion) ? result.installedVersion : null;
    return fail(
      `Antigravity updated${version ? ` to ${version}` : ''}; switching paused until reviewed.`
    );
  }
  if (result.status === 'no-installation')
    return fail(
      'No Antigravity runtime is installed. Install it first: python3 -I scripts/antigravity/install_runtime.py --apply (step 3).'
    );
  if (result.status === 'gate-closed')
    return fail('Antigravity activation is not released in this build. Nothing was changed.');
  return fail('Antigravity runtime refresh could not complete its checks. Nothing was changed.');
}
