// Cursor's reported plan codes distinguish the X subscriptions from the legacy Cursor plans.
// Display names do not supply quota limits, periods or resets; those come from the usage windows.
const X_PLANS = {
  plus: 'X Plus',
  premium: 'X Premium',
  super: 'X Super',
  unified_ultra: 'X Ultra',
};
const LEGACY_PLANS = {
  free: 'Free',
  pro: 'Pro',
  pro_plus: 'Pro+',
  ultra: 'Cursor Ultra',
  team: 'Team',
  enterprise: 'Enterprise',
};

export function cursorPlanLabel(plan) {
  return Object.hasOwn(X_PLANS, plan)
    ? X_PLANS[plan]
    : Object.hasOwn(LEGACY_PLANS, plan)
      ? LEGACY_PLANS[plan]
      : plan || '';
}

export function cursorPlanNote(account) {
  if (account?.provider !== 'cursor' || !Object.hasOwn(X_PLANS, account.plan)) return '';
  const pool = (Array.isArray(account.windows) ? account.windows : []).find(
    (window) =>
      typeof window?.key === 'string' &&
      window.key.startsWith('grok-shared-') &&
      window.kind !== 'spend' &&
      window.kind !== 'balance'
  );
  const cadence = pool?.windowMinutes === 10080 ? 'Weekly ' : '';
  const models = account.plan === 'plus' ? 'Grok' : 'Grok and Composer';
  const topUps = account.plan === 'plus' ? '' : ' Other models use separately purchased top-ups.';
  return `${cadence}${models} usage is shared across Cursor, Grok and X.${topUps}`;
}
