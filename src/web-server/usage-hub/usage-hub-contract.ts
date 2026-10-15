/**
 * The CLIProxyAPI management shapes that T3 Code reads, mirrored from its
 * client (pingdotgg/t3code `apps/server/src/usage/cliproxyApi.ts`, first at tag
 * v0.0.46-nightly.20261006.2735; line numbers below are from
 * v0.0.46-nightly.20261015.3145, where every shape here is unchanged). T3
 * decodes every answer with Effect Schema, which ignores extra keys and rejects
 * a missing required key, so the required fields below are always present.
 *
 * T3 calls, for Codex and Claude accounts only (cliproxyApi.ts:316-324):
 * - `GET /v0/management/auth-files` -> `{ files: AuthFile[] }` (lines 20-33);
 * - `POST /v0/management/api-call` with `{ auth_index, method, url, header, data? }`
 *   -> `{ status_code, body }`, where `body` is the upstream answer as a JSON
 *   string (lines 34, 176-188);
 *   - Codex usage: `https://chatgpt.com/backend-api/wham/usage` (lines 35-48, 253);
 *   - Claude usage: `https://api.anthropic.com/api/oauth/usage` (lines 49-72, 224);
 *   - Codex reset credits (lines 73-82, 192-207) and their redemption (lines
 *     326-377) are refused here, so T3 never offers "Use reset" for AAC rows.
 *
 * The URL is built from the hub address with any path dropped (line 134), so
 * the hub answers at the origin root; the key arrives as `Authorization: Bearer`
 * (line 139). Since 20261010 T3 also reads an optional
 * `id_token.chatgpt_account_id` (the Codex workspace, lines 218-220) to tell one
 * email's workspaces apart; this hub does not send it, so an AAC row joins T3's
 * own login by email alone.
 *
 * `src/cliproxy/services/stats-fetcher.ts` has a looser client-side
 * `CliproxyManagementAuthFile` (every field optional, numeric indexes). It
 * would not satisfy T3's decoder (`id` and `auth_index` are required strings),
 * so the server side keeps these exact types instead.
 */

export const USAGE_HUB_MOUNT = '/v0/management';

export const CODEX_USAGE_URL = 'https://chatgpt.com/backend-api/wham/usage';
export const CLAUDE_USAGE_URL = 'https://api.anthropic.com/api/oauth/usage';
/** Known to T3, refused here: AAC never redeems or lists reset credits for T3. */
export const CODEX_CREDITS_URL = 'https://chatgpt.com/backend-api/wham/rate-limit-reset-credits';

export type UsageHubProvider = 'codex' | 'claude';

/**
 * One entry of `GET /v0/management/auth-files`. T3 reads `id`, `auth_index`,
 * `provider`, `email`, `disabled` (and `id_token`, which is never sent here:
 * no field named after a token is ever serialised). The rest are what the real
 * hub also lists, for a person reading the answer; T3 ignores them.
 */
export interface UsageHubAuthFile {
  id: string;
  auth_index: string;
  provider: UsageHubProvider;
  type: UsageHubProvider;
  label: string;
  email?: string;
  disabled: false;
  /** `active` when a cached reading can be served, else `error`. */
  status: 'active' | 'error';
  status_message: string;
  /** When AAC took the reading (ISO), or null. An AAC extension; T3 ignores it. */
  sampled_at: string | null;
}

export interface UsageHubAuthFiles {
  files: UsageHubAuthFile[];
}

/** The `api-call` request body as T3 sends it (cliproxyApi.ts:176-188). */
export interface UsageHubApiCallRequest {
  auth_index?: unknown;
  method?: unknown;
  url?: unknown;
  header?: unknown;
  data?: unknown;
}

/** The `api-call` answer: the upstream status and body, never upstream headers. */
export interface UsageHubApiCallResponse {
  status_code: number;
  header: Record<string, string[]>;
  body: string;
}

/** `wham/usage` window as T3 decodes it (cliproxyApi.ts:35-39). */
export interface CodexUsageWindowBody {
  used_percent: number;
  /** Epoch seconds. */
  reset_at: number | null;
  limit_window_seconds?: number;
}

/** `wham/usage` body (cliproxyApi.ts:40-48); `rate_limit` is a required key. */
export interface CodexUsageBody {
  plan_type?: string;
  rate_limit: {
    primary_window: CodexUsageWindowBody | null;
    secondary_window: CodexUsageWindowBody | null;
  } | null;
}

/** `oauth/usage` window (cliproxyApi.ts:49-52): utilization is 0-100. */
export interface ClaudeUsageWindowBody {
  utilization: number;
  resets_at: string | null;
}

/** `oauth/usage` body (cliproxyApi.ts:53-72). */
export interface ClaudeUsageBody {
  five_hour: ClaudeUsageWindowBody | null;
  seven_day: ClaudeUsageWindowBody | null;
  limits: Array<{
    kind: 'weekly_scoped';
    percent: number;
    resets_at: string | null;
    scope: { model: { display_name: string } };
  }>;
}
