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
        models: [...GEMINI, 'Claude Sonnet 5.5', 'Claude Opus 5.5'],
        thirdPartyModels: true,
        creditsOverage: true,
      });
      if (plan.startsWith('ultra'))
        expect(antigravityPlanDisplay(plan, BEFORE).antigravityPlan?.models).not.toContain(
          'Claude Sonnet 4.6'
        );
    }
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
