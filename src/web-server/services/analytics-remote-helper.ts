import { createHash } from 'crypto';
import * as path from 'path';
import { ValidationError } from '../../errors/error-types';
import { readPackagedSource } from '../../utils/packaged-source';

const MAX_HELPER_BYTES = 128 * 1024;

/**
 * The packaged helper's SHA-256, pinned like the Claude history writer: only these exact bytes are ever run on the
 * Mac, Windows and Nas1. Update it together with `scripts/analytics-remote/analytics_usage_remote.py`. The pin
 * covers the helper's LF text (`readPackagedSource`), so a package built from a CRLF checkout still passes.
 */
export const ANALYTICS_HELPER_SHA256 =
  'eddd174e9ccad171280ac7c8cc11ca253e99d9a5fb8982d5b2889679a150df54';

/** The packaged helper is missing, oversized or fails its pin: no host can be scanned. */
export class AnalyticsHelperIntegrityError extends ValidationError {
  constructor() {
    super('Analytics remote helper is unavailable.');
    this.name = 'AnalyticsHelperIntegrityError';
  }
}

export function analyticsHelperPath(): string {
  return path.resolve(__dirname, '../../../scripts/analytics-remote/analytics_usage_remote.py');
}

/** The pinned helper's source text, or AnalyticsHelperIntegrityError before any host is contacted. */
export async function readAnalyticsHelper(helperPath = analyticsHelperPath()): Promise<string> {
  const bytes = await readPackagedSource(helperPath).catch(() => {
    throw new AnalyticsHelperIntegrityError();
  });
  if (
    bytes.length > MAX_HELPER_BYTES ||
    createHash('sha256').update(bytes).digest('hex') !== ANALYTICS_HELPER_SHA256
  )
    throw new AnalyticsHelperIntegrityError();
  return bytes.toString('utf8');
}
