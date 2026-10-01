import { CircleDot, Terminal } from 'lucide-react';
import { Badge } from '@/components/ui/badge';
import { Card, CardContent } from '@/components/ui/card';
import { usePrivacy, PRIVACY_BLUR_CLASS } from '@/contexts/privacy-context';
import type { DashboardAccount, DashboardProvider } from '@/hooks/use-accounts-dashboard';
import { cn } from '@/lib/utils';
import { AccountStatusBadge, DashboardUsage } from './dashboard-usage';

const PROVIDERS: { id: DashboardProvider; label: string; icon: string }[] = [
  { id: 'antigravity', label: 'Antigravity CLI', icon: '' },
  { id: 'muse', label: 'Muse Code', icon: '' },
  { id: 'cursor', label: 'Cursor', icon: '/assets/sidebar/cursor.svg' },
  { id: 'kimi-code', label: 'Kimi Code', icon: '/assets/providers/kimi.svg' },
  { id: 'qwen', label: 'Qwen token plan', icon: '/assets/providers/qwen-color.svg' },
  { id: 'zai', label: 'Z.ai coding plan', icon: '/icons/zai.svg' },
  { id: 'opencode-go', label: 'OpenCode Go', icon: '' },
];

export function UsageOnlyPanel({
  accounts,
  loading,
  error,
}: {
  accounts: DashboardAccount[];
  loading: boolean;
  error: boolean;
}) {
  const { privacyMode } = usePrivacy();
  return (
    <section id="usage" className="scroll-mt-24 space-y-4" aria-labelledby="usage-heading">
      <div className="space-y-1">
        <div className="flex items-center gap-2">
          <h2 id="usage-heading" className="text-xl font-semibold tracking-tight">
            Other accounts
          </h2>
          <Badge variant="secondary">Usage only</Badge>
        </div>
        <p className="text-sm text-muted-foreground">
          Subscription limits and reset times from your signed-in tools.
        </p>
      </div>
      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
        {PROVIDERS.flatMap((provider) => {
          const matches = accounts.filter((account) => account.provider === provider.id);
          const rows: (DashboardAccount | undefined)[] = matches.length ? matches : [undefined];
          return rows.map((account) => (
            <Card
              key={account?.id ?? provider.id}
              className="gap-0 overflow-hidden border-border/70 py-0 shadow-sm"
            >
              <CardContent className="space-y-4 p-5">
                <div className="flex items-start justify-between gap-3">
                  <div className="flex min-w-0 items-center gap-3">
                    <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border bg-muted/40">
                      {provider.icon ? (
                        <img src={provider.icon} alt="" className="h-5 w-5 object-contain" />
                      ) : (
                        <Terminal className="h-4 w-4 text-muted-foreground" />
                      )}
                    </div>
                    <div className="min-w-0 space-y-0.5">
                      <h3 className="text-sm font-semibold">{provider.label}</h3>
                      {(account?.email || account?.label) && (
                        <p
                          className={cn(
                            'break-all text-xs text-muted-foreground',
                            privacyMode && account?.email && PRIVACY_BLUR_CLASS
                          )}
                        >
                          {account?.email || account?.label}
                        </p>
                      )}
                    </div>
                  </div>
                  {account && <AccountStatusBadge status={account.status} />}
                </div>
                {account?.plan && (
                  <p className="text-xs font-medium text-muted-foreground">{account.plan}</p>
                )}
                <DashboardUsage account={account} loading={loading} error={error} />
              </CardContent>
              {account?.source && (
                <div className="mt-auto flex items-center gap-1.5 border-t border-border/60 bg-muted/15 px-5 py-3 text-[11px] text-muted-foreground">
                  <CircleDot className="h-3 w-3" />
                  {account.source}
                </div>
              )}
            </Card>
          ));
        })}
      </div>
    </section>
  );
}
