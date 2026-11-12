import { HOST_IDS, HOST_LABELS } from './computers.mjs';
// Claude Code sign-in renewal (dashboard DTO `claudeSignIns`, from scripts/claude-signin).
//
// Every Claude Code sign-in ends about 30 days after it was made; the renewal runner renews each one
// a few days before. These pure helpers turn the runner's published state into the words the Claude
// rows, Details and the "needs you" notice show. They never invent a time: a sign-in whose end was
// not read says so.
import { lazyFormat } from './time-format.mjs';

const when = lazyFormat({ month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' });
const day = lazyFormat({ month: 'short', day: 'numeric' });
const HOSTS = HOST_LABELS;
const valid = (value) => typeof value === 'string' && Number.isFinite(Date.parse(value));
const hostWord = (host) => HOSTS[host] || host;
const RESULT_WORDS = {
  renewed: 'renewed',
  waiting_for_you: 'waiting for you',
  end_not_moved: 'did not save',
  login_failed: 'login failed',
  no_login_link: 'login did not start',
  wrong_account: 'wrong browser account',
  wrong_account_saved: 'saved another account',
  claude_not_found: 'Claude Code not found',
};
// One line in a Details fact; the whole sentence goes in the Details note, which wraps.
const NEED_WORDS = {
  captcha: 'A CAPTCHA in its Chrome window',
  mail_link_missing: 'Sign in to claude.ai in its Chrome window',
  browser_signin_failed: 'Sign in to claude.ai in its Chrome window',
  wrong_account: 'Its Chrome profile is on another account',
  renewal_failing: 'Renewal keeps failing',
};
const resultWord = (value) =>
  RESULT_WORDS[value] || (typeof value === 'string' ? value.replace(/_/g, ' ') : '');

/** The runner's state, or null when no runner publishes (the dashboard then shows nothing new). */
export function renewalOf(data) {
  const value = data?.claudeSignIns;
  return value && typeof value === 'object' && Array.isArray(value.signIns) ? value : null;
}

/** This account's sign-ins on every computer, Mac first. */
export function signInsFor(data, email) {
  const renewal = renewalOf(data);
  const address = typeof email === 'string' ? email.trim().toLowerCase() : '';
  if (!renewal || !address) return [];
  const rank = (host) => {
    const index = ['mac', 'windows', ...HOST_IDS.filter(id => id !== 'mac' && id !== 'windows')].indexOf(host);
    return index < 0 ? 9 : index;
  };
  return renewal.signIns
    .filter((row) => row?.email === address)
    .sort((a, b) => rank(a.host) - rank(b.host) || String(a.profile).localeCompare(String(b.profile)));
}

/** What needs a person, for this account or (without an email) for any. */
export function needsFor(data, email) {
  const renewal = renewalOf(data);
  if (!renewal || !Array.isArray(renewal.needsYou)) return [];
  const address = typeof email === 'string' ? email.trim().toLowerCase() : null;
  return renewal.needsYou.filter((need) => need && (address === null || need.email === address));
}

/** One row's state in a few words: "ends Dec 2", "logged out", "renewal failed (login failed)". */
function rowWords(row, now) {
  if (row.state === 'logged_out' || row.state === 'no_refresh_token') return 'logged out';
  if (row.state === 'unreadable') return 'not readable';
  if (row.state === 'not_signed_in') return 'not signed in';
  if (!valid(row.signInEndsAt)) return 'end not reported';
  const end = Date.parse(row.signInEndsAt);
  const parts = [`${end <= now ? 'ended' : 'ends'} ${when.format(new Date(end))}`];
  if (row.lastResult === 'renewed' && valid(row.lastRenewAt))
    parts.push(`renewed ${day.format(new Date(row.lastRenewAt))}`);
  else if (row.lastResult) parts.push(`last try: ${resultWord(row.lastResult)}`);
  if (row.autoRenew === false) parts.push('not renewed automatically');
  else if (valid(row.renewFrom) && Date.parse(row.renewFrom) > now)
    parts.push(`renews from ${day.format(new Date(row.renewFrom))}`);
  else if (row.state === 'ok') parts.push('renewing now');
  return parts.join(' · ');
}

/**
 * The Claude row's sign-in line: '' without a runner, else what matters most: a person is needed,
 * the runner stopped, a sign-in ended, or the soonest end ("Auto-renews · next end Dec 2"). Kept short:
 * it shares the row's meta line with the plan and the sample age.
 */
export function claudeSignInLine(data, email, now = Date.now()) {
  const renewal = renewalOf(data);
  const rows = signInsFor(data, email);
  if (!renewal || rows.length === 0) return '';
  if (needsFor(data, email).length > 0) return 'Sign-in renewal needs you';
  if (renewal.stale) return 'Sign-in renewal stopped';
  const out = rows.filter((r) => r.state === 'logged_out' || r.state === 'no_refresh_token');
  if (out.length) return `Signed out on ${[...new Set(out.map((r) => hostWord(r.host)))].join(', ')}`;
  const ends = rows.map((r) => r.signInEndsAt).filter(valid).map(Date.parse).sort((a, b) => a - b);
  if (!ends.length) return 'Auto-renews';
  return `Auto-renews · next end ${day.format(new Date(ends[0]))}`;
}

/** Details facts: the runner, any person-only step, then one line per computer and profile. */
export function claudeSignInFacts(data, email, now = Date.now()) {
  const renewal = renewalOf(data);
  const rows = signInsFor(data, email);
  if (!renewal || rows.length === 0) return [];
  const facts = needsFor(data, email).map((need) => ({
    label: 'Needs you',
    value: NEED_WORDS[need.reason] || resultWord(need.reason),
    mono: false,
  }));
  facts.push({
    label: 'Sign-in renewal',
    value: renewal.stale
      ? `Stopped: the runner on ${hostWord(renewal.runner)} last reported ${valid(renewal.generatedAt) ? when.format(new Date(renewal.generatedAt)) : 'long ago'}`
      : `Automatic, by the runner on ${hostWord(renewal.runner)}`,
    mono: false,
  });
  const session = (renewal.browserSessions || []).find((b) => b?.email === String(email || '').trim().toLowerCase());
  if (session)
    facts.push({
      label: 'Browser session',
      value:
        session.signedIn === false
          ? 'Signed out: the next renewal needs one CAPTCHA'
          : valid(session.sessionEndsAt)
            ? `Signed in until ${when.format(new Date(session.sessionEndsAt))}`
            : 'Signed in',
      mono: false,
    });
  for (const row of rows)
    facts.push({
      label: `Sign-in · ${hostWord(row.host)}${row.profile === 'default' ? '' : ` ${row.profile}`}`,
      value: rowWords(row, now),
      mono: false,
    });
  return facts;
}

/** The Details note: what exactly to do, in the runner's own words ('' when nothing is needed). */
export function claudeSignInNote(data, email) {
  return needsFor(data, email)
    .map((need) => need.how)
    .filter(Boolean)
    .join(' ');
}

/** The notice the dashboard raises once per new person-only step. */
export function needToasts(data, seen) {
  const out = [];
  for (const need of needsFor(data)) {
    const id = `${need.email}|${need.reason}|${need.since || ''}`;
    if (seen.has(id)) continue;
    seen.add(id);
    out.push({
      kind: 'err',
      title: `Claude sign-in needs you · ${need.email}`,
      body: need.how || resultWord(need.reason),
    });
  }
  return out;
}
