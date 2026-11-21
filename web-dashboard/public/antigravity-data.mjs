import { timeLabel } from './accounts-data.mjs';
import { visibleUsageWindows } from './visible-usage.mjs';

// Google keeps separate Antigravity limits for the CLI that T3 runs (keys "cli-...") and for the
// Antigravity app. They are never added together. Home shows only T3's (what T3 spends); the app's
// appear only in the account's Details (the owner, 2026-11-19: "I won't often care about that").
const ANTIGRAVITY_LABELS = [
  { key: 'cli-gemini-5h', label: 'Gemini 5-hour, in T3 (Antigravity CLI)' },
  { key: 'cli-gemini-weekly', label: 'Gemini weekly, in T3 (Antigravity CLI)' },
  { key: 'cli-3p-5h', label: 'Claude and GPT 5-hour, in T3 (Antigravity CLI)' },
  { key: 'cli-3p-weekly', label: 'Claude and GPT weekly, in T3 (Antigravity CLI)' },
  { key: 'gemini-5h', label: 'Gemini 5-hour, in the Antigravity app' },
  { key: 'gemini-weekly', label: 'Gemini weekly, in the Antigravity app' },
  { key: '3p-5h', label: 'Claude and GPT 5-hour, in the Antigravity app' },
  { key: '3p-weekly', label: 'Claude and GPT weekly, in the Antigravity app' },
];
const antigravityService = key => (typeof key === 'string' && key.startsWith('cli-') ? 'cli' : 'app');
/** Home's canonical columns: T3's four (Antigravity CLI). */
const ANTIGRAVITY_COLUMNS = ANTIGRAVITY_LABELS.filter(column => antigravityService(column.key) === 'cli');
/** The windows Home shows for an Antigravity account: T3's (CLI) pools only, never the app's. */
export function antigravityHomeWindows(windows) {
  return (Array.isArray(windows) ? windows : []).filter(window => antigravityService(window?.key) === 'cli');
}
export function antigravityWindowLabel(window) {
  return window?.key === 'google-ai-credits' ? 'AI credits (overage)'
    : ANTIGRAVITY_LABELS.find(column => column.key === window?.key)?.label || window?.label || 'Usage';
}
/** Canonical provider bucket IDs identify display columns; extra windows keep their own labels. */
export function antigravityColumns(accounts) {
  const windows = accounts.flatMap(account => visibleUsageWindows('antigravity', antigravityHomeWindows(account.windows)))
    .filter(window => !['balance', 'spend', 'extra_usage'].includes(window.kind) && window.unlimited !== true);
  const canonical = windows.some(window => ANTIGRAVITY_COLUMNS.some(column => column.key === window.key));
  const columns = canonical ? ANTIGRAVITY_COLUMNS.map(column => ({ ...column })) : [];
  // Extra T3 pools (such as Gemini 4 Argon), in the order the accounts report them.
  for (const window of windows)
    if (!columns.some(column => column.key === window.key)) columns.push({ key: window.key, label: antigravityWindowLabel(window) });
  return columns;
}
export function antigravityMissingWindow(account, key) {
  const plan = account?.antigravityPlan;
  if (['3p-5h', '3p-weekly', 'cli-3p-5h', 'cli-3p-weekly'].includes(key) && plan?.thirdPartyModels === false) return 'Not on plan';
  if (['gemini-5h', '3p-5h', 'cli-gemini-5h', 'cli-3p-5h'].includes(key) && plan?.quotaPolicy === 'weekly') return 'Weekly only';
  // An extra pool column (such as Gemini 4 Argon) comes from another account. When this
  // account's own live reading of the same service (T3's CLI or the app) arrived without it,
  // Google reported no such quota there: that is not a failed reading, so it is never called
  // unavailable (and never shown as zero). A service that did not answer, or a cached reading
  // (the last good one after a failed refresh), stays unavailable.
  if (!ANTIGRAVITY_COLUMNS.some(column => column.key === key) && antigravityHasLiveReading(account, antigravityService(key))) return 'Not reported';
  return 'Unavailable';
}
function antigravityHasLiveReading(account, service) {
  return account?.status === 'ok'
    && visibleUsageWindows('antigravity', account.windows)
      .some(window => window.kind !== 'balance' && window.status !== 'cached' && window.cached !== true
        && window.status !== 'unavailable' && antigravityService(window.key) === service);
}
const SERVICE_NAMES = { cli: 'T3 (Antigravity CLI)', app: 'The Antigravity app' };
/** Details: a quota service that failed this read (its pools arrive as `unavailable` windows) is named,
 *  so a Live account never hides it; the other service's readings stay as they are. */
export function antigravityServiceFacts(account) {
  if (account?.provider !== 'antigravity' || !Array.isArray(account.windows)) return [];
  return ['cli', 'app']
    .filter(service => account.windows.some(window => window?.status === 'unavailable' && antigravityService(window.key) === service))
    .map(service => ({
      label: 'Notice',
      value: `${SERVICE_NAMES[service]} quota could not be read this time; its pools show Unavailable. The other readings are live.`,
      mono: false,
    }));
}
export function antigravityNoQuota(account) {
  return account?.provider === 'antigravity' && account.antigravityPlan?.quotaPolicy === 'none'
    && (!Array.isArray(account.windows) || account.windows.length === 0);
}
export function antigravityPlanFacts(account) {
  const plan = account?.provider === 'antigravity' ? account.antigravityPlan : null;
  if (!Array.isArray(plan?.models)) return [];
  const models = plan.models.filter(model => typeof model === 'string');
  const versions = [...new Set(models.flatMap(model => model.match(/^Claude (?:Sonnet|Opus) (\d+\.\d+)$/)?.[1] || []))].sort();
  const orchestrator = model => /^Orchestrator \(.+\)$/.test(model);
  const groups = [
    ...(models.some(model => model.startsWith('Gemini ')) ? ['Gemini'] : []),
    ...(models.some(orchestrator) ? ['Orchestrator'] : []),
    ...(versions.length ? [`Claude ${versions.join('/')}`] : []),
    ...(models.includes('GPT-OSS-120B') ? ['GPT-OSS'] : []),
    ...models.filter(model => !model.startsWith('Gemini ') && !orchestrator(model) && !/^Claude (?:Sonnet|Opus) \d+\.\d+$/.test(model) && model !== 'GPT-OSS-120B'),
  ];
  return [{ label: 'Models', value: groups.length === 1 && groups[0] === 'Gemini' ? 'Gemini only' : groups.join('; ') || 'None', mono: false }];
}
export function antigravityPlanNote(account) {
  const plan = account?.provider === 'antigravity' ? account.antigravityPlan : null;
  if (!plan || typeof plan !== 'object') return '';
  const paragraphs = [
    typeof plan.summary === 'string' ? plan.summary : '',
    Array.isArray(plan.models) ? `Models: ${plan.models.filter(model => typeof model === 'string').join(', ') || 'None'}` : '',
    ['pro', 'ultra', 'ultra-5x', 'ultra-20x'].includes(plan.class) ? 'Family members sharing this plan may share one quota pool.' : '',
  ].filter(Boolean);
  // Explicit short lines give the existing Slint note its full height at narrow pane widths.
  return paragraphs.flatMap(paragraph => {
    const lines = [''];
    for (const word of paragraph.split(/\s+/)) {
      const last = lines.length - 1;
      if (lines[last] && lines[last].length + word.length + 1 > 40) lines.push(word);
      else lines[last] += `${lines[last] ? ' ' : ''}${word}`;
    }
    return lines;
  }).join('\n');
}
export function antigravityCreditsHint(account) {
  return account?.antigravityPlan?.creditsOverage === false ? 'Not usable for Antigravity on this plan.'
    : 'Used only after the plan quota runs out, when AI Credit Overages is on.';
}

const profileId = value => typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(value);
const range = (value, min, max) => Number.isInteger(value) && value >= min && value <= max;
const moduleName = value => typeof value === 'string' && /^[A-Za-z_][A-Za-z0-9_.]{0,63}$/.test(value);
const minor = value => typeof value === 'string' && /^\d{1,2}\.\d{1,3}$/.test(value);
/** Why the Ubuntu runtime service is not running (server read-only check), in plain words; null when unknown. */
export function antigravityServiceProblem(inventory) {
  const problem = inventory?.runtimeServiceProblem;
  if (!problem || typeof problem !== 'object') return null;
  const name = moduleName(problem.module) ? problem.module : 'unknown';
  const moved = minor(problem.python) && minor(problem.builtFor) && problem.python !== problem.builtFor
    ? ` (Python ${problem.python}; runtime built for ${problem.builtFor})` : '';
  const rebuild = '; managed updates wait until the runtime bundle is rebuilt';
  if (problem.reason === 'missing-python-module') return `Runtime service failed: missing Python module ${name}${moved}${rebuild}`;
  if (problem.reason === 'parser-mismatch') return `Runtime service failed: Python module ${name} is not the pinned version${moved}${rebuild}`;
  if (problem.reason === 'parser-import-failed') return `Runtime service failed: Python module ${name} does not load${moved}${rebuild}`;
  if (problem.reason === 'service-failed') return `Runtime service failed${range(problem.exitStatus, 1, 255) ? ` (exit status ${problem.exitStatus})` : ''}; managed updates wait until its cause is fixed`;
  return null;
}
function boundProfiles(data, inventory) {
  if (inventory?.schemaVersion !== 1 || inventory.hostId !== 'ubuntu' || !Array.isArray(inventory.profiles) || inventory.profiles.length > 16) return [];
  const counts = new Map();
  for (const p of inventory.profiles) counts.set(p?.id, (counts.get(p?.id) || 0) + 1);
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  return inventory.profiles.flatMap(profile => {
    if (!profileId(profile?.id) || counts.get(profile.id) !== 1 || profile.hostId !== 'ubuntu' || typeof profile.email !== 'string'
      || typeof profile.available !== 'boolean' || typeof profile.selected !== 'boolean' || typeof profile.runtimeVerified !== 'boolean') return [];
    const matches = accounts.filter(row => row.provider === 'antigravity' && row.id === `antigravity:profile:${profile.id}`
      && row.capabilities?.antigravityProfileId === profile.id && row.platform === 'ubuntu'
      && row.email === profile.email);
    if (matches.length !== 1) return [];
    return [{ profile, account: matches[0] }];
  });
}
/** The saved Ubuntu profiles bound to their dashboard rows: which login is live, and what the runtime reports. */
export function antigravityView(data, inventory) {
  const rows = boundProfiles(data, inventory);
  return {
    antigravityAccounts: rows.map(row => ({
      id: row.account.id, profile: row.profile.id, email: row.profile.email, plan: row.account.plan || row.profile.plan || '',
      ...(row.account.antigravityPlan ? { antigravityPlan: row.account.antigravityPlan } : {}),
      selected: row.profile.selected, runtimeVerified: row.profile.runtimeVerified,
      status: (row.profile.selected ? row.profile.runtimeVerified ? 'Selected · running verified' : 'Selected · runtime unverified' : row.account.status === 'needs_sign_in' ? 'Sign-in needed' : row.profile.available ? 'Saved login' : 'Login unavailable') + ({ cached: ' · Cached', needs_sign_in: ' · Sign-in needed', unavailable: ' · Usage unavailable', error: ' · Refresh failed' }[row.account.status] || ''),
      note: [row.account.source, row.account.status, timeLabel(row.account.sampledAt, 'Sampled'), row.account.message].filter(Boolean).join(' · '),
    })),
    antigravityServiceProblem: antigravityServiceProblem(inventory),
  };
}
