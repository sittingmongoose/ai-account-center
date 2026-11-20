/** Antigravity coding plans, 2026-11-19. Keep labels, policies and model availability here.
 * Source: antigravity.google/docs/models, /docs/plans, /docs/workspace and /docs/enterprise, read
 * 2026-11-19; the dated table with sources is RESEARCH.md in the 2026-11-19-plan-tiers share folder.
 * loadCodeAssist fields/legacy IDs: google-gemini/gemini-cli, packages/core/src/code_assist/types.ts.
 * AAC fixture/history: planInfo.planType = pro (6298e60a, 9cf75fbe); generic tiers are ambiguous.
 * The plan always comes from the latest successful reading; nothing here is a configured label.
 * Quota windows are never taken from this table: they are what Google reports on each read.
 * 2026-11-13 quota reads: Google AI Pro lists Gemini 4 Argon with its own weekly bucket and
 * Gemini 4 Hydrogen in the Gemini Models windows (same reset and fraction); the free tier,
 * now named "Antigravity Starter Quota", lists neither.
 * 2026-11-18: Gemini 4 Neon and Orchestrator (Neon) launched on paid Pro, Ultra and Enterprise.
 * Claude Sonnet/Opus 4.6 and GPT-OSS-120b: the docs still say "removed on November 18", but the
 * Free account reports its Claude and GPT bucket on Nov 19 and users report a move to Nov 30, so
 * they are listed until the end of Nov 30.
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

const FLASH_MODELS = ['Gemini 3.8 Flash'];
/** Paid Google AI Pro (not a trial): docs/models, 2026-11-19. */
const PRO_GEMINI_MODELS = [
  'Gemini 4 Neon',
  'Gemini 4 Argon',
  'Gemini 4 Hydrogen',
  'Orchestrator (Neon)',
  'Orchestrator (Argon)',
  'Gemini 3.8 Flash',
];
const ULTRA_GEMINI_MODELS = [
  'Gemini 4 Neon',
  'Gemini 4 Carbon',
  'Gemini 4 Argon',
  'Gemini 4 Hydrogen',
  'Orchestrator (Neon)',
  'Orchestrator (Carbon)',
  'Orchestrator (Argon)',
  'Gemini 3.8 Flash',
];
const ENTERPRISE_GEMINI_MODELS = [
  'Gemini 4 Neon',
  'Gemini 4 Carbon',
  'Gemini 4 Argon',
  'Gemini 4 Hydrogen',
  'Gemini 3.8 Flash',
  'Gemini 3.7 Flash',
  'Gemini 3.6 Flash',
  'Gemini 3.1 Pro',
];
const RETIRING_MODELS = ['Claude Sonnet 4.6', 'Claude Opus 4.6', 'GPT-OSS-120B'];
const PAID_CLAUDE_MODELS = ['Claude Sonnet 5.5', 'Claude Opus 5.5'];
const MODEL_REMOVAL_AT = Date.parse('2026-12-01T00:00:00Z');
const DUAL_WINDOW = 'Refreshes every 5 hours up to a weekly limit.';
const STARTER = 'Weekly quota only; no 5-hour refresh.';
// Google keeps separate Antigravity limits for the CLI that T3 runs and for the Antigravity app (2026-11-19).
const SEPARATE = 'T3 (the Antigravity CLI) and the Antigravity app have separate limits.';
const POOLS =
  'Gemini models share one limit that costlier models use up faster (Gemini 4 Neon counts against it); Claude and GPT models have their own.';
const PRO_POOLS =
  'Gemini models share one limit that costlier models use up faster (Gemini 4 Neon and Hydrogen count against it); Gemini 4 Argon has its own weekly limit; Claude and GPT models have their own.';
const TRIAL =
  'Gemini 4 Neon, Gemini 4 Argon and their Orchestrators only on trials with eligible premium devices; no Gemini 4 Hydrogen or Claude 5.5 during a trial.';
const POOLED =
  'Gemini models use pooled edition seat credits; Claude models need administrator opt-in.';
const NO_QUOTA = 'This plan does not include Antigravity coding quota.';

interface PlanDefinition {
  label: string;
  aliases?: string[];
  quotaPolicy: NonNullable<AntigravityPlan['quotaPolicy']>;
  summary: string;
  models: 'consumer' | 'trial' | 'paid-consumer' | 'ultra-consumer' | 'gemini' | 'flash';
  creditsOverage?: boolean;
}

const PLANS = {
  free: {
    label: 'Free',
    aliases: ['free-tier', 'Antigravity Starter Quota'],
    quotaPolicy: 'weekly',
    summary: `${STARTER} ${SEPARATE}`,
    models: 'consumer',
  },
  plus: {
    label: 'Google AI Plus',
    aliases: ['plus'],
    quotaPolicy: 'weekly',
    summary: `${STARTER} ${SEPARATE}`,
    models: 'consumer',
  },
  'pro-trial': {
    label: 'Google AI Pro (trial)',
    aliases: ['pro-trial', 'Google AI Pro trial'],
    quotaPolicy: 'five-hour-weekly',
    summary: `${DUAL_WINDOW} ${TRIAL} ${SEPARATE}`,
    models: 'trial',
  },
  pro: {
    label: 'Google AI Pro',
    aliases: ['pro', 'Gemini Code Assist in Google One AI Pro'],
    quotaPolicy: 'five-hour-weekly',
    summary: `${DUAL_WINDOW} ${PRO_POOLS} ${SEPARATE}`,
    models: 'paid-consumer',
    creditsOverage: true,
  },
  ultra: {
    label: 'Google AI Ultra',
    aliases: ['ultra', 'Gemini Code Assist in Google One AI Ultra'],
    quotaPolicy: 'five-hour-weekly',
    summary: `${DUAL_WINDOW} ${POOLS} ${SEPARATE}`,
    models: 'ultra-consumer',
    creditsOverage: true,
  },
  'ultra-5x': {
    label: 'Google AI Ultra 5x',
    aliases: ['ultra-5x'],
    quotaPolicy: 'five-hour-weekly',
    summary: `5x Google AI Pro capacity; refreshes every 5 hours up to a weekly limit. ${POOLS} ${SEPARATE}`,
    models: 'ultra-consumer',
    creditsOverage: true,
  },
  'ultra-20x': {
    label: 'Google AI Ultra 20x',
    aliases: ['ultra-20x'],
    quotaPolicy: 'five-hour-weekly',
    summary: `20x Google AI Pro capacity; refreshes every 5 hours up to a weekly limit. ${POOLS} ${SEPARATE}`,
    models: 'ultra-consumer',
    creditsOverage: true,
  },
  'enterprise-standard': {
    label: 'Gemini Enterprise Standard',
    quotaPolicy: 'pooled-7d',
    summary: POOLED,
    models: 'gemini',
  },
  'enterprise-plus': {
    label: 'Gemini Enterprise Plus',
    quotaPolicy: 'pooled-7d',
    summary: POOLED,
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
    models: 'flash',
  },
  'code-assist-enterprise': {
    label: 'Gemini Code Assist Enterprise',
    quotaPolicy: 'none',
    summary:
      'This plan includes separate Gemini CLI quota that AAC does not read; no Antigravity coding quota.',
    models: 'flash',
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
    quotaPolicy: 'weekly',
    summary: `Antigravity Starter Quota: ${STARTER} Workspace plans add no higher limits or AI credits. ${SEPARATE}`,
    models: 'consumer',
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
    ...(beforeRemoval && ['consumer', 'trial', 'paid-consumer'].includes(definition.models)
      ? RETIRING_MODELS
      : []),
    ...(beforeRemoval && definition.models === 'ultra-consumer' ? ['GPT-OSS-120B'] : []),
  ];
  const gemini =
    definition.models === 'paid-consumer'
      ? PRO_GEMINI_MODELS
      : definition.models === 'ultra-consumer'
        ? ULTRA_GEMINI_MODELS
        : definition.models === 'gemini'
          ? ENTERPRISE_GEMINI_MODELS
          : FLASH_MODELS;
  return {
    plan: definition.label,
    antigravityPlan: {
      class: planClass,
      quotaPolicy: definition.quotaPolicy,
      summary: definition.summary,
      models: [...gemini, ...thirdParty],
      thirdPartyModels: thirdParty.length > 0,
      creditsOverage: definition.creditsOverage === true,
    },
  };
}
