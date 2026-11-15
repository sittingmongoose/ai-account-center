/**
 * D7 Home view: the facts only the browser has, and the M3 presentation envelope the Rust builder reads.
 *
 * The page formats every time in its display time zone with its own `Intl` and sends what it alone knows in
 * `X-AAC-Home-Context` (its clock, zone, platform, refresh interval, user, host and a format probe of its
 * formatters). The envelope is the shared one (`rustReadInput`): this server's display-zone clock and day words
 * for every instant in the inputs, the `Date.parse` epochs of their ISO strings, and the `localeCompare` ranks of
 * the strings Home sorts by. Rust joins the words in the style the probe proves, and refuses when a fact is missing.
 */
import { rustReadInput } from '../rust-read-input';

/** The page's own context (view-model.mjs inputs), as aac-api `HomeContext`. */
export interface HomeContext {
  now: number;
  timeZone: string;
  platform: 'mac' | 'windows';
  intervalSeconds: number;
  username: string;
  host: string;
  formatProbe: string[];
}

const MAX_TEXT = 256;
const PROBE_LENGTH = 8;

/** A display zone this engine's `Intl` accepts (the envelope would otherwise fall back to UTC). */
function knownZone(timeZone: string): boolean {
  if (typeof timeZone !== 'string' || !timeZone || timeZone.length > 64) return false;
  try {
    new Intl.DateTimeFormat('en-US', { timeZone }).format(0);
    return true;
  } catch {
    return false;
  }
}

const text = (value: unknown, max = MAX_TEXT) => typeof value === 'string' && value.length <= max;

/**
 * The `X-AAC-Home-Context` header (URI-encoded JSON, so the probe's narrow spaces stay ASCII on the wire), checked
 * against the page's platform. Null for anything else.
 */
export function parseHomeContext(header: unknown, platform: string): HomeContext | null {
  if (typeof header !== 'string' || header.length > 4096) return null;
  let value: unknown;
  try {
    value = JSON.parse(decodeURIComponent(header));
  } catch {
    return null;
  }
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const v = value as Record<string, unknown>;
  const keys = [
    'now',
    'timeZone',
    'platform',
    'intervalSeconds',
    'username',
    'host',
    'formatProbe',
  ];
  if (
    Object.keys(v).length !== keys.length ||
    !keys.every((key) => Object.prototype.hasOwnProperty.call(v, key))
  )
    return null;
  if (
    !Number.isSafeInteger(v.now) ||
    (v.now as number) < 0 ||
    !text(v.timeZone, 64) ||
    !knownZone(v.timeZone as string) ||
    v.platform !== platform ||
    !Number.isInteger(v.intervalSeconds) ||
    (v.intervalSeconds as number) < 30 ||
    (v.intervalSeconds as number) > 3600 ||
    !text(v.username) ||
    !text(v.host) ||
    !Array.isArray(v.formatProbe) ||
    v.formatProbe.length !== PROBE_LENGTH ||
    !v.formatProbe.every((item) => text(item, 64))
  )
    return null;
  return {
    now: v.now as number,
    timeZone: v.timeZone as string,
    platform: v.platform as 'mac' | 'windows',
    intervalSeconds: v.intervalSeconds as number,
    username: v.username as string,
    host: v.host as string,
    formatProbe: [...(v.formatProbe as string[])],
  };
}

const record = (value: unknown): Record<string, unknown> | null =>
  value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;

/**
 * The strings Home compares with `localeCompare`: account ids (view-model.mjs `providerCards`, `text(id)`) and the
 * Claude sign-in profiles (claude-signins.mjs `signInsFor`, `String(profile)`). A pair without ranks refuses.
 */
function homeSortWords(dashboard: unknown): string[] {
  const data = record(dashboard);
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  const ids = accounts.map((account) => {
    const id = record(account)?.id;
    return typeof id === 'string' ? id : '';
  });
  const renewal = record(data?.claudeSignIns);
  const rows = Array.isArray(renewal?.signIns) ? renewal.signIns : [];
  const profiles = rows.map((row) => String(record(row)?.profile));
  return [...ids, ...profiles];
}

/**
 * GET /api/views/home's input to Rust: the page's context and the three answers as the envelope's data, with
 * this engine's clock/day words in the page's display zone, ISO epochs and the ranks of the words Home sorts by.
 */
export function homeReadInput(
  ctx: HomeContext,
  dashboard: unknown,
  profiles: unknown,
  antigravity: unknown
): unknown {
  return rustReadInput(
    { ctx, dashboard, profiles, antigravity },
    ctx.timeZone,
    homeSortWords(dashboard),
    {
      rankAll: false,
    }
  );
}

function object(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

/** The finished Home view as aac-api `HomeView` writes it (version 3). */
export function isHomeView(value: unknown): boolean {
  return (
    object(value) &&
    value.version === 3 &&
    Array.isArray(value.sections) &&
    Array.isArray(value.cards) &&
    Array.isArray(value.registry) &&
    object(value.chrome)
  );
}
