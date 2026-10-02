import os from 'os';
import { getCcsDir } from '../../utils/config-manager';
import { AntigravityAccountLifecycle } from '../../antigravity/account-lifecycle';
import { nativeBinaryProblem, sweepSignInStaging } from '../../antigravity/signin-sandbox';
import { sweepAntigravitySignInMarkers } from '../../antigravity/signin-marker';
import { createLogger } from '../../services/logging';
import { broadcastDashboardEvent, type DashboardEventClient } from '../dashboard-events';
import { sweepOrphanKeys } from './account-key-sweep';
import { ClaudeAccountLifecycle } from './claude-account-lifecycle';
import { SshClaudeHostTransport } from './claude-host-transport';
import { CodexAccountLifecycle } from './codex-account-lifecycle';
import { MuseAccountLifecycle, museSignInEnabled } from './muse-account-lifecycle';
import type { ProviderRegistryFacts } from './dashboard-provider-registry';
import type { DashboardSignInUnavailableReason } from './account-dashboard-types';
import { SignInJobRunner, type SignInJob, type SignInJobRunnerDeps } from './signin-jobs';

/**
 * The lifecycle runtime: one sign-in job runner, the Codex and Claude flows,
 * the provider facts the dashboard advertises, and the startup and daily
 * maintenance (staging sweep, orphan-key sweep, trash purge).
 *
 * Claude Add, Remove and Restore are implemented against a host transport,
 * but they stay off by default until the contract's prerequisite ships (the
 * Windows launcher helper and the usage helper reading profile ids from the
 * inventory instead of their hard-coded lists) and a supervised dry run on both
 * hosts is approved. While off, the Claude add, remove and restore routes
 * answer 409 `not_implemented`, and the dashboard advertises neither.
 *
 * `CCS_CLAUDE_HOST_LIFECYCLE=on` in the server's environment turns them on for
 * that process only: the supervised dry run, or a sandbox whose ssh aliases
 * reach fake hosts. Nothing else turns them on; any other value leaves them off.
 * A computer's default Claude profile is refused (`account_protected`) either way.
 *
 * Muse Code Sign in again is built fully but unverified (the Mac `muse login`
 * output was never observed live). It stays off by default; `CCS_MUSE_SIGNIN=on`
 * turns it on for that process only, after the live verification step in
 * status/MUSE-LIFECYCLE.md. While off, Muse Sign in again answers 409
 * `not_implemented`.
 */
export const CLAUDE_HOST_LIFECYCLE_ENABLED = false;
export function claudeHostLifecycleEnabled(
  environment: Record<string, string | undefined> = process.env
): boolean {
  return CLAUDE_HOST_LIFECYCLE_ENABLED || environment.CCS_CLAUDE_HOST_LIFECYCLE === 'on';
}
const DAY_MS = 24 * 60 * 60_000;

const logger = createLogger('account-lifecycle');

/** A job as a client on a plain transport sees it: no verification URL or code, no email. */
export function redactSignInJob<
  T extends { verification: unknown; result: null | { email: string | null } },
>(job: T): T {
  return { ...job, verification: null, result: job.result ? { ...job.result, email: null } : null };
}

/**
 * The /ws copy of a job for one client (contract 6.6): browser sessions only,
 * never a device token or an unknown client; redacted unless the socket
 * connected over a secure transport.
 */
export function signInJobEvent(
  job: SignInJob,
  client: DashboardEventClient
): { type: 'signin-job'; job: SignInJob } | null {
  if (client.authKind !== 'session') return null;
  return { type: 'signin-job', job: client.secure ? job : redactSignInJob(job) };
}

export function auditLifecycle(event: string, data: Record<string, unknown>): void {
  try {
    logger.info(event, 'Account lifecycle event', data);
  } catch {
    /* Logging is best effort. */
  }
}

/**
 * What a saved change does beyond the audit line: by default only the /ws hint.
 * The lifecycle environment also drops the dashboard cache (it owns that import).
 */
let accountsChanged: () => void = () => {
  broadcastDashboardEvent({ type: 'accounts-changed' });
};

export function setAccountsChangedHandler(handler: () => void): void {
  accountsChanged = handler;
}

export function notifyAccountsChanged(): void {
  try {
    accountsChanged();
  } catch {
    /* Clients also refresh on their own schedule. */
  }
}

let runner: SignInJobRunner | null = null;
let codex: CodexAccountLifecycle | null = null;
let claude: ClaudeAccountLifecycle | null = null;
let antigravity: AntigravityAccountLifecycle | null = null;
let muse: MuseAccountLifecycle | null = null;

/** A runner wired like the server's: the /ws push, the audit line and the accounts-changed hint. */
export function createSignInJobRunner(
  deps: Omit<SignInJobRunnerDeps, 'onChange' | 'onFinish'> = {}
): SignInJobRunner {
  return new SignInJobRunner({
    ...deps,
    onChange: (job: SignInJob) => {
      broadcastDashboardEvent((client) => signInJobEvent(job, client));
    },
    onFinish: (job) => {
      // Values never; the account is named by provider and mode only.
      auditLifecycle('accounts.signin.job', {
        provider: job.provider,
        kind: job.kind,
        mode: job.mode,
        state: job.state,
      });
      if (job.state === 'succeeded') notifyAccountsChanged();
    },
  });
}

export function getSignInJobRunner(): SignInJobRunner {
  runner ??= createSignInJobRunner();
  return runner;
}

export function getCodexLifecycle(): CodexAccountLifecycle {
  codex ??= new CodexAccountLifecycle();
  return codex;
}

export function getClaudeLifecycle(): ClaudeAccountLifecycle {
  claude ??= new ClaudeAccountLifecycle({
    ccsDir: getCcsDir,
    transport: new SshClaudeHostTransport(),
    enabled: claudeHostLifecycleEnabled(),
  });
  return claude;
}

export function getAntigravityLifecycle(): AntigravityAccountLifecycle {
  antigravity ??= new AntigravityAccountLifecycle({ ccsDir: getCcsDir, home: os.homedir });
  return antigravity;
}

export function getMuseLifecycle(): MuseAccountLifecycle {
  muse ??= new MuseAccountLifecycle({ enabled: museSignInEnabled() });
  return muse;
}

/**
 * Antigravity sign-in runs in the user's terminal on Ubuntu
 * (`ai-account-center antigravity signin <profile>`): the official CLI's
 * first-run screens need the user's keys, so the in-browser supervised flow
 * is not served. With the CLI installed the provider reads `preflight_failed`
 * and Add and Sign in again answer with the terminal command (contract 6.2:
 * "otherwise ... preflight_failed, and the UI shows the terminal fallback").
 */
export function antigravitySignInFlow(
  home: string = os.homedir()
): DashboardSignInUnavailableReason {
  return nativeBinaryProblem(home, process.getuid?.() ?? null)
    ? 'tool_missing'
    : 'preflight_failed';
}

export interface LifecycleFactsSources {
  codexCliAvailable: () => boolean;
  claudeEnabled: () => boolean;
  /** Antigravity's flow reason; the default checks the installed CLI. */
  antigravityFlow?: () => DashboardSignInUnavailableReason;
  /** Muse Sign in again; off by default until its Mac output is verified live. */
  museEnabled?: () => boolean;
}

/** What the lifecycle routes can do now, for `providers[]` (contract section 2). */
export function lifecycleProviderFacts(
  context: { secureTransport?: boolean },
  sources: LifecycleFactsSources = {
    codexCliAvailable: () => getCodexLifecycle().codexCli() !== null,
    claudeEnabled: () => getClaudeLifecycle().enabled,
  }
): ProviderRegistryFacts {
  const claudeEnabled = sources.claudeEnabled();
  const museEnabled = (sources.museEnabled ?? (() => getMuseLifecycle().enabled))();
  return {
    lifecycleRoutes: true,
    secureTransport: context.secureTransport === true,
    flows: {
      ...(sources.codexCliAvailable() ? {} : { codex: 'tool_missing' as const }),
      ...(claudeEnabled ? {} : { claude: 'not_implemented' as const }),
      ...(museEnabled ? {} : { muse: 'not_implemented' as const }),
      antigravity: (sources.antigravityFlow ?? antigravitySignInFlow)(),
    },
    remove: { claude: claudeEnabled },
    recheck: { codex: false, claude: false, antigravity: false },
  };
}

let maintenance: ReturnType<typeof setInterval> | null = null;

async function runMaintenance(): Promise<void> {
  try {
    await getCodexLifecycle().sweepStaging();
  } catch {
    /* Retried at the next sweep. */
  }
  try {
    const count = await sweepOrphanKeys(getCcsDir());
    if (count > 0) auditLifecycle('accounts.keys.swept', { count });
  } catch {
    /* Retried at the next sweep. */
  }
  try {
    sweepSignInStaging(getCcsDir());
    sweepAntigravitySignInMarkers(getCcsDir());
  } catch {
    /* Retried at the next sweep. */
  }
  try {
    const swept = await getAntigravityLifecycle().sweepOrphans();
    if (swept.removed > 0 || swept.leftInPlace > 0)
      auditLifecycle('accounts.antigravity.orphans.swept', swept);
  } catch {
    /* Retried at the next sweep. */
  }
  try {
    const count = await getClaudeLifecycle().purgeDue();
    if (count > 0) auditLifecycle('accounts.trash.purge', { count });
  } catch {
    /* Retried at the next sweep. */
  }
}

/** At startup and once a day. */
export function startAccountLifecycleMaintenance(): void {
  if (maintenance) return;
  void runMaintenance();
  maintenance = setInterval(() => void runMaintenance(), DAY_MS);
  maintenance.unref?.();
}

export function stopAccountLifecycleMaintenance(): void {
  if (maintenance) clearInterval(maintenance);
  maintenance = null;
  runner?.shutdown();
}
