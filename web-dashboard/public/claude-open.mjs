// Claude "Open on Mac / Windows" progress (CONTRACT-serving-misc 4.4). Pure state plus a small poller with
// injected I/O, so tests/claude-open.test.mjs drives the 202 path with fixtures and a fake clock.
//
// The POST asks for the asynchronous answer with `Prefer: respond-async`:
// - 200 `{ opened, id, platform }` is the finished Open, as before;
// - 202 `{ id, platform, state, operationId }` starts the progress: GET /api/claude/desktop-profiles is read
//   every 1 s for 120 s, then every 5 s, until that operation's `state` is opened, failed or blocked_uncertain.
// The POST is never sent again by the poller, and nothing resumes after a reload or a server restart: an
// operation the server no longer reports ends the progress with a sentence that says so.

export const OPEN_TERMINAL = Object.freeze(['opened', 'failed', 'blocked_uncertain']);
const STATES = new Set(['checking', 'copying', 'opening', ...OPEN_TERMINAL]);
export const FAST_POLL_MS = 1_000;
export const SLOW_POLL_MS = 5_000;
export const FAST_FOR_MS = 120_000;
/** The poller gives up here; a running Open still shows from the regular refresh of the profile list. */
export const POLL_LIMIT_MS = 10 * 60_000;
/** How long a finished Open keeps its last line on the row. */
export const HOLD_OPENED_MS = 6_000;
export const HOLD_FAILED_MS = 12_000;

export const OPEN_SENTENCES = Object.freeze({
  failed: 'Claude account could not be opened safely.',
  blocked_uncertain: 'Claude history copy is unconfirmed. Verify it has stopped before opening this profile.',
  lost: 'The dashboard no longer reports this Open. Check Claude before opening it again.',
  timeout: 'Claude is still opening. Its progress shows again on the next refresh.',
});

const count = value => Number.isInteger(value) && value >= 0 ? value : null;
const PLATFORM = { mac: 'Mac', windows: 'Windows' };

/** A reported operation, checked field by field; anything malformed is treated as not reported. */
export function validOperation(op) {
  if (!op || typeof op !== 'object' || typeof op.id !== 'string' || !op.id) return null;
  if (!STATES.has(op.state) || !(op.platform in PLATFORM)) return null;
  return {
    id: op.id, platform: op.platform, state: op.state,
    confirmedCount: count(op.confirmedCount), totalCount: count(op.totalCount),
    message: typeof op.message === 'string' && op.message ? op.message : null,
  };
}

export const isTerminal = state => OPEN_TERMINAL.includes(state);

/** The reported operation of one profile in a GET /api/claude/desktop-profiles answer. */
export function operationOf(payload, profileId) {
  const profiles = Array.isArray(payload?.profiles) ? payload.profiles : [];
  const profile = profiles.find(row => row && row.id === profileId);
  return profile ? validOperation(profile.openOperation) : null;
}

/** The row's line for an operation: "Copying history 3 of 18", "Opening on Mac", "Opened on Mac", or the sentence. */
export function openProgressText(op) {
  const where = PLATFORM[op?.platform] ? ` on ${PLATFORM[op.platform]}` : '';
  switch (op?.state) {
    case 'checking': return `Checking history${where}`;
    case 'copying':
      return op.totalCount !== null && op.confirmedCount !== null
        ? `Copying history ${Math.min(op.confirmedCount, op.totalCount)} of ${op.totalCount}`
        : 'Copying history';
    case 'opening': return `Opening${where}`;
    case 'opened': return `Opened${where}`;
    case 'failed': case 'blocked_uncertain': return op.message || OPEN_SENTENCES[op.state];
    case 'lost': case 'timeout': return OPEN_SENTENCES[op.state];
    default: return '';
  }
}

/** 1 s for the first 120 s, then 5 s; null once the poller should stop. */
export function pollDelay(elapsedMs) {
  if (!(elapsedMs >= 0) || elapsedMs >= POLL_LIMIT_MS) return null;
  return elapsedMs < FAST_FOR_MS ? FAST_POLL_MS : SLOW_POLL_MS;
}

/**
 * One controller per page.
 * deps: post(id, platform) -> { status, body } (throws on a refusal), list() -> profiles payload,
 *       schedule(fn, ms) -> handle, cancel(handle), clock() -> ms, changed() (redraw), finished(id, view).
 */
export function createClaudeOpen({ post, list, schedule = setTimeout, cancel = clearTimeout, clock = Date.now, changed = () => {}, finished = () => {} }) {
  const entries = new Map();
  const set = (id, entry) => { entries.set(id, entry); changed(); };
  const clearLater = (id, entry, ms) => {
    entry.timer = schedule(() => { if (entries.get(id) === entry) { entries.delete(id); changed(); } }, ms);
  };
  const end = (id, entry, op) => {
    entry.op = op; entry.done = true;
    changed();
    finished(id, { platform: entry.platform, state: op.state, text: openProgressText(op) });
    clearLater(id, entry, op.state === 'opened' ? HOLD_OPENED_MS : HOLD_FAILED_MS);
  };
  const tick = async (id, entry) => {
    if (entries.get(id) !== entry || entry.done) return;
    let payload = null;
    try { payload = await list(); } catch { payload = null; }
    if (entries.get(id) !== entry || entry.done) return;
    if (payload) {
      const reported = operationOf(payload, id);
      if (reported && reported.id === entry.operationId) {
        const moved = !entry.op || reported.state !== entry.op.state || reported.confirmedCount !== entry.op.confirmedCount || reported.totalCount !== entry.op.totalCount;
        if (isTerminal(reported.state)) { end(id, entry, reported); return; }
        entry.op = reported;
        if (moved) changed();
      } else if (!reported) {
        // The server no longer has it (a restart, or it aged out): never resumed, never posted again.
        end(id, entry, { ...entry.op, state: 'lost', message: null });
        return;
      }
      // Another operation of the same profile is reported first: keep waiting for ours.
    }
    const delay = pollDelay(clock() - entry.startedAt);
    if (delay === null) { end(id, entry, { ...entry.op, state: 'timeout', message: null }); return; }
    entry.timer = schedule(() => { void tick(id, entry); }, delay);
  };

  const api = {
    /** True while an Open started here has not finished. */
    running(id) { const entry = entries.get(id); return !!entry && !entry.done; },
    /** profile id -> { platform, state, text, done } for every Open started here that is still shown. */
    views() {
      const out = new Map();
      for (const [id, entry] of entries) if (entry.op) out.set(id, { platform: entry.platform, state: entry.op.state, text: openProgressText(entry.op), done: entry.done });
      return out;
    },
    /**
     * Sends the one POST. Answers 'opened' (200), 'started' (202, the poller runs) or 'running' when an Open
     * of this profile is already being followed here (nothing is sent).
     */
    async start(id, platform) {
      if (api.running(id)) return 'running';
      const previous = entries.get(id);
      if (previous) { cancel(previous.timer); entries.delete(id); }
      const { status, body } = await post(id, platform);
      const first = status === 202 ? validOperation({ id: body?.operationId, platform: body?.platform, state: body?.state }) : null;
      if (!first || isTerminal(first.state)) {
        if (status === 202) changed();
        return 'opened';
      }
      const entry = { platform: first.platform, operationId: first.id, op: { ...first, confirmedCount: null, totalCount: null, message: null }, startedAt: clock(), done: false, timer: null };
      set(id, entry);
      entry.timer = schedule(() => { void tick(id, entry); }, FAST_POLL_MS);
      return 'started';
    },
    /** Stops following everything (sign-out). Nothing is sent. */
    reset() { for (const entry of entries.values()) cancel(entry.timer); entries.clear(); changed(); },
  };
  return api;
}

/**
 * The rows' progress: Opens followed here first, then running Opens the regular profile refresh reports
 * (started by a tray or another browser). A finished operation the server still remembers is not shown,
 * so an old failure never reappears after a reload.
 */
export function openProgress(controllerViews, profiles) {
  const out = new Map(controllerViews instanceof Map ? controllerViews : []);
  for (const profile of Array.isArray(profiles) ? profiles : []) {
    if (!profile || typeof profile.id !== 'string' || out.has(profile.id)) continue;
    const op = validOperation(profile.openOperation);
    if (op && !isTerminal(op.state)) out.set(profile.id, { platform: op.platform, state: op.state, text: openProgressText(op), done: false });
  }
  return out;
}
