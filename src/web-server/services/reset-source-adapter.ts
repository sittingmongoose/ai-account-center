/** Registry/manifest glue only. Native Rust owns normalization and all claim policy.
 * Host credentials stay inside the existing collector's verified Windows process.
 */
import * as fs from 'fs';
import * as path from 'path';
import { ProviderError } from '../../errors/error-types';
import { getCodexAuthProfilesSummary } from '../../codex-auth/codex-auth-dashboard-service';
import { loadNativeStatus } from '../../status-native/native-loader';
import type { ResetSource, ResetWrite } from '../routes/reset-claim-routes';
import { listClaudeDesktopProfiles } from './claude-desktop-profile-service';
import { isDefaultClaudeDataFolder } from './claude-account-records';
import { localUsageHost, runPrivateUsageHelper } from './usage-failover-transport';
import type { UsageHelperRequest, UsageHost } from './usage-failover-contract';

export function createResetSourceAdapter(): ResetSource {
  async function native(method: string, input: unknown) {
    const loaded = loadNativeStatus();
    if (!loaded.module) throw new ProviderError('Reset module unavailable.', 'reset-claims');
    return JSON.parse(await new loaded.module.NativeStatus().call(method, JSON.stringify(input)));
  }
  async function execute(account: string, write?: ResetWrite): Promise<unknown> {
    if (account.startsWith('codex:')) {
      const name = account.slice('codex:'.length);
      const summary = await getCodexAuthProfilesSummary();
      const profile = summary.profiles.find((p) => p.name === name && p.authValid);
      if (!profile || !profile.accountId)
        throw new ProviderError(
          'credential-host-unavailable: Exact saved profile unavailable.',
          'codex'
        );
      return native('resetCodexSource', {
        home: profile.codexHome,
        expectedAccount: profile.accountId,
        profile: profile.name,
        host: localUsageHost().host,
        write: write ?? null,
      });
    }
    if (!account.startsWith('claude:'))
      throw new ProviderError('provider-read-only: Provider read only.', 'reset-claims');
    const profiles = await listClaudeDesktopProfiles();
    const profile = profiles.find((p) => `claude:${p.id ?? p.email}` === account);
    const win = profile?.windows;
    if (!profile || !win || (!win.sshHost && process.platform !== 'win32')) {
      throw new ProviderError(
        'credential-host-unavailable: Credential host unavailable.',
        'claude'
      );
    }
    const host: UsageHost = { host: 'windows', platform: 'windows', sshHost: win.sshHost ?? null };
    const input = {
      action: write ? 'claim' : 'read',
      profile: profile.id ?? 'default',
      expectedEmail: profile.email,
      profileDir: win.profilePath ?? null,
      isDefault: win.isDefault === true || isDefaultClaudeDataFolder(win.profilePath),
      host: `windows:${win.sshHost ?? 'local'}`,
      write: write ?? null,
    };
    const script = await fs.promises.readFile(
      path.resolve(__dirname, '../../../scripts/account-usage/claude_usage.py'),
      'utf8'
    );
    // Strings are JSON literals in Python; no shell interpolation or temporary remote file.
    const source = [
      'import json,sys',
      'scope={"__name__":"aac_private_reset"}',
      `exec(compile(json.loads(${JSON.stringify(JSON.stringify(script))}),"managed-reset","exec"),scope)`,
      `request=json.loads(${JSON.stringify(JSON.stringify(input))})`,
      'print(json.dumps(scope["reset_host"](request),allow_nan=False))',
    ].join('\n');
    const transportRequest: UsageHelperRequest = {
      version: 1,
      action: 'inventory',
      provider: 'claude',
    };
    const response = JSON.parse(await runPrivateUsageHelper(host, transportRequest, source));
    if (write) return response;
    return native('resetNormalizeClaude', response);
  }
  return {
    read: (account) => execute(account),
    send: async (write) =>
      execute(write.routeAccount, write) as Promise<{
        outcome: string;
        retryAfterMs?: number;
      }>,
  };
}
