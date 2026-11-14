// The action kinds the app frame's native pages may run (leptos/src/dash.rs, leptos/src/analytics/feed.rs). They began
// as the seam between bridge.js, which ran the dashboard in the frame's hidden iframe, and the frame; since G5 the
// frame's own controllers (frame-dashboard.mjs, frame-analytics.mjs) run them, and the classic Slint app with bridge.js
// is retired (Leptos finish, step 9). Pure and tested (tests/frame-seam.test.mjs).

/**
 * The action kinds the app frame's pages may run (dash.rs `act`, analytics/feed.rs): the dashboard controller runs the
 * rest (frame-dashboard.mjs DASHBOARD_ACTIONS), the Analytics controller the `analytics-` ones. Sign-in, the frame's
 * own commands (navigate, theme, refresh, update-apps, logout, auth-recheck; frame.rs; analytics-refresh is the same
 * as refresh) and the retired Slint canvas's reports are not here, so a page can never reach them this way.
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
  // Analytics (frame-analytics.mjs, analytics-page.mjs). analytics-layout reports the chart boxes
  // ("trend|daily|heat|focus,w,h" or "heat-cell,w"); the charts are laid out in those pixels.
  'analytics-range', 'analytics-custom', 'analytics-provider', 'analytics-split', 'analytics-cache',
  'analytics-donut', 'analytics-cbm-sort', 'analytics-donut-open', 'analytics-heat',
  'analytics-focus', 'analytics-compare', 'analytics-group', 'analytics-layout',
]));
