import { afterAll, describe, expect, it } from 'vitest';
import i18n from '@/lib/i18n';

const originalLanguage = i18n.language;
afterAll(async () => {
  await i18n.changeLanguage(originalLanguage);
});

describe('Claude desktop account translations', () => {
  it.each(['en', 'pt-BR', 'zh-CN', 'vi', 'ja', 'ko'])(
    'defines account guidance in %s',
    (locale) => {
      for (const key of [
        'title',
        'instruction',
        'refresh',
        'loading',
        'loadError',
        'empty',
        'accountList',
        'originalProfile',
        'openLauncher',
        'launcherLocation',
        'startMenuLocation',
        'findInstalledApp',
        'noLauncher',
        'copyLocation',
        'copied',
        'copyFailed',
        'cliDescription',
        'cliLink',
        'openAccount',
        'openProfile',
        'opening',
        'openSuccess',
        'browserOpenHint',
        'cachedUsageHint',
        'noUsageHistory',
        'launcherDetails',
      ]) {
        expect(i18n.exists(`claudeAccountsPage.${key}`, { lng: locale, fallbackLng: false })).toBe(
          true
        );
      }
      for (const key of [
        'title',
        'cached',
        'loading',
        'loadError',
        'refreshError',
        'reauthRequired',
        'notConnected',
        'unavailable',
        'usedPercent',
        'resets',
        'sampled',
        'fetched',
        'fiveHour',
        'weekly',
        'weeklyOpus',
        'weeklySonnet',
        'extra',
        'explanation',
        'recordedCliUsage',
        'recordedCliUsageDescription',
        'resetUnavailable',
      ]) {
        expect(i18n.exists(`accountQuota.${key}`, { lng: locale, fallbackLng: false })).toBe(true);
      }
      expect(i18n.exists('codex.auth.quotaProfileLabel', { lng: locale, fallbackLng: false })).toBe(
        true
      );
    }
  );
});
