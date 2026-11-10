// The seam between the dashboard (bridge.js, running in the app frame's hidden iframe `/?content=1`) and the frame's
// native Leptos pages (web-dashboard/leptos/src/dash.rs). bridge.js keeps the network, the session, the timers, the
// accounts controller, switch approvals and every server call; the native pages draw the same versioned view JSON it
// hands Slint and send its action kinds back. Content mode only: the classic app never loads a frame and never
// subscribes, so nothing here runs there. Pure and tested (tests/frame-seam.test.mjs).

/** The views a native page can draw: the same JSON as the Slint exports named beside each. */
export const VIEW_KEYS = Object.freeze([
  'dashboard',          // set_dashboard (view model v2)
  'accounts',           // set_accounts (Accounts & Settings v3)
  'accounts-strength',  // set_accounts_strength
  'details',            // show_details; '' = closed
  'activation',         // show_activation_confirmation; '' = closed
  'update',             // set_update_status
  'busy',               // set_busy: "true" | "false"
  'refresh-interval',   // set_refresh_interval: {"seconds", "confirmed"}
]);

/** Overlays the frame can draw itself. bridge.js draws one in Slint unless the frame said it owns it. */
export const FRAME_OVERLAYS = Object.freeze(['details', 'activation']);

/**
 * The action kinds a native page may run through `window.ccsDashboardAction`. Sign-in, the frame's own commands
 * (navigate, theme, refresh, update-apps, logout, auth-recheck; frame.rs), the canvas's layout reports and the
 * Analytics kinds (Analytics is still drawn by Slint) are not here, so a page can never reach them this way.
 */
export const PAGE_ACTIONS = Object.freeze(new Set([
  // Details and Home rows
  'details', 'details-closed', 'launch',
  // switching: the target-bound approvals stay in activation-confirmation.mjs and antigravity-confirmation.mjs
  'activate', 'antigravity-activate', 'activation-confirm', 'activation-cancel',
  'automatic', 'threshold', 'antigravity-automatic', 'antigravity-threshold', 'antigravity-pool', 'antigravity-cooldown',
  // Accounts & Settings (accounts-controller.mjs handle())
  'accounts-show', 'accounts-tray', 'account-show', 'account-tray',
  'add', 'add-key', 'replace-key', 'signin-again', 'signin', 'session-signin', 'recheck',
  'flow-submit', 'flow-cancel', 'flow-done', 'flow-retry', 'flow-copy', 'flow-open-url', 'flow-open-app', 'flow-recheck',
  'claude-host', 'claude-open',
  'remove', 'refuse', 'line-cancel', 'remove-commit', 'restore', 'restore-commit', 'purge',
  'others-out', 'network-off', 'network-on', 'session-lifetime', 'time-zone',
  'cleanup-auto', 'cleanup-now', 'logsource-add', 'logsource-remove',
  'pw-toggle', 'pw-typing', 'pw-submit', 'device-revoke', 'devices-revoke-all',
  // Settings
  'refresh-interval', 'update-cancel', 'install-app',
]));

/** A message from the frame that framed this page: same origin and the parent window itself, never any other. */
function fromFrame(event, { origin, parent }) {
  return !!event && !!parent && event.origin === origin && event.source === parent;
}

/** The frame's subscription (`{type: 'aac-views', overlays}`): the overlays it draws, or null for any other message. */
export function viewsSubscription(event, where) {
  if (!fromFrame(event, where)) return null;
  const message = event.data;
  if (!message || message.type !== 'aac-views') return null;
  return (Array.isArray(message.overlays) ? message.overlays : []).filter(name => FRAME_OVERLAYS.includes(name));
}

/** A page action (`{type: 'aac-page', kind, value}`): `{ kind, value }` when it may run, else null. */
export function pageAction(event, where) {
  if (!fromFrame(event, where)) return null;
  const message = event.data;
  if (!message || message.type !== 'aac-page' || typeof message.kind !== 'string' || !PAGE_ACTIONS.has(message.kind)) return null;
  return { kind: message.kind, value: typeof message.value === 'string' ? message.value : '' };
}

/**
 * The latest view of each key and the frame's subscription. `post(kind, fields)` sends to the frame (bridge.js
 * tellFrame). Views are kept from the start and sent once the frame subscribes, and all of them again on each
 * subscription, so a page that mounts late still draws the current state.
 */
export function createFrameViews({ post }) {
  const latest = new Map();
  let subscribed = false;
  let overlays = new Set();
  return {
    view(key, json) {
      if (!VIEW_KEYS.includes(key)) return;
      const text = typeof json === 'string' ? json : String(json ?? '');
      latest.set(key, text);
      if (subscribed) post('view', { key, json: text });
    },
    subscribe(list) {
      subscribed = true;
      overlays = new Set((Array.isArray(list) ? list : []).filter(name => FRAME_OVERLAYS.includes(name)));
      for (const [key, json] of latest) post('view', { key, json });
    },
    /** Whether the frame draws this overlay (so Slint must not). */
    owns(overlay) { return subscribed && overlays.has(overlay); },
    latest(key) { return latest.get(key); },
  };
}
