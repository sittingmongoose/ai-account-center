/**
 * Why an Update apps row ended as it did: a fixed cause class, plus what ran (command, exit code,
 * phase times), which copy it used (install owner) and a short stderr tail. The helper already
 * scrubs all of it; this boundary keeps only known fields with bounded shapes and scrubs text again,
 * so a credential, an email or another user's folder never reaches the page or a saved job.
 */
import { record, safeVersion } from './app-update-values';

export const APP_UPDATE_CAUSES = [
  'none',
  'auth_required',
  'signin_needed',
  'policy_blocked',
  'wrong_prefix',
  'not_writable',
  'lost_session_reply',
  't3_absent',
  't3_not_ready',
  't3_restart_pending',
  'installer_failed',
  'in_use',
  'timeout',
  'unknown',
] as const;
export type AppUpdateCause = (typeof APP_UPDATE_CAUSES)[number];

export interface AppUpdateOwnerPackage {
  package: 'muse-acp' | 'zcode-acp-server';
  manager: string;
  prefix: string;
  source: string;
  version: string | null;
}
export interface AppUpdateOwnerCopy {
  package: 'muse-acp' | 'zcode-acp-server';
  prefix: string;
  version: string | null;
}
/** Which copy an update used and how it was chosen: fixed words and home-relative paths only. */
export interface AppUpdateOwner {
  manager?: string;
  runtime?: 'native' | 'harness';
  target?: 't3-offer' | 'cli-manifest';
  source?: string;
  executable?: string;
  firstOnPath?: string;
  onPath?: boolean;
  copies?: number;
  packages?: AppUpdateOwnerPackage[];
  elsewhere?: AppUpdateOwnerCopy[];
}
export interface AppUpdateDiagnostics {
  cause: AppUpdateCause;
  /** One fixed word naming the step or the refusal (no_enabled_instance, http_401, previous_server). */
  reason?: string;
  /** What ran: the program's own name and its fixed arguments, never a script body. */
  command?: string;
  exitCode?: number;
  /** Milliseconds per step (t3_ready, offer, install, verify, refresh_models, update, ...). */
  phases?: Record<string, number>;
  owner?: AppUpdateOwner;
  notes?: string[];
  /** The last lines of a failed command's stderr, scrubbed. */
  tail?: string;
}

/** Codes that name a cause, each allowed on one app and status (and Windows only for policy refusals). */
export const CAUSE_CODE_RULES: Record<string, { app: string; status: string; windows?: true }> = {
  cursor_signin_needed: { app: 'cursor-cli', status: 'action_required' },
  policy_blocked: { app: 'cursor-cli', status: 'action_required', windows: true },
  wrong_prefix: { app: 't3-acp-adapters', status: 'failed' },
  prefix_not_writable: { app: 't3-acp-adapters', status: 'action_required' },
  t3_session_reply_lost: { app: 'antigravity-acp', status: 'failed' },
  t3_unreachable: { app: 'antigravity-acp', status: 'action_required' },
  t3_auth_failed: { app: 'antigravity-acp', status: 'action_required' },
  t3_absent: { app: 'antigravity-acp', status: 'action_required' },
  t3_not_ready: { app: 'antigravity-acp', status: 'action_required' },
  t3_restart_pending: { app: 'antigravity-acp', status: 'action_required' },
  antigravity_acp_signin_hold: { app: 'antigravity-acp', status: 'action_required' },
  antigravity_acp_signin_needed: { app: 'antigravity-acp', status: 'action_required' },
  antigravity_acp_models_stale: { app: 'antigravity-acp', status: 'updated' },
  antigravity_acp_offer_unknown: { app: 'antigravity-acp', status: 'unknown' },
};

/** Whether a row's cause code is somewhere it cannot come from (the row then reads helper_invalid). */
export function causeCodeMisplaced(
  code: string,
  appId: string,
  status: unknown,
  windows: boolean
): boolean {
  const rule = Object.prototype.hasOwnProperty.call(CAUSE_CODE_RULES, code)
    ? CAUSE_CODE_RULES[code]
    : undefined;
  return (
    !!rule && (rule.app !== appId || rule.status !== status || (rule.windows === true && !windows))
  );
}

const WORD = /^[a-z0-9][a-z0-9_.:-]{0,63}$/;
const PACKAGES = ['muse-acp', 'zcode-acp-server'] as const;
export const TAIL_CHARS = 1200;
const TAIL_LINES = 12;
const LINE_CHARS = 200;
const PATH_CHARS = 160;

const SECRET_PATTERNS: Array<[RegExp, string]> = [
  [/\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}/gi, '$1 <redacted>'],
  [
    /(["']?(?:[a-z0-9_-]*(?:token|secret|password|passwd|apikey|api[_-]key|authorization|cookie|credential|session[_-]?id|bearer)[a-z0-9_-]*)["']?\s*[:=]\s*)("[^"]*"|'[^']*'|[^\s,;}&]+)/gi,
    '$1<redacted>',
  ],
  [/\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}(?:\.[A-Za-z0-9_-]+)?/g, '<redacted>'],
  [/\b(?:npm_|ghp_|gho_|github_pat_|sk-|xox[abpr]-|aiza)[A-Za-z0-9_-]{8,}/gi, '<redacted>'],
  [/\b(https?|wss?):\/\/[^/\s@]+@/gi, '$1://'],
  [/\b((?:https?|wss?):\/\/[^\s?#"']+)[?#][^\s"']*/gi, '$1'],
  [/[\w.+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b/g, '<email>'],
  [/[A-Za-z0-9+_=-]{40,}/g, '<redacted>'],
];

function scrubPaths(text: string): string {
  return text
    .replace(/([A-Z]:(?:\\\\|\\|\/)+Users(?:\\\\|\\|\/)+)[^\\/\s"']+/gi, '$1<user>')
    .replace(/(\/(?:Users|home)\/)[^/\s"']+/g, '$1<user>');
}

/** The dashboard's own pass over helper text: control characters, credentials, people and other users' folders. */
export function scrubText(value: string, lines = TAIL_LINES): string {
  let text = value.slice(-65536).replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, '');
  text = text.replace(/\r\n?/g, '\n');
  text = text.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, ' ');
  text = scrubPaths(text);
  for (const [pattern, replacement] of SECRET_PATTERNS) text = text.replace(pattern, replacement);
  return text
    .split('\n')
    .map((line) => line.trimEnd().slice(0, LINE_CHARS))
    .filter((line) => line.trim())
    .slice(-lines)
    .join('\n')
    .slice(-TAIL_CHARS);
}

function word(value: unknown): string | undefined {
  return typeof value === 'string' && WORD.test(value) ? value : undefined;
}

function displayPath(value: unknown): string | undefined {
  if (typeof value !== 'string' || !value || value.length > 1024) return undefined;
  const text = scrubText(value, 1).slice(-PATH_CHARS);
  return text || undefined;
}

function count(value: unknown, maximum: number): number | undefined {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 && value <= maximum
    ? value
    : undefined;
}

function adapterName(value: unknown): AppUpdateOwnerPackage['package'] | undefined {
  return PACKAGES.find((name) => name === value);
}

function ownerOf(value: unknown): AppUpdateOwner | undefined {
  const raw = record(value);
  if (!raw) return undefined;
  const owner: AppUpdateOwner = {};
  const manager = word(raw.manager);
  if (manager) owner.manager = manager;
  if (raw.runtime === 'native' || raw.runtime === 'harness') owner.runtime = raw.runtime;
  if (raw.target === 't3-offer' || raw.target === 'cli-manifest') owner.target = raw.target;
  const source = word(raw.source);
  if (source) owner.source = source;
  const executable = displayPath(raw.executable);
  if (executable) owner.executable = executable;
  const first = displayPath(raw.firstOnPath);
  if (first) owner.firstOnPath = first;
  if (typeof raw.onPath === 'boolean') owner.onPath = raw.onPath;
  const copies = count(raw.copies, 32);
  if (copies !== undefined) owner.copies = copies;
  if (Array.isArray(raw.packages)) {
    const packages: AppUpdateOwnerPackage[] = [];
    for (const item of raw.packages.slice(0, 4).map(record)) {
      const name = adapterName(item?.package);
      const prefix = displayPath(item?.prefix);
      if (!item || !name || !prefix) continue;
      packages.push({
        package: name,
        manager: word(item.manager) ?? 'unknown',
        prefix,
        source: word(item.source) ?? 'unknown',
        version: safeVersion(item.version),
      });
    }
    if (packages.length) owner.packages = packages;
  }
  if (Array.isArray(raw.elsewhere)) {
    const copiesFound: AppUpdateOwnerCopy[] = [];
    for (const item of raw.elsewhere.slice(0, 8).map(record)) {
      const name = adapterName(item?.package);
      const prefix = displayPath(item?.prefix);
      if (!item || !name || !prefix) continue;
      copiesFound.push({ package: name, prefix, version: safeVersion(item.version) });
    }
    if (copiesFound.length) owner.elsewhere = copiesFound;
  }
  return Object.keys(owner).length ? owner : undefined;
}

/** A helper row's diagnostics, or undefined when it sent none (older helpers, saved rows). */
export function normalizeDiagnostics(value: unknown): AppUpdateDiagnostics | undefined {
  const raw = record(value);
  if (!raw || !APP_UPDATE_CAUSES.includes(raw.cause as AppUpdateCause)) return undefined;
  const result: AppUpdateDiagnostics = { cause: raw.cause as AppUpdateCause };
  const reason = word(raw.reason);
  if (reason) result.reason = reason;
  if (typeof raw.command === 'string' && raw.command.length <= 1024) {
    const command = scrubText(raw.command, 1).slice(0, PATH_CHARS);
    if (command) result.command = command;
  }
  if (
    typeof raw.exitCode === 'number' &&
    Number.isSafeInteger(raw.exitCode) &&
    raw.exitCode >= -2147483648 &&
    raw.exitCode < 4294967296
  )
    result.exitCode = raw.exitCode;
  const phases = record(raw.phases);
  if (phases) {
    const kept: Record<string, number> = {};
    for (const [name, ms] of Object.entries(phases).slice(0, 8)) {
      const value = count(ms, 86_400_000);
      if (word(name) && value !== undefined) kept[name] = value;
    }
    if (Object.keys(kept).length) result.phases = kept;
  }
  const owner = ownerOf(raw.owner);
  if (owner) result.owner = owner;
  if (Array.isArray(raw.notes)) {
    const notes = raw.notes.slice(0, 6).flatMap((note) => word(note) ?? []);
    if (notes.length) result.notes = notes;
  }
  if (typeof raw.tail === 'string' && raw.tail && result.cause !== 'none') {
    const tail = scrubText(raw.tail);
    if (tail) result.tail = tail;
  }
  return result;
}
