/** Codex account activation for the shared VM home, separate from CCS launch defaults. */

import { Loader2 } from 'lucide-react';
import type { ReactNode } from 'react';
import type { TFunction } from 'i18next';
import { Trans, useTranslation } from 'react-i18next';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip';
import { useActivateCodexAuthProfile, useCodexAuthProfiles } from '@/hooks/use-codex-auth-profiles';
import type {
  CodexAuthProfileEntry,
  CodexAuthProfilesResponse,
} from '@/hooks/use-codex-auth-profiles';

function InlineCode({ children }: { children?: ReactNode }) {
  return (
    <code className="rounded bg-muted px-1 py-0.5 font-mono text-[0.85em] text-foreground">
      {children}
    </code>
  );
}

function formatLastUsed(iso: string | null): string {
  if (!iso) return 'never';
  const diffMs = Date.now() - new Date(iso).getTime();
  if (!Number.isFinite(diffMs)) return iso;
  const diffMin = Math.floor(diffMs / 60_000);
  if (diffMin < 2) return 'just now';
  if (diffMin < 60) return `${diffMin} min ago`;
  const diffH = Math.floor(diffMin / 60);
  if (diffH < 24) return `${diffH}h ago`;
  const diffD = Math.floor(diffH / 24);
  if (diffD === 1) return 'yesterday';
  return `${diffD}d ago`;
}

function sourceLabel(source: 'default' | 'env' | 'explicit-codex-home', t: TFunction): string {
  switch (source) {
    case 'default':
      return t('codex.auth.sourceDefault');
    case 'env':
      return t('codex.auth.sourceEnv');
    case 'explicit-codex-home':
      return t('codex.auth.sourceExplicitCodexHome');
  }
}

function TerminalOnlyRemoveButton() {
  const { t } = useTranslation();
  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>
          <span tabIndex={0} className="inline-block">
            <Button variant="outline" size="sm" disabled className="pointer-events-none">
              {t('codex.auth.removeAction')}
            </Button>
          </span>
        </TooltipTrigger>
        <TooltipContent>
          <Trans i18nKey="codex.auth.removeTooltipRich" components={{ code: <InlineCode /> }} />
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  );
}

function ProfileRow({
  entry,
  isActivated,
  isActivating,
  activationPending,
  onActivate,
}: {
  entry: CodexAuthProfileEntry;
  isActivated: boolean;
  isActivating: boolean;
  activationPending: boolean;
  onActivate: (name: string) => void;
}) {
  const { t } = useTranslation();
  return (
    <TableRow
      className={`grid grid-cols-[minmax(0,1fr)_auto] items-start gap-x-3 gap-y-1 p-3 @3xl:table-row @3xl:p-0 ${isActivated ? 'bg-muted/40' : ''}`}
    >
      <TableCell className="col-start-1 row-start-1 block whitespace-normal p-0 font-medium @3xl:table-cell @3xl:p-2">
        <span className="flex flex-wrap items-center gap-2">
          {entry.name}
          {isActivated && (
            <Badge variant="secondary" className="text-xs">
              {t('codex.auth.activatedBadge')}
            </Badge>
          )}
        </span>
      </TableCell>
      <TableCell className="col-start-1 row-start-2 block break-all whitespace-normal p-0 @3xl:table-cell @3xl:p-2">
        {entry.email ?? '—'}
      </TableCell>
      <TableCell className="col-start-1 row-start-3 block p-0 text-muted-foreground @3xl:table-cell @3xl:p-2 @3xl:text-foreground">
        {entry.plan ?? '—'}
      </TableCell>
      <TableCell className="hidden @3xl:table-cell">{formatLastUsed(entry.lastUsed)}</TableCell>
      <TableCell className="col-start-1 row-start-4 block p-0 @3xl:table-cell @3xl:p-2">
        {entry.authValid ? (
          <Badge variant="secondary" className="text-xs text-green-700 dark:text-green-400">
            {t('codex.auth.statusOk')}
          </Badge>
        ) : (
          <Badge variant="destructive" className="text-xs">
            {t('codex.auth.statusInvalid')}
          </Badge>
        )}
      </TableCell>
      <TableCell className="col-start-2 row-start-1 row-span-4 block p-0 @3xl:table-cell @3xl:p-2">
        <span className="flex flex-col gap-1 @3xl:flex-row">
          <Button
            variant="outline"
            size="sm"
            disabled={!entry.authValid || !entry.email || isActivated || activationPending}
            title={!entry.email ? t('codex.auth.activationRequiresEmail') : undefined}
            aria-label={t('codex.auth.activateProfileAction', { name: entry.name })}
            onClick={() => onActivate(entry.name)}
          >
            {isActivating && <Loader2 className="mr-1 h-3 w-3 animate-spin" />}
            {isActivating ? t('codex.auth.activatingAction') : t('codex.auth.activateAction')}
          </Button>
          <TerminalOnlyRemoveButton />
        </span>
      </TableCell>
    </TableRow>
  );
}

export function CodexAuthProfilesCard() {
  const { t } = useTranslation();
  const { data, isLoading, error } = useCodexAuthProfiles();
  const activation = useActivateCodexAuthProfile();

  if (isLoading) {
    return (
      <div className="flex items-center gap-2 text-sm text-muted-foreground p-4">
        <Loader2 className="h-4 w-4 animate-spin" />
        {t('codex.auth.loading')}
      </div>
    );
  }
  if (error || !data) {
    return (
      <div className="rounded-md border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive">
        {t('codex.auth.loadError')}
      </div>
    );
  }

  return (
    <div className="@container space-y-3">
      <AccountBanners data={data} />
      {activation.error && (
        <div
          role="alert"
          className="rounded-md border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive"
        >
          {activation.error.message}
        </div>
      )}
      {activation.isSuccess && (
        <p role="status" className="text-sm text-muted-foreground">
          {t('codex.auth.activationSuccess', { email: activation.data.email })}
        </p>
      )}
      {data.profiles.length === 0 ? (
        <div className="rounded-md border bg-muted/30 px-4 py-3 text-sm text-muted-foreground">
          <Trans i18nKey="codex.auth.emptyRegistryRich" components={{ code: <InlineCode /> }} />
        </div>
      ) : (
        <div className="rounded-md border overflow-auto">
          <Table className="block @3xl:table">
            <TableHeader className="hidden @3xl:table-header-group">
              <TableRow>
                <TableHead>{t('codex.auth.col.name')}</TableHead>
                <TableHead>{t('codex.auth.col.email')}</TableHead>
                <TableHead>{t('codex.auth.col.plan')}</TableHead>
                <TableHead>{t('codex.auth.col.lastUsed')}</TableHead>
                <TableHead>{t('codex.auth.col.status')}</TableHead>
                <TableHead>{t('codex.auth.col.actions')}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody className="block @3xl:table-row-group">
              {data.profiles.map((entry) => (
                <ProfileRow
                  key={entry.name}
                  entry={entry}
                  isActivated={data.activated?.name === entry.name}
                  isActivating={activation.isPending && activation.variables === entry.name}
                  activationPending={activation.isPending}
                  onActivate={activation.mutate}
                />
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </div>
  );
}

function AccountBanners({ data }: { data: CodexAuthProfilesResponse }) {
  const { t } = useTranslation();
  return (
    <div className="rounded-md border bg-muted/20 px-4 py-3 text-sm space-y-2">
      <div className="space-y-1">
        <div className="font-medium">{t('codex.auth.liveAccount')}</div>
        {data.activated ? (
          <div className="flex flex-wrap items-center gap-2">
            <span>{data.activated.email}</span>
            <Badge variant="secondary">
              {data.activated.name ?? t('codex.auth.unknownProfile')}
            </Badge>
            {data.activated.plan && <span>{data.activated.plan}</span>}
          </div>
        ) : (
          <div className="text-muted-foreground">{t('codex.auth.noLiveAccount')}</div>
        )}
        <p className="text-xs text-muted-foreground">{t('codex.auth.activationDescription')}</p>
      </div>
      <div className="border-t pt-2 text-xs text-muted-foreground space-y-1">
        <p>
          {t('codex.auth.launchDefault')} {data.default ?? t('codex.auth.noLaunchDefault')}
        </p>
        {data.active && (
          <p>
            {t('codex.auth.launchProfile')} {data.active.name ?? data.active.codexHome}
            {' · '}
            {sourceLabel(data.active.source, t)}
          </p>
        )}
      </div>
    </div>
  );
}
