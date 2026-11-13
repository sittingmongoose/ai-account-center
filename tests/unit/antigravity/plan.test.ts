import { describe, expect, it } from 'bun:test';
import { antigravityPlanDisplay } from '../../../src/antigravity/plan';

const BEFORE = '2026-11-01T23:59:59Z';
const AFTER = '2026-11-02T00:00:00Z';
const GEMINI = ['Gemini 3.8 Flash', 'Gemini 3.7 Flash', 'Gemini 3.6 Flash', 'Gemini 3.1 Pro'];

describe('Antigravity plan table dated 2026-10-07', () => {
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
    ['Google Workspace Business Standard', 'Google Workspace', 'workspace', 'none'],
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

  it('removes retired models exactly on November 2 using the sample time', () => {
    for (const plan of ['free', 'plus', 'pro-trial']) {
      const before = antigravityPlanDisplay(plan, BEFORE).antigravityPlan;
      expect(before?.models).toEqual([
        ...GEMINI,
        'Claude Sonnet 4.6',
        'Claude Opus 4.6',
        'GPT-OSS-120B',
      ]);
      expect(before?.thirdPartyModels).toBe(true);
      expect(before?.creditsOverage).toBe(false);
      const after = antigravityPlanDisplay(plan, AFTER).antigravityPlan;
      expect(after?.models).toEqual(GEMINI);
      expect(after?.thirdPartyModels).toBe(false);
    }
    for (const plan of ['pro', 'ultra', 'ultra-5x', 'ultra-20x']) {
      expect(antigravityPlanDisplay(plan, BEFORE).antigravityPlan?.models).toContain(
        'GPT-OSS-120B'
      );
      expect(antigravityPlanDisplay(plan, AFTER).antigravityPlan).toMatchObject({
        models: [
          ...GEMINI,
          ...(plan === 'pro' ? ['Gemini 4 Argon', 'Gemini 4 Hydrogen'] : []),
          'Claude Sonnet 5.5',
          'Claude Opus 5.5',
        ],
        thirdPartyModels: true,
        creditsOverage: true,
      });
      if (plan.startsWith('ultra'))
        expect(antigravityPlanDisplay(plan, BEFORE).antigravityPlan?.models).not.toContain(
          'Claude Sonnet 4.6'
        );
    }
  });

  it('reads the free tier by its current Google name and records Pro Argon and Hydrogen pools (2026-11-13)', () => {
    const free = antigravityPlanDisplay(null, AFTER, {
      paidTier: { id: 'free-tier', name: 'Antigravity Starter Quota' },
      currentTier: { id: 'free-tier', name: 'Antigravity' },
    });
    expect(free.plan).toBe('Free');
    expect(free.antigravityPlan).toMatchObject({ class: 'free', quotaPolicy: 'weekly' });
    expect(antigravityPlanDisplay('Antigravity Starter Quota', AFTER).plan).toBe('Free');
    for (const plan of ['free', 'plus', 'pro-trial', 'ultra'])
      expect(antigravityPlanDisplay(plan, AFTER).antigravityPlan?.models).not.toContain(
        'Gemini 4 Hydrogen'
      );
    const pro = antigravityPlanDisplay(null, AFTER, {
      paidTier: { id: 'g1-pro-tier', name: 'Google AI Pro' },
    }).antigravityPlan;
    expect(pro?.models).toEqual(expect.arrayContaining(['Gemini 4 Argon', 'Gemini 4 Hydrogen']));
    expect(pro?.summary).toContain('Gemini 4 Hydrogen counts against the Gemini Models limits');
    expect(pro?.summary).toContain('Gemini 4 Argon has its own weekly limit');
  });

  it('keeps enterprise Gemini-only, Code Assist quota separate, Workspace empty and model arrays independent', () => {
    for (const plan of [
      'Gemini Enterprise Standard',
      'Gemini Enterprise Plus',
      'Gemini Enterprise Standard Emerging Market',
      'Gemini Enterprise Pay-as-you-go',
      'Gemini Enterprise Business',
      'Gemini Code Assist Standard',
      'Gemini Code Assist Enterprise',
    ]) {
      expect(antigravityPlanDisplay(plan, BEFORE).antigravityPlan).toMatchObject({
        models: GEMINI,
        thirdPartyModels: false,
        creditsOverage: false,
      });
    }
    expect(
      antigravityPlanDisplay('Gemini Code Assist Standard', BEFORE).antigravityPlan?.summary
    ).toContain('Gemini CLI quota that AAC does not read');
    expect(antigravityPlanDisplay('Google Workspace', BEFORE).antigravityPlan?.models).toEqual([]);
    const changed = antigravityPlanDisplay('pro', AFTER);
    changed.antigravityPlan?.models?.push('mutated');
    expect(antigravityPlanDisplay('pro', AFTER).antigravityPlan?.models).not.toContain('mutated');
  });
});
