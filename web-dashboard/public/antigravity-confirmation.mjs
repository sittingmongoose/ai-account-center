const HOST = 'ubuntu';
const PRODUCT = 'Antigravity';
const PROCESS_LABELS = Object.freeze({
  cli: 'Antigravity CLI',
  desktop: 'Antigravity Desktop',
  'language-server': 'Antigravity language server',
});
const WARNING = 'Another Antigravity program is running on Ubuntu. Stop the listed programs, switch accounts, and restore their reviewed sessions?';
const EXPIRED = 'This confirmation expired. Cancel, then click Activate again.';
const FAILED = 'The switch could not complete. Cancel, then click Activate again.';
const ACTIVATION_MESSAGES = Object.freeze({
  busy: 'Antigravity is busy. The switch waits until its programs are idle.',
  deferred: 'Antigravity activation is deferred on Ubuntu. Refresh its native status before trying again.',
  'unsupported-runtime-probe': 'The Ubuntu Antigravity runtime could not be verified. Account switching is unavailable.',
  'stale-confirmation': 'This Antigravity confirmation is no longer valid. Cancel, then click Activate again.',
  'failed-rolled-back': 'Antigravity could not switch accounts on Ubuntu. The previous state was restored.',
  'recovery-required': 'Antigravity activation needs recovery on Ubuntu. Account switching is unavailable.',
});

const record = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const profileId = value => typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(value);
const token = value => typeof value === 'string' && /^[A-Za-z0-9_-]{16,256}$/.test(value);
function displayText(value, maximum) {
  if (typeof value !== 'string' || value.length > maximum || /[\u0000-\u001f\u007f]/.test(value)
      || /(?:bearer\s|dca:|sk[-_]|eyJ[A-Za-z0-9_-]{8}|https?:\/\/|[{}]|access_token|refresh_token|client_secret)/i.test(value)) return null;
  return value.trim() || null;
}
function email(value) {
  const candidate = displayText(value, 254);
  return candidate && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(candidate) ? candidate : null;
}
function expiration(value) {
  if (typeof value !== 'string' || value.length > 40
      || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(value)) return null;
  const parsed = new Date(value);
  if (!Number.isFinite(parsed.getTime()) || parsed.getUTCFullYear() < 2000 || parsed.getUTCFullYear() > 2200) return null;
  return { expiresAt: parsed.toISOString(), expiration: parsed.getTime() };
}
function failureMessage(failure, fallback, privateToken) {
  let message = typeof failure?.message === 'string' ? failure.message : '';
  if (/^Request failed \(\d{3}\)\.$/.test(message) && typeof failure?.payload?.status === 'string'
      && Object.hasOwn(ACTIVATION_MESSAGES, failure.payload.status)) message = ACTIVATION_MESSAGES[failure.payload.status];
  for (const candidate of [privateToken, failure?.payload?.confirmation?.token]) {
    if (typeof candidate === 'string' && candidate) message = message.split(candidate).join('[redacted]');
  }
  return displayText(message, 500) || fallback;
}

/** Tokens stay inside this controller; only a separate affirmative request consumes one. */
export function createAntigravityConfirmation({ activate, confirm: approve, prompt, close, busy, success, error, now = Date.now }) {
  let pending = null;
  let inFlight = false;

  const describe = (failure, target) => {
    const payload = failure?.payload;
    const offer = payload?.confirmation;
    if (failure?.status !== 409 || !record(payload) || payload.status !== 'confirmation-required'
        || payload.profileId !== target || payload.hostId !== HOST || !record(offer)
        || offer.profileId !== target || offer.hostId !== HOST || !token(offer.token)) return null;
    const identity = email(payload.email);
    const nestedIdentity = email(offer.email);
    const expiry = expiration(offer.expiresAt);
    if (!identity || identity !== nestedIdentity || !expiry || expiry.expiration <= now()
        || identity.includes(offer.token) || target.includes(offer.token)
        || !Array.isArray(offer.processes) || offer.processes.length > 32) return null;
    const processes = [];
    for (const process of offer.processes) {
      if (!record(process) || !Number.isInteger(process.pid) || process.pid < 1 || process.pid > 2_147_483_647
          || typeof process.role !== 'string' || !Object.hasOwn(PROCESS_LABELS, process.role)) return null;
      processes.push({ pid: process.pid, role: process.role, label: PROCESS_LABELS[process.role] });
    }
    return { token: offer.token, profileId: target, email: identity, ...expiry, processes, valid: true, error: '' };
  };
  const display = () => {
    if (!pending) return;
    const expired = pending.expiration <= now();
    prompt({
      product: PRODUCT, targetProfile: pending.email, profileId: pending.profileId,
      expiresAt: pending.expiresAt,
      processes: pending.processes.map(process => ({ ...process })),
      warning: WARNING, canConfirm: pending.valid && !expired && !inFlight,
      inProgress: inFlight, error: pending.error || (expired ? EXPIRED : ''),
    });
  };
  const accepted = (result, target, reviewedEmail) => {
    if (!record(result) || !['active', 'already-active'].includes(result.status)
        || result.profileId !== target || result.hostId !== HOST) return null;
    const identity = result.email === undefined ? null : email(result.email);
    if ((result.email !== undefined && !identity) || (reviewedEmail && identity && identity !== reviewedEmail)) return null;
    return { status: result.status, profileId: target, hostId: HOST, ...(identity ? { email: identity } : {}) };
  };
  const attempt = async (target, approval = null) => {
    inFlight = true;
    try {
      busy(true);
      if (pending) display();
      const result = approval
        ? await approve(target, { hostId: HOST, confirmationToken: approval.token })
        : await activate(target, { hostId: HOST });
      const completed = accepted(result, target, approval?.email);
      if (!completed) throw new Error('Invalid Antigravity activation response.');
      pending = null;
      close();
      await success(completed);
      return true;
    } catch (failure) {
      if (approval) {
        // Never replace a consumed approval with a new offer or retry it automatically.
        if (pending) {
          pending.valid = false;
          pending.token = null;
          pending.error = failureMessage(failure, FAILED, approval.token);
        } else error(failureMessage(failure, FAILED, approval.token));
      } else {
        const offer = describe(failure, target);
        if (offer) pending = offer;
        else error(failureMessage(failure, 'Unable to activate the Antigravity account.'));
      }
      return false;
    } finally {
      inFlight = false;
      busy(false);
      if (pending) display();
    }
  };
  return {
    begin(target) {
      if (inFlight || pending) return Promise.resolve(false);
      if (!profileId(target)) {
        error('Invalid Antigravity profile.');
        return Promise.resolve(false);
      }
      return attempt(target);
    },
    confirm() {
      if (inFlight || !pending || !pending.valid) return Promise.resolve(false);
      if (pending.expiration <= now()) {
        pending.valid = false;
        pending.token = null;
        display();
        return Promise.resolve(false);
      }
      const approval = { token: pending.token, email: pending.email };
      pending.valid = false;
      pending.token = null;
      return attempt(pending.profileId, approval);
    },
    cancel() {
      if (inFlight) return false;
      pending = null;
      close();
      return true;
    },
    expire() {
      if (pending && !inFlight && pending.expiration <= now()) {
        pending.valid = false;
        pending.token = null;
        display();
      }
    },
    hasPending() { return pending !== null; },
  };
}
