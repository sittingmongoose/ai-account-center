import { createHash } from 'crypto';
import fs from 'fs/promises';
import path from 'path';
import { ConfigError } from '../../errors/error-types';
import {
  ADDITIONAL_PROVIDERS,
  isAdditionalProvider,
  isSafeUsageSshAlias,
  type AdditionalProvider,
  type AdditionalUsageSource,
} from './additional-usage-transport';

/**
 * The version 1 source manifest, `account-usage-sources.json`: at most one
 * entry per additional provider. It is read exactly as before registry v2 and
 * never written by the dashboard, so an older package keeps reading it.
 */
export const SOURCE_MANIFEST_FILE = 'account-usage-sources.json';
const MAX_MANIFEST_BYTES = 32 * 1024;

export interface SourceManifest {
  fingerprint: string;
  valid: boolean;
  sources: AdditionalUsageSource[];
}

function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

export async function readSourceManifestFile(ccsDir: string): Promise<string | null> {
  let handle: Awaited<ReturnType<typeof fs.open>> | undefined;
  try {
    handle = await fs.open(path.join(ccsDir, SOURCE_MANIFEST_FILE), 'r');
    const buffer = Buffer.alloc(MAX_MANIFEST_BYTES + 1);
    const { bytesRead } = await handle.read(buffer, 0, buffer.length, 0);
    if (bytesRead > MAX_MANIFEST_BYTES) throw new ConfigError('Invalid source configuration.');
    return buffer.subarray(0, bytesRead).toString('utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw new ConfigError('Invalid source configuration.');
  } finally {
    await handle?.close();
  }
}

/** Defaults for every provider, overridden by the validated entries; invalid falls back. */
export function parseSourceManifest(contents: string | null): SourceManifest {
  const defaults = ADDITIONAL_PROVIDERS.map((provider) => ({
    provider,
    platform: 'ubuntu' as const,
  }));
  const fingerprint = createHash('sha256')
    .update(contents ?? 'local-ubuntu-defaults')
    .digest('hex');
  if (contents === null) return { fingerprint, valid: true, sources: defaults };
  try {
    if (Buffer.byteLength(contents, 'utf8') > MAX_MANIFEST_BYTES) {
      throw new ConfigError('Invalid source configuration.');
    }
    const config: unknown = JSON.parse(contents);
    if (
      !record(config) ||
      config.version !== 1 ||
      Object.keys(config).some((key) => !['version', 'sources'].includes(key)) ||
      !Array.isArray(config.sources) ||
      config.sources.length > ADDITIONAL_PROVIDERS.length
    ) {
      throw new ConfigError('Invalid source configuration.');
    }
    const configured = new Map<AdditionalProvider, AdditionalUsageSource>();
    for (const entry of config.sources) {
      if (
        !record(entry) ||
        !isAdditionalProvider(entry.provider) ||
        !['ubuntu', 'mac', 'windows'].includes(entry.platform as string) ||
        Object.keys(entry).some((key) => !['provider', 'platform', 'sshHost'].includes(key)) ||
        (entry.sshHost !== undefined && !isSafeUsageSshAlias(entry.sshHost)) ||
        configured.has(entry.provider)
      ) {
        throw new ConfigError('Invalid source configuration.');
      }
      configured.set(entry.provider, {
        provider: entry.provider,
        platform: entry.platform as AdditionalUsageSource['platform'],
        ...(entry.sshHost === undefined ? {} : { sshHost: entry.sshHost }),
      });
    }
    return {
      fingerprint,
      valid: true,
      sources: defaults.map((source) => configured.get(source.provider) ?? source),
    };
  } catch {
    return { fingerprint, valid: false, sources: defaults };
  }
}
