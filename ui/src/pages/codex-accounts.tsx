import { useTranslation } from 'react-i18next';
import { CodexAuthProfilesCard } from '@/components/compatible-cli/codex-auth-profiles-card';

export function CodexAccountsPage() {
  const { t } = useTranslation();

  return (
    <div className="mx-auto w-full max-w-6xl space-y-6 p-4 sm:p-6">
      <header className="space-y-2">
        <h1 className="text-2xl font-semibold tracking-tight">{t('codexPage.accountsTitle')}</h1>
        <p className="text-sm text-muted-foreground">{t('codexPage.accountsInstruction')}</p>
      </header>
      <CodexAuthProfilesCard />
    </div>
  );
}
