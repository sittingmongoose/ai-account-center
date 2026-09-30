import { activateCodexProfile, CodexActivationError } from '../activate-codex-profile';
import type { CodexActivationOptions } from '../activate-codex-profile';
import type { CodexCommandContext } from './types';

const USAGE = 'ccs codex-auth activate <name>';

export async function handleActivateCodex(
  ctx: CodexCommandContext,
  rawArgs: string[],
  options: CodexActivationOptions = {}
): Promise<void> {
  if (rawArgs.length !== 1 || rawArgs[0].startsWith('-')) {
    throw new CodexActivationError('invalid_profile', `Usage: ${USAGE}`);
  }
  const result = await activateCodexProfile(rawArgs[0], { ...options, registry: ctx.registry });
  process.stdout.write(
    `[OK] Activated Codex profile '${result.name}': ${result.email} (${result.plan ?? 'unknown plan'})\n`
  );
}
