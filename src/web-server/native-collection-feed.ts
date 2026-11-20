import { ConfigError, NetworkError } from '../errors/error-types';

/** The compatibility worker reads the boot's native producer until it owns the collection lease. */
export function workerReadsNative(): boolean {
  return process.env.AAC_RUST_CUT_WORKER === '1' && process.env.AAC_CUT_WORKER_COLLECTION !== '1';
}

export async function readNativeCollection(
  route: '/sample' | `/dashboard?${string}`,
  signal?: AbortSignal
): Promise<unknown> {
  const timeout = AbortSignal.timeout(15_000);
  const response = await requestNativeFeed(route, {
    signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
  });
  if (!response.ok) throw new NetworkError('The native account feed is unavailable.');
  return response.json();
}

export async function requestNativeFeed(route: string, options: RequestInit): Promise<Response> {
  const base = process.env.AAC_CUT_COLLECTION_FEED ?? '';
  const proof = process.env.AAC_CUT_COLLECTION_PROOF ?? '';
  if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(base) || !/^[a-f0-9]{64}$/.test(proof)) {
    throw new ConfigError('The native account feed is unavailable.');
  }
  return fetch(`${base}${route}`, {
    ...options,
    headers: { 'x-aac-collection-proof': proof },
    redirect: 'error',
  });
}
