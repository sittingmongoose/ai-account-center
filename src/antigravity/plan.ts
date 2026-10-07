/** Antigravity coding plans, 2026-10-07. Keep labels, policies and model availability here.
 * loadCodeAssist fields/legacy IDs: google-gemini/gemini-cli, packages/core/src/code_assist/types.ts.
 * AAC fixture/history: planInfo.planType = pro (6298e60a, 9cf75fbe); generic tiers are ambiguous.
 */
export interface AntigravityPlan {
  class?: AntigravityPlanClass;
  quotaPolicy?: 'weekly' | 'five-hour-weekly' | 'pooled-7d' | 'metered' | 'none';
  summary?: string;
  models?: string[];
  thirdPartyModels?: boolean;
  creditsOverage?: boolean;
}

/** Private loadCodeAssist metadata; only these bounded display fields are retained. */
export interface AntigravityReportedPlan {
  planType?: string;
  paidTier?: { id?: string; name?: string };
  currentTier?: { id?: string; name?: string };
}

export function cloneAntigravityPlan(plan: AntigravityPlan): AntigravityPlan {
  return { ...plan, ...(plan.models ? { models: [...plan.models] } : {}) };
}

const GEMINI_MODELS = [
  'Gemini 3.8 Flash',
  'Gemini 3.7 Flash',
  'Gemini 3.6 Flash',
  'Gemini 3.1 Pro',
];
const RETIRING_MODELS = ['Claude Sonnet 4.6', 'Claude Opus 4.6', 'GPT-OSS-120B'];
const PAID_CLAUDE_MODELS = ['Claude Sonnet 5.5', 'Claude Opus 5.5'];
const MODEL_REMOVAL_AT = Date.parse('2026-11-02T00:00:00Z');
const DUAL_WINDOW = 'Refreshes every 5 hours up to a weekly limit.';
const NO_QUOTA = 'This plan does not include Antigravity coding quota.';

interface PlanDefinition {
  label: string;
  aliases?: string[];
  quotaPolicy: NonNullable<AntigravityPlan['quotaPolicy']>;
  summary: string;
  models: 'consumer' | 'paid-consumer' | 'ultra-consumer' | 'gemini' | 'none';
  creditsOverage?: boolean;
}

const PLANS = {
  free: {
    label: 'Free',
    aliases: ['free-tier'],
    quotaPolicy: 'weekly',
    summary: 'Weekly quota only; no 5-hour refresh.',
    models: 'consumer',
  },
  plus: {
    label: 'Google AI Plus',
    aliases: ['plus'],
    quotaPolicy: 'weekly',
    summary: 'Weekly quota only; no 5-hour refresh.',
    models: 'consumer',
  },
  'pro-trial': {
    label: 'Google AI Pro (trial)',
    aliases: ['pro-trial', 'Google AI Pro trial'],
    quotaPolicy: 'five-hour-weekly',
    summary: DUAL_WINDOW,
    models: 'consumer',
  },
  pro: {
    label: 'Google AI Pro',
    aliases: ['pro', 'Gemini Code Assist in Google One AI Pro'],
    quotaPolicy: 'five-hour-weekly',
    summary: DUAL_WINDOW,
    models: 'paid-consumer',
    creditsOverage: true,
  },
  ultra: {
    label: 'Google AI Ultra',
    aliases: ['ultra', 'Gemini Code Assist in Google One AI Ultra'],
    quotaPolicy: 'five-hour-weekly',
    summary: DUAL_WINDOW,
    models: 'ultra-consumer',
    creditsOverage: true,
  },
  'ultra-5x': {
    label: 'Google AI Ultra 5x',
    aliases: ['ultra-5x'],
    quotaPolicy: 'five-hour-weekly',
    summary: '5x Google AI Pro capacity; refreshes every 5 hours up to a weekly limit.',
    models: 'ultra-consumer',
    creditsOverage: true,
  },
  'ultra-20x': {
    label: 'Google AI Ultra 20x',
    aliases: ['ultra-20x'],
    quotaPolicy: 'five-hour-weekly',
    summary: '20x Google AI Pro capacity; refreshes every 5 hours up to a weekly limit.',
    models: 'ultra-consumer',
    creditsOverage: true,
  },
  'enterprise-standard': {
    label: 'Gemini Enterprise Standard',
    quotaPolicy: 'pooled-7d',
    summary: 'Gemini models use a rolling 7-day shared project credit pool.',
    models: 'gemini',
  },
  'enterprise-plus': {
    label: 'Gemini Enterprise Plus',
    quotaPolicy: 'pooled-7d',
    summary: 'Gemini models use a rolling 7-day shared project credit pool.',
    models: 'gemini',
  },
  'enterprise-emerging': {
    label: 'Gemini Enterprise Standard Emerging Market',
    quotaPolicy: 'none',
    summary: 'Antigravity is available, but this plan includes no bundled coding quota.',
    models: 'gemini',
  },
  'enterprise-payg': {
    label: 'Gemini Enterprise Pay-as-you-go',
    quotaPolicy: 'metered',
    summary: 'Gemini models use metered Antigravity consumption.',
    models: 'gemini',
  },
  'enterprise-business': {
    label: 'Gemini Enterprise Business',
    quotaPolicy: 'none',
    summary: NO_QUOTA,
    models: 'gemini',
  },
  'code-assist-standard': {
    label: 'Gemini Code Assist Standard',
    quotaPolicy: 'none',
    summary:
      'This plan includes separate Gemini CLI quota that AAC does not read; no Antigravity coding quota.',
    models: 'gemini',
  },
  'code-assist-enterprise': {
    label: 'Gemini Code Assist Enterprise',
    quotaPolicy: 'none',
    summary:
      'This plan includes separate Gemini CLI quota that AAC does not read; no Antigravity coding quota.',
    models: 'gemini',
  },
  workspace: {
    label: 'Google Workspace',
    aliases: [
      'Google Workspace Business Starter',
      'Google Workspace Business Standard',
      'Google Workspace Business Plus',
      'Google Workspace Enterprise Starter',
      'Google Workspace Enterprise Standard',
      'Google Workspace Enterprise Plus',
      'Workspace Business Starter',
      'Workspace Business Standard',
      'Workspace Business Plus',
      'Workspace Enterprise Starter',
      'Workspace Enterprise Standard',
      'Workspace Enterprise Plus',
    ],
    quotaPolicy: 'none',
    summary: NO_QUOTA,
    models: 'none',
  },
} satisfies Record<string, PlanDefinition>;

export type AntigravityPlanClass = keyof typeof PLANS;
const key = (value: string) =>
  value
    .toLowerCase()
    .replace(/[()_-]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
const aliases = new Map<string, AntigravityPlanClass>();
for (const [planClass, definition] of Object.entries(PLANS))
  for (const value of [definition.label, ...('aliases' in definition ? definition.aliases : [])])
    aliases.set(key(value), planClass as AntigravityPlanClass);

function known(value: unknown): AntigravityPlanClass | undefined {
  return typeof value === 'string' ? aliases.get(key(value)) : undefined;
}

/** Unknown tiers (including ambiguous standard-tier/legacy-tier) pass through unchanged. */
export function antigravityPlanDisplay(
  plan: string | null,
  sampledAt: string | number | null,
  reported?: AntigravityReportedPlan
): { plan: string | null; antigravityPlan?: AntigravityPlan } {
  const tier = reported?.paidTier ?? reported?.currentTier;
  const planType = known(reported?.planType);
  const tierClass = known(tier?.id) ?? known(tier?.name);
  let planClass = planType ?? tierClass ?? known(plan);
  // An explicit trial/multiplier refines a generic plan type within that same family.
  if (
    (planClass === 'ultra' && tierClass?.startsWith('ultra-')) ||
    (planClass === 'pro' && tierClass === 'pro-trial')
  )
    planClass = tierClass;
  if (!planClass) return { plan };
  const definition: PlanDefinition = PLANS[planClass];
  const observed =
    typeof sampledAt === 'number' ? sampledAt : sampledAt ? Date.parse(sampledAt) : NaN;
  const beforeRemoval = (Number.isFinite(observed) ? observed : Date.now()) < MODEL_REMOVAL_AT;
  const thirdParty = [
    ...(['paid-consumer', 'ultra-consumer'].includes(definition.models) ? PAID_CLAUDE_MODELS : []),
    ...(beforeRemoval && ['consumer', 'paid-consumer'].includes(definition.models)
      ? RETIRING_MODELS
      : []),
    ...(beforeRemoval && definition.models === 'ultra-consumer' ? ['GPT-OSS-120B'] : []),
  ];
  return {
    plan: definition.label,
    antigravityPlan: {
      class: planClass,
      quotaPolicy: definition.quotaPolicy,
      summary: definition.summary,
      models: [...(definition.models === 'none' ? [] : GEMINI_MODELS), ...thirdParty],
      thirdPartyModels: thirdParty.length > 0,
      creditsOverage: definition.creditsOverage === true,
    },
  };
}
