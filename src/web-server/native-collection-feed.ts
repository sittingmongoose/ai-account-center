import { ConfigError, NetworkError } from '../errors/error-types';

/** The compatibility worker reads the boot's native producer. */
export function workerReadsNative(): boolean {
  return process.env.AAC_RUST_CUT_WORKER === '1';
}

export async function readNativeCollection(route: `/dashboard?${string}`): Promise<unknown> {
  const response = await requestNativeFeed(process.env.AAC_CUT_COLLECTION_FEED, route, {
    signal: AbortSignal.timeout(15_000),
  });
  if (!response.ok) throw new NetworkError('The native account feed is unavailable.');
  return response.json();
}

export function requestNativeStore(
  route: `/store/${string}`,
  options: RequestInit
): Promise<Response> {
  return requestNativeFeed(process.env.AAC_CUT_STORE_FEED, route, options);
}

async function requestNativeFeed(
  base: string | undefined,
  route: string,
  options: RequestInit
): Promise<Response> {
  const proof = process.env.AAC_CUT_COLLECTION_PROOF ?? '';
  if (!base || !/^http:\/\/127\.0\.0\.1:\d+$/.test(base) || !/^[a-f0-9]{64}$/.test(proof)) {
    throw new ConfigError('The native account feed is unavailable.');
  }
  return fetch(`${base}${route}`, {
    ...options,
    headers: { 'x-aac-collection-proof': proof },
    redirect: 'error',
  });
}
