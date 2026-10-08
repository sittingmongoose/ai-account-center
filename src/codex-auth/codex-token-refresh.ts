/**
 * The Codex CLI's refresh-token grant, for saved logins that no Codex process
 * owns. The endpoint is fixed: unlike Codex, no environment variable can send
 * a refresh token elsewhere (tests inject `fetch`). Results carry enumerated
 * codes only; response bodies and parser errors never leave this module.
 */

export const CODEX_REFRESH_URL = 'https://auth.openai.com/oauth/token';
export const CODEX_REFRESH_CLIENT_ID = 'app_EMoamEEZ73f0CkXaXp7hrann';
/**
 * Long enough that a slow answer is still received: once OpenAI has rotated the
 * token, a response dropped on timeout leaves the saved login dead. Other users
 * of the activation lock wait about 10 s and then report busy and retry.
 */
export const CODEX_REFRESH_TIMEOUT_MS = 20_000;
const MAX_RESPONSE_CHARS = 256 * 1024;

/** Codex's permanent refresh failures, plus the OAuth form of a rejected grant. */
const DEAD_CODES = [
  'refresh_token_expired',
  'refresh_token_reused',
  'refresh_token_invalidated',
  'invalid_grant',
] as const;

export type CodexRefreshDeadCode = (typeof DEAD_CODES)[number] | 'unauthorized';
export type CodexRefreshTransientCode =
  | 'timeout'
  | 'network'
  | 'rate_limited'
  | 'server_error'
  | 'http_error';

export interface CodexRefreshedTokens {
  idToken?: string;
  accessToken: string;
  refreshToken?: string;
}

/** `tokens` is in-memory only: never log, persist or return it past the renewal write. */
export type CodexTokenRefreshResult =
  | { kind: 'ok'; tokens: CodexRefreshedTokens }
  | { kind: 'dead'; code: CodexRefreshDeadCode; httpStatus: number }
  | { kind: 'transient'; code: CodexRefreshTransientCode; httpStatus: number | null }
  | { kind: 'invalid_response'; httpStatus: number };

export interface CodexTokenFetchInit {
  method: 'POST';
  headers: Record<string, string>;
  body: string;
  redirect: 'manual';
  signal: AbortSignal;
}

export type CodexTokenFetch = (
  url: string,
  init: CodexTokenFetchInit
) => Promise<{ status: number; text(): Promise<string> }>;

export interface CodexTokenRefreshOptions {
  /** Test seam; production uses the global fetch (and its proxy dispatcher). */
  fetch?: CodexTokenFetch;
  timeoutMs?: number;
}

function parseObject(text: string): Record<string, unknown> | null {
  try {
    const value = JSON.parse(text) as unknown;
    return value && typeof value === 'object' && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : null;
  } catch {
    // Parser messages can quote the body; nothing is reported.
    return null;
  }
}

/** A known permanent code from `error`, `error.code` or `code`; never the raw text. */
function deadCode(body: Record<string, unknown> | null): (typeof DEAD_CODES)[number] | null {
  const error = body?.error;
  const candidates = [
    typeof error === 'string' ? error : null,
    error && typeof error === 'object' ? (error as Record<string, unknown>).code : null,
    body?.code,
  ];
  for (const candidate of candidates) {
    const known = DEAD_CODES.find((code) => code === candidate);
    if (known) return known;
  }
  return null;
}

function optionalToken(value: unknown): string | undefined | null {
  if (value === undefined || value === null) return undefined;
  return typeof value === 'string' && value.length > 0 ? value : null;
}

function isTimeout(error: unknown): boolean {
  const name = (error as { name?: unknown } | null)?.name;
  return name === 'TimeoutError' || name === 'AbortError';
}

/** One refresh request. Never throws; a lost response is reported as transient. */
export async function refreshCodexTokens(
  refreshToken: string,
  options: CodexTokenRefreshOptions = {}
): Promise<CodexTokenRefreshResult> {
  const send: CodexTokenFetch = options.fetch ?? ((url, init) => fetch(url, init));
  let status: number;
  let text: string;
  try {
    const response = await send(CODEX_REFRESH_URL, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({
        client_id: CODEX_REFRESH_CLIENT_ID,
        grant_type: 'refresh_token',
        refresh_token: refreshToken,
        scope: 'openid profile email',
      }),
      // A redirect would resend the refresh token to another URL.
      redirect: 'manual',
      signal: AbortSignal.timeout(options.timeoutMs ?? CODEX_REFRESH_TIMEOUT_MS),
    });
    status = response.status;
    text = await response.text();
  } catch (error) {
    return { kind: 'transient', code: isTimeout(error) ? 'timeout' : 'network', httpStatus: null };
  }
  if (status >= 200 && status < 300) {
    const body = text.length > MAX_RESPONSE_CHARS ? null : parseObject(text);
    const accessToken = optionalToken(body?.access_token);
    const idToken = optionalToken(body?.id_token);
    const refreshed = optionalToken(body?.refresh_token);
    if (!accessToken || idToken === null || refreshed === null) {
      return { kind: 'invalid_response', httpStatus: status };
    }
    return {
      kind: 'ok',
      tokens: {
        accessToken,
        ...(idToken ? { idToken } : {}),
        ...(refreshed ? { refreshToken: refreshed } : {}),
      },
    };
  }
  if (status === 400 || status === 401) {
    const code = deadCode(text.length > MAX_RESPONSE_CHARS ? null : parseObject(text));
    // Codex treats every 401 from this endpoint as permanent; a 400 only with a known code.
    if (code) return { kind: 'dead', code, httpStatus: status };
    if (status === 401) return { kind: 'dead', code: 'unauthorized', httpStatus: status };
  }
  if (status === 429) return { kind: 'transient', code: 'rate_limited', httpStatus: status };
  if (status >= 500) return { kind: 'transient', code: 'server_error', httpStatus: status };
  return { kind: 'transient', code: 'http_error', httpStatus: status };
}
