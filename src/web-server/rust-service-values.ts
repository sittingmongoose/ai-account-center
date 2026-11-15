/** Pure helpers of the M3 bridge: the verified principal, reply checks and key order. */
import type { Request } from 'express';
import { createHash } from 'crypto';
import { authKind, requestDevice } from './middleware/request-auth';
import { isAccountRefreshSettings } from './services/account-refresh-settings';
import { PUBLIC_HEALTH_PRINCIPAL } from './rust-service-lifecycle';
import type { RustReply, VerifiedRustPrincipal } from './rust-service-transport';

/** Honest TS fallback for the new signed-in diagnostics endpoint. */
export function skeletonDiagnostics() {
  return {
    jobs: [],
    scheduler: { status: 'unavailable', lastSuccessAt: null, cause: 'jobs_not_migrated' },
    store: { status: 'unavailable', lastSuccessAt: null, cause: 'store_owned_by_ts' },
    computers: [],
  };
}

export function canonical(value: unknown): string {
  if (Array.isArray(value)) return '[' + value.map(canonical).join(',') + ']';
  if (value && typeof value === 'object')
    return (
      '{' +
      Object.entries(value)
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([k, v]) => JSON.stringify(k) + ':' + canonical(v))
        .join(',') +
      '}'
    );
  return JSON.stringify(value);
}

export function principal(req: Request, route: string): VerifiedRustPrincipal | null {
  // No caller-provided principal/role/secret header is inspected or copied.
  if (authKind(req) === 'session' && req.sessionID)
    return {
      kind: 'session',
      id: createHash('sha256').update(req.sessionID).digest('hex'),
      role: 'admin',
    };
  const device = requestDevice(req);
  if (device) return { kind: 'device', id: device.deviceId, role: 'device' };
  return route === '/api/health' ? PUBLIC_HEALTH_PRINCIPAL : null;
}

export function validReply(route: string, result: RustReply): boolean {
  if (result.status !== 200) return false;
  if (route === '/api/health') return canonical(result.body) === '{"status":"ok"}';
  if (route === '/api/accounts/settings') return isAccountRefreshSettings(result.body);
  return route === '/api/diag' && canonical(result.body) === canonical(skeletonDiagnostics());
}

/** Match the legacy serializer's property order without replacing any candidate values. */
export function orderedLike(candidate: unknown, reference: unknown): unknown {
  if (Array.isArray(reference) && Array.isArray(candidate))
    return candidate.map((value, index) => orderedLike(value, reference[index]));
  if (reference && candidate && typeof reference === 'object' && typeof candidate === 'object') {
    const values = candidate as Record<string, unknown>;
    return Object.fromEntries(
      Object.entries(reference).map(([key, value]) => [key, orderedLike(values[key], value)])
    );
  }
  return candidate;
}
