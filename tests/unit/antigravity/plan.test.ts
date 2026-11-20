import { describe, expect, it } from 'bun:test';
import { antigravityPlanDisplay } from '../../../src/antigravity/plan';

const BEFORE = '2026-11-30T23:59:59Z';
const AFTER = '2026-12-01T00:00:00Z';
const FLASH = ['Gemini 3.8 Flash'];
const PRO = [
  'Gemini 4 Neon',
  'Gemini 4 Argon',
  'Gemini 4 Hydrogen',
  'Orchestrator (Neon)',
  'Orchestrator (Argon)',
  'Gemini 3.8 Flash',
];
const ULTRA = [
  'Gemini 4 Neon',
  'Gemini 4 Carbon',
  'Gemini 4 Argon',
  'Gemini 4 Hydrogen',
  'Orchestrator (Neon)',
  'Orchestrator (Carbon)',
  'Orchestrator (Argon)',
  'Gemini 3.8 Flash',
];
const ENTERPRISE = [
  'Gemini 4 Neon',
  'Gemini 4 Carbon',
  'Gemini 4 Argon',
  'Gemini 4 Hydrogen',
  'Gemini 3.8 Flash',
  'Gemini 3.7 Flash',
  'Gemini 3.6 Flash',
  'Gemini 3.1 Pro',
];
const RETIRING = ['Claude Sonnet 4.6', 'Claude Opus 4.6', 'GPT-OSS-120B'];
const CLAUDE = ['Claude Sonnet 5.5', 'Claude Opus 5.5'];

describe('Antigravity plan table dated 2026-11-19', () => {
  it.each([
    ['free-tier', 'Free', 'free', 'weekly'],
    ['GOOGLE_AI_PLUS', 'Google AI Plus', 'plus', 'weekly'],
    ['pro', 'Google AI Pro', 'pro', 'five-hour-weekly'],
    ['pro-trial', 'Google AI Pro (trial)', 'pro-trial', 'five-hour-weekly'],
    ['ultra', 'Google AI Ultra', 'ultra', 'five-hour-weekly'],
    ['ultra-5x', 'Google AI Ultra 5x', 'ultra-5x', 'five-hour-weekly'],
    ['GOOGLE_AI_ULTRA_20X', 'Google AI Ultra 20x', 'ultra-20x', 'five-hour-weekly'],
    [
      'Gemini Enterprise Standard',
      'Gemini Enterprise Standard',
      'enterprise-standard',
      'pooled-7d',
    ],
    ['Gemini Enterprise Plus', 'Gemini Enterprise Plus', 'enterprise-plus', 'pooled-7d'],
    [
      'Gemini Enterprise Standard Emerging Market',
      'Gemini Enterprise Standard Emerging Market',
      'enterprise-emerging',
      'none',
    ],
    [
      'Gemini Enterprise Pay-as-you-go',
      'Gemini Enterprise Pay-as-you-go',
      'enterprise-payg',
      'metered',
    ],
    ['Gemini Enterprise Business', 'Gemini Enterprise Business', 'enterprise-business', 'none'],
    ['Gemini Code Assist Standard', 'Gemini Code Assist Standard', 'code-assist-standard', 'none'],
    [
      'Gemini Code Assist Enterprise',
      'Gemini Code Assist Enterprise',
      'code-assist-enterprise',
      'none',
    ],
    ['Google Workspace Business Standard', 'Google Workspace', 'workspace', 'weekly'],
  ])('normalizes %s to %s', (raw, label, planClass, quotaPolicy) => {
    const result = antigravityPlanDisplay(raw, BEFORE);
    expect(result.plan).toBe(label);
    expect(result.antigravityPlan).toMatchObject({ class: planClass, quotaPolicy });
  });

  it('uses explicit tier ids/names and refines trial/multiplier without treating generic tiers as plans', () => {
    expect(
      antigravityPlanDisplay('Standard', BEFORE, {
        planType: 'ULTRA',
        paidTier: { id: 'GOOGLE_AI_ULTRA_20X', name: 'Standard' },
        currentTier: { id: 'free-tier' },
      }).plan
    ).toBe('Google AI Ultra 20x');
    expect(
      antigravityPlanDisplay('Standard', BEFORE, {
        planType: 'PRO',
        paidTier: { id: 'standard-tier', name: 'Google AI Pro (trial)' },
      }).antigravityPlan?.class
    ).toBe('pro-trial');
    expect(antigravityPlanDisplay('Gemini Code Assist in Google One AI Pro', BEFORE).plan).toBe(
      'Google AI Pro'
    );
    expect(antigravityPlanDisplay('Gemini Code Assist in Google One AI Ultra', BEFORE).plan).toBe(
      'Google AI Ultra'
    );
    for (const raw of ['standard-tier', 'legacy-tier', 'premium-tier', 'Future Google plan', null])
      expect(antigravityPlanDisplay(raw, BEFORE)).toEqual({ plan: raw });
  });

  it("lists each tier's models from the 2026-11-19 model table (Neon launched Nov 18)", () => {
    const models = (plan: string, at = BEFORE) => antigravityPlanDisplay(plan, at).antigravityPlan;
    for (const plan of ['free', 'plus', 'Google Workspace'])
      expect(models(plan)).toMatchObject({
        models: [...FLASH, ...RETIRING],
        thirdPartyModels: true,
        creditsOverage: false,
        quotaPolicy: 'weekly',
      });
    expect(models('pro-trial')).toMatchObject({
      models: [...FLASH, ...RETIRING],
      quotaPolicy: 'five-hour-weekly',
      creditsOverage: false,
    });
    expect(models('pro-trial')?.summary).toContain('eligible premium devices');
    expect(models('pro')).toMatchObject({
      models: [...PRO, ...CLAUDE, ...RETIRING],
      thirdPartyModels: true,
      creditsOverage: true,
    });
    for (const plan of ['ultra', 'ultra-5x', 'ultra-20x'])
      expect(models(plan)).toMatchObject({
        models: [...ULTRA, ...CLAUDE, 'GPT-OSS-120B'],
        thirdPartyModels: true,
        creditsOverage: true,
        quotaPolicy: 'five-hour-weekly',
      });
    for (const plan of ['free', 'plus', 'pro-trial', 'pro'])
      expect(models(plan)?.models).not.toContain('Gemini 4 Carbon');
    for (const plan of ['free', 'plus', 'pro-trial'])
      expect(models(plan)?.models).not.toContain('Gemini 4 Neon');
    expect(models('ultra')?.summary).toContain('Claude and GPT models have their own');
    expect(models('pro')?.summary).toContain('Gemini 4 Argon has its own weekly limit');
  });

  it('says Gemini 4 Neon draws from the Gemini pool and that T3 and the app have separate limits (2026-11-19)', () => {
    const models = (plan: string) => antigravityPlanDisplay(plan, BEFORE).antigravityPlan;
    for (const plan of ['pro', 'ultra', 'ultra-5x', 'ultra-20x'])
      expect(models(plan)?.summary).toMatch(/Gemini 4 Neon (and Hydrogen count|counts) against it/);
    for (const plan of [
      'free',
      'plus',
      'pro-trial',
      'pro',
      'ultra',
      'ultra-5x',
      'ultra-20x',
      'Google Workspace',
    ])
      expect(models(plan)?.summary).toEndWith(
        'T3 (the Antigravity CLI) and the Antigravity app have separate limits.'
      );
    for (const plan of ['enterprise-standard', 'enterprise-payg', 'code-assist-standard'])
      expect(models(plan)?.summary ?? '').not.toContain('separate limits');
  });

  it('drops Claude 4.6 and GPT-OSS-120B at the end of Nov 30, by the sample time', () => {
    for (const plan of ['free', 'plus', 'pro-trial', 'Google Workspace']) {
      const after = antigravityPlanDisplay(plan, AFTER).antigravityPlan;
      expect(after?.models).toEqual(FLASH);
      expect(after?.thirdPartyModels).toBe(false);
    }
    expect(antigravityPlanDisplay('pro', AFTER).antigravityPlan?.models).toEqual([
      ...PRO,
      ...CLAUDE,
    ]);
    for (const plan of ['ultra', 'ultra-5x', 'ultra-20x']) {
      expect(antigravityPlanDisplay(plan, AFTER).antigravityPlan?.models).toEqual([
        ...ULTRA,
        ...CLAUDE,
      ]);
      expect(antigravityPlanDisplay(plan, BEFORE).antigravityPlan?.models).not.toContain(
        'Claude Sonnet 4.6'
      );
    }
  });

  it('reads the free tier by its current Google name and shows no plan for an empty tier', () => {
    const free = antigravityPlanDisplay(null, AFTER, {
      paidTier: { id: 'free-tier', name: 'Antigravity Starter Quota' },
      currentTier: { id: 'free-tier', name: 'Antigravity' },
    });
    expect(free.plan).toBe('Free');
    expect(free.antigravityPlan).toMatchObject({ class: 'free', quotaPolicy: 'weekly' });
    expect(antigravityPlanDisplay('Antigravity Starter Quota', AFTER).plan).toBe('Free');
    const pro = antigravityPlanDisplay(null, AFTER, {
      paidTier: { id: 'g1-pro-tier', name: 'Google AI Pro' },
    });
    expect(pro.plan).toBe('Google AI Pro');
    // Reported on some Ultra accounts since 2026-11-18: an empty tier is unknown, never guessed.
    expect(
      antigravityPlanDisplay(null, AFTER, {
        paidTier: { id: '', name: '' },
        currentTier: { id: '', name: '' },
      })
    ).toEqual({ plan: null });
  });

  it('follows an upgrade between two reads with no configuration change', () => {
    const first = antigravityPlanDisplay(null, BEFORE, {
      paidTier: { id: 'free-tier', name: 'Antigravity Starter Quota' },
    });
    const second = antigravityPlanDisplay(null, BEFORE, {
      planType: 'pro',
      paidTier: { id: 'g1-pro-tier', name: 'Google AI Pro' },
    });
    const third = antigravityPlanDisplay(null, BEFORE, { planType: 'ULTRA' });
    expect([first.plan, second.plan, third.plan]).toEqual([
      'Free',
      'Google AI Pro',
      'Google AI Ultra',
    ]);
    expect(first.antigravityPlan?.quotaPolicy).toBe('weekly');
    expect(second.antigravityPlan?.quotaPolicy).toBe('five-hour-weekly');
    expect(second.antigravityPlan?.models).toContain('Gemini 4 Neon');
    expect(third.antigravityPlan?.models).toContain('Gemini 4 Carbon');
  });

  it('keeps enterprise on its Gemini list, Code Assist quota separate and model arrays independent', () => {
    for (const plan of [
      'Gemini Enterprise Standard',
      'Gemini Enterprise Plus',
      'Gemini Enterprise Standard Emerging Market',
      'Gemini Enterprise Pay-as-you-go',
      'Gemini Enterprise Business',
    ]) {
      expect(antigravityPlanDisplay(plan, BEFORE).antigravityPlan).toMatchObject({
        models: ENTERPRISE,
        thirdPartyModels: false,
        creditsOverage: false,
      });
    }
    for (const plan of ['Gemini Code Assist Standard', 'Gemini Code Assist Enterprise'])
      expect(antigravityPlanDisplay(plan, BEFORE).antigravityPlan).toMatchObject({
        models: FLASH,
        quotaPolicy: 'none',
        thirdPartyModels: false,
      });
    expect(
      antigravityPlanDisplay('Gemini Code Assist Standard', BEFORE).antigravityPlan?.summary
    ).toContain('Gemini CLI quota that AAC does not read');
    expect(
      antigravityPlanDisplay('Gemini Enterprise Standard', BEFORE).antigravityPlan?.summary
    ).toContain('pooled edition seat credits');
    const changed = antigravityPlanDisplay('pro', AFTER);
    changed.antigravityPlan?.models?.push('mutated');
    expect(antigravityPlanDisplay('pro', AFTER).antigravityPlan?.models).not.toContain('mutated');
  });
});
