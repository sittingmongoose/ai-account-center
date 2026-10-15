/**
 * codex-auth login command.
 * Stages interactive OAuth privately, then uses the dashboard lifecycle guards to publish.
 */

import * as childProcess from 'child_process';
import { randomBytes } from 'crypto';
import { initUI, info, ok } from '../../utils/ui';
import { exitWithError } from '../../errors';
import { ExitCode } from '../../errors/exit-codes';
import { CodexAccountLifecycle } from '../../web-server/services/codex-account-lifecycle';
import { SignInJobError } from '../../web-server/services/signin-jobs';
import { parseArgs, rejectUnsupportedOptions, getProfileNameError } from './types';
import type { CodexCommandContext } from './types';

export async function handleLoginCodex(ctx: CodexCommandContext, args: string[]): Promise<void> {
  await initUI();
  const parsed = parseArgs(args);
  rejectUnsupportedOptions(parsed, 'ai-account-center codex-auth login <name>');

  const { profileName } = parsed;

  if (!profileName) {
    console.log('Usage: ai-account-center codex-auth login <name>');
    exitWithError('Profile name required', ExitCode.PROFILE_ERROR);
    return;
  }

  const nameError = getProfileNameError(profileName);
  if (nameError) {
    exitWithError(nameError, ExitCode.PROFILE_ERROR);
    return;
  }

  const lifecycle = new CodexAccountLifecycle({ registry: () => ctx.registry });
  const exists = ctx.registry.hasProfile(profileName);
  const flow = exists ? lifecycle.signInAgainFlow(profileName) : lifecycle.addFlow(profileName);
  const jobId = `job_${randomBytes(8).toString('hex')}`;
  let failed = false;
  let failureCode: ExitCode = ExitCode.AUTH_ERROR;
  try {
    const command = await flow.prepare(jobId);
    console.log(info(`Launching codex login for profile: ${profileName}`));
    const exitCode = await new Promise<number>((resolve) => {
      const child = childProcess.spawn(command.file, ['login'], {
        stdio: 'inherit',
        env: { ...process.env, CODEX_HOME: command.env.CODEX_HOME },
        windowsHide: true,
      });
      child.on('error', () => resolve(ExitCode.BINARY_ERROR));
      child.on('exit', (code) => resolve(code ?? 1));
    });
    if (exitCode !== 0) throw new SignInJobError('provider_denied');
    const result = await flow.complete(jobId, { stopped: () => false });
    console.log(ok(`Logged in as ${result.email}${result.plan ? ` (plan: ${result.plan})` : ''}`));
    console.log(`  Profile: ${profileName}`);
  } catch (error) {
    failed = true;
    if (error instanceof SignInJobError && error.code === 'tool_missing')
      failureCode = ExitCode.BINARY_ERROR;
    const message =
      error instanceof SignInJobError ? error.message : 'The sign-in could not be saved safely.';
    process.stderr.write(`[!] ${message} Previous credentials were preserved.\n`);
  } finally {
    await flow.cleanup(jobId);
  }
  if (failed) exitWithError('Codex sign-in failed', failureCode);
}
