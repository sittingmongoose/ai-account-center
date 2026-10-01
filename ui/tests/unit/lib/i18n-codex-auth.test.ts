import { afterAll, describe, expect, it } from 'vitest';
import i18n from '@/lib/i18n';

const locales = ['en', 'pt-BR', 'zh-CN', 'vi', 'ja', 'ko'] as const;

const codexAuthKeys = [
  ['codex.auth.sourceDefault'],
  ['codex.auth.sourceEnv'],
  ['codex.auth.sourceExplicitCodexHome'],
  ['codex.auth.terminalOnlyTooltipRich'],
  ['codex.auth.activeSourceBadge', { source: 'default' }],
  ['codex.auth.statusOk'],
  ['codex.auth.statusInvalid'],
  ['codex.auth.loading'],
  ['codex.auth.loadError'],
  ['codex.auth.emptyRegistryRich'],
  ['codex.auth.legacyCodexHomeRich'],
  ['codex.auth.legacyModeRich'],
  ['codex.auth.externalCodexHomeRich', { path: '/tmp/codex-home' }],
  ['codex.auth.activeProfile'],
  ['codex.auth.unknownProfile'],
  ['codex.auth.planLabel'],
  ['codex.auth.switchAction'],
  ['codex.auth.removeAction'],
  ['codex.auth.liveAccount'],
  ['codex.auth.noLiveAccount'],
  ['codex.auth.launchDefault'],
  ['codex.auth.noLaunchDefault'],
  ['codex.auth.launchProfile'],
  ['codex.auth.activateAction'],
  ['codex.auth.activateProfileAction', { name: 'work' }],
  ['codex.auth.activatingAction'],
  ['codex.auth.activatedBadge'],
  ['codex.auth.activationDescription'],
  ['codex.auth.activationRequiresEmail'],
  ['codex.auth.activationSuccess', { email: 'work@example.test' }],
  ['codex.auth.removeTooltipRich'],
  ['codex.auth.col.name'],
  ['codex.auth.col.email'],
  ['codex.auth.col.plan'],
  ['codex.auth.col.lastUsed'],
  ['codex.auth.col.status'],
  ['codex.auth.col.actions'],
  ['codexPage.authProfiles'],
  ['codexPage.accountsTitle'],
  ['codexPage.accountsInstruction'],
  ['codexPage.autoSwitch.title'],
  ['codexPage.autoSwitch.enabled'],
  ['codexPage.autoSwitch.disabled'],
  ['codexPage.autoSwitch.loading'],
  ['codexPage.autoSwitch.unavailable'],
  ['codexPage.autoSwitch.description'],
  ['codexPage.autoSwitch.claudeManual'],
  ['codexPage.autoSwitch.loadError'],
  ['codexPage.autoSwitch.retry'],
  ['codexPage.autoSwitch.updateError'],
  ['codexPage.autoSwitch.outcomes.disabled'],
  ['codexPage.autoSwitch.outcomes.scheduled'],
  ['codexPage.autoSwitch.outcomes.healthy'],
  ['codexPage.autoSwitch.outcomes.no_quota'],
  ['codexPage.autoSwitch.outcomes.no_candidate'],
  ['codexPage.autoSwitch.outcomes.waiting_idle'],
  ['codexPage.autoSwitch.outcomes.switching'],
  ['codexPage.autoSwitch.outcomes.switched'],
  ['codexPage.autoSwitch.outcomes.error'],
] as const;

const originalLanguage = i18n.language;

afterAll(async () => {
  await i18n.changeLanguage(originalLanguage);
});

describe('codex auth i18n', () => {
  it.each(locales)('resolves codex auth dashboard keys for %s', async (locale) => {
    await i18n.changeLanguage(locale);

    for (const [key, options] of codexAuthKeys) {
      const translated = i18n.t(key, options);

      expect(i18n.exists(key, { lng: locale, fallbackLng: false })).toBe(true);
      expect(translated).not.toBe(key);
      expect(translated).not.toContain('codex.auth.');
      expect(translated).not.toContain('codexPage.');
      if (key === 'codex.auth.externalCodexHomeRich') {
        expect(translated).toContain('/tmp/codex-home');
      }
      if (key === 'codex.auth.activeSourceBadge') {
        expect(translated).toContain('default');
      }
    }
  });
});
