/** A confirmation token is kept in this closure and is never placed in a UI model. */
export function createActivationConfirmation({ activate, prompt, close, busy, success, error, now = Date.now }) {
  let pending = null;
  let inFlight = false;
  const describe = (confirmation, targetProfile) => {
    if (!confirmation || typeof confirmation.token !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(confirmation.token) || confirmation.targetProfile !== targetProfile || typeof confirmation.expiresAt !== 'string') return null;
    const expiration = Date.parse(confirmation.expiresAt);
    if (!Number.isFinite(expiration)) return null;
    const processes = Array.isArray(confirmation.processes) ? confirmation.processes.flatMap(process =>
      process && typeof process.label === 'string' && Number.isInteger(process.pid) && process.pid > 0
        ? [{ label: process.label.slice(0, 200), pid: process.pid, role: typeof process.role === 'string' ? process.role.slice(0, 80) : '' }] : []) : [];
    return { token: confirmation.token, targetProfile, expiresAt: confirmation.expiresAt, expiration, processes, warning: typeof confirmation.warning === 'string' ? confirmation.warning.slice(0, 2000) : 'Active Codex work will stop. The listed processes will be stopped and Codex will restart with the selected account.' };
  };
  const display = (message = '') => {
    if (!pending) return;
    const expired = pending.expiration <= now();
    prompt({ targetProfile: pending.targetProfile, expiresAt: pending.expiresAt, processes: pending.processes, warning: pending.warning, canConfirm: !expired && !inFlight, inProgress: inFlight, error: message || pending.error || (expired ? 'This confirmation expired. Cancel, then click Activate again.' : '') });
  };
  const attempt = async (targetProfile, body) => {
    inFlight = true;
    busy(true);
    if (pending) display();
    try {
      const result = await activate(targetProfile, body);
      pending = null;
      close();
      await success(result);
      return true;
    } catch (failure) {
      const confirmation = failure?.status === 409 ? describe(failure.payload?.confirmation, targetProfile) : null;
      if (confirmation) {
        pending = confirmation;
      } else if (pending) {
        // A failed single-use approval cannot be retried without a fresh review.
        pending.expiration = 0;
        pending.error = failure?.message || 'The switch could not complete. Cancel and try Activate again.';
        display();
      } else error(failure?.message || 'Unable to activate the Codex account.');
      return false;
    } finally {
      inFlight = false;
      busy(false);
      if (pending) display();
    }
  };
  return {
    begin(targetProfile) {
      if (inFlight || pending) return Promise.resolve(false);
      return attempt(targetProfile, {});
    },
    confirm() {
      if (inFlight || !pending) return Promise.resolve(false);
      if (pending.expiration <= now()) { display(); return Promise.resolve(false); }
      return attempt(pending.targetProfile, { confirmationToken: pending.token });
    },
    cancel() {
      if (inFlight) return false;
      pending = null;
      close();
      return true;
    },
    expire() { if (pending && !inFlight && pending.expiration <= now()) display(); },
    hasPending() { return pending !== null; },
  };
}
