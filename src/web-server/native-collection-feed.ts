import { ConfigError, NetworkError } from '../errors/error-types';

/** The compatibility worker reads the boot's native producer until it owns the collection lease. */
export function workerReadsNative(): boolean {
  return process.env.AAC_RUST_CUT_WORKER === '1' && process.env.AAC_CUT_WORKER_COLLECTION !== '1';
}

export async function readNativeCollection(
  route: '/sample' | `/dashboard?${string}`,
  signal?: AbortSignal
): Promise<unknown> {
  const base = process.env.AAC_CUT_COLLECTION_FEED ?? '';
  const proof = process.env.AAC_CUT_COLLECTION_PROOF ?? '';
  if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(base) || !/^[a-f0-9]{64}$/.test(proof)) {
    throw new ConfigError('The native account feed is unavailable.');
  }
  const timeout = AbortSignal.timeout(15_000);
  const response = await fetch(`${base}${route}`, {
    headers: { 'x-aac-collection-proof': proof },
    signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
    redirect: 'error',
  });
  if (!response.ok) throw new NetworkError('The native account feed is unavailable.');
  return response.json();
}
