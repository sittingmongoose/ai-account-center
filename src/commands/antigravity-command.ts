import os from 'os';
import { getCcsDir } from '../utils/config-manager';

type Writer = (line: string) => void;

export function printAntigravityHelp(writeLine: Writer = console.log): void {
  writeLine('AI Account Center Antigravity profiles');
  writeLine('');
  writeLine('Usage');
  writeLine('  ai-account-center antigravity signin <profile>');
  writeLine('  ai-account-center antigravity status');
  writeLine('  ai-account-center antigravity recover');
  writeLine('  ai-account-center antigravity activate <profile>');
  writeLine('');
  writeLine('Commands');
  writeLine('  signin <profile>   Add a new saved profile, or sign in again to a saved one');
  writeLine('  status             Show what account switching still needs (read-only)');
  writeLine('  recover            Finish or undo a stuck account switch (proof-driven)');
  writeLine('  activate <profile> Switch the Ubuntu login (same guarded path as the dashboard)');
  writeLine('');
  writeLine('Notes');
  writeLine('  Runs on Ubuntu in an interactive terminal (over SSH is fine). The official');
  writeLine('  Antigravity CLI starts in a private sign-in home: choose Google OAuth, open the');
  writeLine('  link in a browser, sign in to the Google account for this profile and paste');
  writeLine('  the code back. The live Antigravity login, history and settings are not');
  writeLine('  touched. Sign in again keeps the same Google account and is refused for the');
  writeLine('  live login. Switch accounts and remove profiles in the dashboard.');
  writeLine('  Recover re-proves the live Ubuntu login with the same identity check the');
  writeLine('  switcher uses, then completes the stuck switch, undoes it, or restores the');
  writeLine('  previous saved login when the live one matches neither. Saved profiles are');
  writeLine('  never changed. Anything unproven leaves the switch stuck.');
  writeLine('  Activate runs the same guarded switch the dashboard runs (idle proof,');
  writeLine('  identity proof, transaction, rollback) and only as the user that owns');
  writeLine("  this computer's Antigravity state. Running programs are listed for");
  writeLine('  review first; the switch needs your answer in an interactive terminal.');
}

/** Test seam: builds the runtime recover acts on. Production uses the installed one. */
export type AntigravityRecoverRuntime = () => {
  recover(): Promise<{
    status: string;
    profileId?: string;
    email?: string;
  }>;
} | null;

/** `ai-account-center antigravity ...`; returns the exit code. */
export async function handleAntigravityCommand(
  args: string[],
  writeLine: Writer = console.log,
  recoverRuntime?: AntigravityRecoverRuntime
): Promise<number> {
  if (!args.length || ['help', '--help', '-h'].includes(args[0])) {
    printAntigravityHelp(writeLine);
    return 0;
  }
  if (args[0] === 'status') {
    if (args.length !== 1) {
      writeLine('[X] Usage: ai-account-center antigravity status');
      return 1;
    }
    const { formatAntigravityReleaseStatus, readAntigravityReleaseStatus } = await import(
      '../antigravity/release-status'
    );
    for (const line of formatAntigravityReleaseStatus(
      readAntigravityReleaseStatus({ ccsDir: getCcsDir(), home: os.homedir() })
    ))
      writeLine(line);
    return 0;
  }
  if (args[0] === 'recover') {
    if (args.length !== 1) {
      writeLine('[X] Usage: ai-account-center antigravity recover');
      return 1;
    }
    // Loaded late so help and usage errors never construct a runtime.
    const { createInstalledAntigravityRuntimeFactory } = await import(
      '../antigravity/production-runtime'
    );
    const { PrivateStorageError } = await import('../antigravity/registry');
    const runtime = recoverRuntime
      ? recoverRuntime()
      : createInstalledAntigravityRuntimeFactory({ home: os.homedir() })(getCcsDir());
    if (!runtime) {
      writeLine('[X] No Antigravity profiles on this computer.');
      return 1;
    }
    let result: { status: string; profileId?: string; email?: string };
    try {
      result = await runtime.recover();
    } catch (error) {
      if (error instanceof PrivateStorageError && error.code === 'busy') {
        writeLine('[X] An Antigravity switch is running. Try recovery again when it finishes.');
        return 1;
      }
      throw error;
    }
    const who =
      result.profileId && result.email
        ? `${result.profileId} (${result.email})`
        : (result.profileId ?? result.email ?? 'the live login');
    if (result.status === 'completed') {
      writeLine('[OK] Antigravity recovery completed the stuck switch on Ubuntu.');
      writeLine(`The live login is ${who}, now the active profile.`);
      return 0;
    }
    if (result.status === 'aborted') {
      writeLine('[OK] Antigravity recovery undid the stuck switch on Ubuntu.');
      writeLine(`The live login is ${who}, still the active profile.`);
      return 0;
    }
    if (result.status === 'restored-previous') {
      writeLine('[OK] Antigravity recovery restored the previous login on Ubuntu.');
      writeLine(`The live login matched neither profile, so ${who} was restored and re-proved.`);
      return 0;
    }
    if (result.status === 'no-recovery-pending') {
      writeLine('[OK] No Antigravity switch needs recovery.');
      return 0;
    }
    writeLine('[X] Antigravity recovery could not prove the live Ubuntu login.');
    writeLine('The switch is still stuck; check the signed-in account, then run recovery again.');
    return 1;
  }
  if (args[0] === 'activate') {
    if (args.length !== 2 || args[1].startsWith('-')) {
      writeLine('[X] Usage: ai-account-center antigravity activate <profile>');
      return 1;
    }
    // Loaded late so help and usage errors never construct a runtime.
    const [{ createInstalledAntigravityRuntimeFactory }, { AntigravityAccountLifecycle }] =
      await Promise.all([
        import('../antigravity/production-runtime'),
        import('../antigravity/account-lifecycle'),
      ]);
    const { runAntigravityTerminalActivate } = await import('../antigravity/terminal-activate');
    const ccsDir = getCcsDir();
    const realHome = os.homedir();
    return runAntigravityTerminalActivate(args[1], {
      runtime: createInstalledAntigravityRuntimeFactory({ home: realHome })(ccsDir),
      lifecycle: new AntigravityAccountLifecycle({ ccsDir: () => ccsDir, home: () => realHome }),
      ccsDir,
      uid: process.getuid?.() ?? null,
    });
  }
  if (args[0] !== 'signin') {
    writeLine(
      `[X] Unknown antigravity command: ${args[0]}. Run: ai-account-center antigravity help`
    );
    return 1;
  }
  if (args.length !== 2 || args[1].startsWith('-')) {
    writeLine('[X] Usage: ai-account-center antigravity signin <profile>');
    return 1;
  }
  const [{ AntigravityAccountLifecycle }, { runAntigravityTerminalSignIn }] = await Promise.all([
    import('../antigravity/account-lifecycle'),
    import('../antigravity/terminal-signin'),
  ]);
  const ccsDir = getCcsDir();
  const realHome = os.homedir();
  return runAntigravityTerminalSignIn(args[1], {
    ccsDir,
    realHome,
    env: process.env,
    lifecycle: new AntigravityAccountLifecycle({ ccsDir: () => ccsDir, home: () => realHome }),
  });
}
