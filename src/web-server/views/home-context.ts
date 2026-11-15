/**
 * D7 Home view: the facts only the browser has, and the M3 presentation envelope the Rust builder reads.
 *
 * The page formats every time in its display time zone with its own `Intl` and sends what it alone knows in
 * `X-AAC-Home-Context` (its clock, zone, platform, refresh interval, user, host, a format probe of its formatters
 * and its display zone's UTC offsets over a window of days). The envelope is the shared one (`rustReadInput`): this
 * server's display-zone clock and day words for every instant in the inputs, the `Date.parse` epochs of their ISO
 * strings, and the `localeCompare` ranks of the strings Home sorts by. Rust joins the words in the style the probe
 * proves, checks each against the page's offsets, and refuses when a fact is missing or differs.
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
  /** The page's display-zone UTC offsets (view-model.mjs `homeZoneFacts`); Rust checks every word against them. */
  zone: HomeZone;
}

/** `{from, to, offsets: [[start, minutes], ...]}` as the page's `Intl` found them. */
export interface HomeZone {
  from: number;
  to: number;
  offsets: [number, number][];
}

const MAX_TEXT = 256;
const PROBE_LENGTH = 8;
const MAX_ZONE_SEGMENTS = 64;

/** The zone facts' shape only; their meaning (order, window, every word) is checked by the Rust builder. */
function zoneFacts(value: unknown): HomeZone | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const v = value as Record<string, unknown>;
  if (Object.keys(v).sort().join(',') !== 'from,offsets,to') return null;
  if (!Number.isSafeInteger(v.from) || !Number.isSafeInteger(v.to)) return null;
  if (!Array.isArray(v.offsets) || !v.offsets.length || v.offsets.length > MAX_ZONE_SEGMENTS)
    return null;
  const offsets: [number, number][] = [];
  for (const row of v.offsets) {
    if (!Array.isArray(row) || row.length !== 2 || !row.every((n) => Number.isSafeInteger(n)))
      return null;
    offsets.push([row[0] as number, row[1] as number]);
  }
  return { from: v.from as number, to: v.to as number, offsets };
}

/** Zone names this engine has answered for (R1b: building a formatter per request cost about 0.5 ms). */
const zoneNames = new Map<string, boolean>();
const ZONE_NAMES = 128;

/** A display zone this engine's `Intl` accepts (the envelope would otherwise fall back to UTC). */
function knownZone(timeZone: string): boolean {
  if (typeof timeZone !== 'string' || !timeZone || timeZone.length > 64) return false;
  const known = zoneNames.get(timeZone);
  if (known !== undefined) return known;
  let accepted: boolean;
  try {
    new Intl.DateTimeFormat('en-US', { timeZone }).format(0);
    accepted = true;
  } catch {
    accepted = false;
  }
  // The engine's answer for a name does not change while it runs; the map stays small.
  if (zoneNames.size < ZONE_NAMES) zoneNames.set(timeZone, accepted);
  return accepted;
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
    'zone',
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
  const zone = zoneFacts(v.zone);
  if (!zone) return null;
  return {
    now: v.now as number,
    timeZone: v.timeZone as string,
    platform: v.platform as 'mac' | 'windows',
    intervalSeconds: v.intervalSeconds as number,
    username: v.username as string,
    host: v.host as string,
    formatProbe: [...(v.formatProbe as string[])],
    zone,
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
export function homeSortWords(dashboard: unknown): string[] {
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
