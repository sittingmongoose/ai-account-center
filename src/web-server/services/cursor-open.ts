import { randomBytes } from 'node:crypto';
import { ConfigError } from '../../errors/error-types';
import { requestRustAgent } from '../rust-service-transport';
import { isSafeUsageSshAlias } from './dashboard-hosts';

/** True after native Open, false only when the legacy transport can run. */
export async function openCursorThroughAgent(
  root: string,
  sshHost: string,
  enabled: boolean
): Promise<boolean> {
  if (!enabled) return false;
  if (!isSafeUsageSshAlias(sshHost)) throw new ConfigError('Invalid Cursor host.');
  const reply = await requestRustAgent(
    root,
    'cursor-open',
    {
      computer: { platform: 'mac', sshHost },
      args: {},
      deadlineMs: 30_000,
      timeoutMs: 32_000,
      maxBytes: 4096,
      callId: randomBytes(16).toString('hex'),
    },
    undefined,
    true
  );
  if (reply === null || reply === undefined) return false;
  if (JSON.parse(reply).opened === true) return true;
  throw new ConfigError('The Cursor console session could not be opened.');
}
