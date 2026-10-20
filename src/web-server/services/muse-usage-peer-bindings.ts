import * as path from 'path';
import { DASHBOARD_HOSTS } from './dashboard-hosts';
import { readUsageCache } from './usage-failover-cache';
import {
  HEX_DIGEST,
  isRecord,
  parseUsageIdentity,
  sameUsageIdentity,
  type UsageHost,
  type UsageIdentity,
} from './usage-failover-contract';

export interface MuseUsagePeer {
  host: UsageHost['host'];
  identity: UsageIdentity;
}

export function parseMuseUsagePeer(value: unknown): MuseUsagePeer | null {
  if (
    !isRecord(value) ||
    Object.keys(value).sort().join(',') !== 'host,identity' ||
    !DASHBOARD_HOSTS.includes(value.host as UsageHost['host'])
  )
    return null;
  const identity = parseUsageIdentity(value.identity);
  return identity?.kind === 'muse-native-credential'
    ? { host: value.host as UsageHost['host'], identity }
    : null;
}

/** Explicit account links contain only fingerprints; they never copy a sign-in. */
export function musePeerBindingFile(ccsDir: string, key: string): string {
  return path.join(ccsDir, 'account-usage-failover', `${key}.muse-peers.json`);
}

/** Email or matching plan names cannot authorize a native credential for a portal team. */
export function isBoundMusePeer(
  ccsDir: string,
  key: string,
  primary: UsageIdentity | null,
  peer: UsageIdentity,
  host: UsageHost['host']
): boolean {
  if (
    !HEX_DIGEST.test(key) ||
    !primary ||
    !['muse-team', 'muse-native-credential'].includes(primary.kind) ||
    peer.kind !== 'muse-native-credential'
  )
    return false;
  const value = readUsageCache(musePeerBindingFile(ccsDir, key));
  if (
    !isRecord(value) ||
    Object.keys(value).sort().join(',') !== 'identity,key,peers,version' ||
    value.version !== 1 ||
    value.key !== key ||
    !sameUsageIdentity(parseUsageIdentity(value.identity), primary) ||
    !Array.isArray(value.peers) ||
    !value.peers.length ||
    value.peers.length > 8
  )
    return false;
  // Validate the whole grant, including unused entries, before accepting any of it.
  const peers = value.peers.map(parseMuseUsagePeer);
  if (peers.some((entry) => !entry)) return false;
  return peers.some((entry) => entry?.host === host && sameUsageIdentity(entry.identity, peer));
}
