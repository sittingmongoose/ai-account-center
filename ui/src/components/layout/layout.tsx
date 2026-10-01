import { useEffect } from 'react';
import { Link, Outlet, useLocation } from 'react-router-dom';
import { ThemeToggle } from './theme-toggle';
import { PrivacyToggle } from '@/components/shared/privacy-toggle';
import { ConnectionIndicator } from '@/components/shared/connection-indicator';
import { CcsLogo } from '@/components/shared/ccs-logo';
import { UserMenu } from '@/components/auth/user-menu';
import { LanguageSwitcher } from './language-switcher';
import { storeLastRoute } from '@/lib/last-route';

export function Layout() {
  const location = useLocation();
  useEffect(() => {
    storeLastRoute(location.pathname, location.search, location.hash);
    if (location.hash) {
      const target = document.getElementById(location.hash.slice(1));
      target?.scrollIntoView?.({ behavior: 'smooth', block: 'start' });
    }
  }, [location.pathname, location.search, location.hash]);
  return (
    <div className="flex min-h-screen w-full flex-col bg-background">
      <header className="sticky top-0 z-30 border-b border-border/70 bg-background/95 backdrop-blur">
        <div className="mx-auto flex min-h-16 max-w-7xl flex-wrap items-center justify-between gap-3 px-4 py-3 sm:px-6 lg:px-8">
          <div className="flex items-center gap-6">
            <Link to="/" aria-label="CCS accounts home">
              <CcsLogo size="sm" />
            </Link>
            <nav
              aria-label="Account sections"
              className="hidden items-center gap-5 text-sm sm:flex"
            >
              <Link
                className="text-muted-foreground transition-colors hover:text-foreground"
                to="/#codex"
              >
                Codex
              </Link>
              <Link
                className="text-muted-foreground transition-colors hover:text-foreground"
                to="/#claude"
              >
                Claude
              </Link>
              <Link
                className="text-muted-foreground transition-colors hover:text-foreground"
                to="/#usage"
              >
                Other accounts
              </Link>
            </nav>
          </div>
          <div className="flex items-center gap-1 sm:gap-2">
            <ConnectionIndicator />
            <div className="hidden lg:block">
              <LanguageSwitcher />
            </div>
            <PrivacyToggle />
            <ThemeToggle />
            <UserMenu />
          </div>
        </div>
      </header>
      <main className="flex-1">
        <Outlet />
      </main>
    </div>
  );
}
