import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom';
import { QueryClientProvider } from '@tanstack/react-query';
import { Toaster } from 'sonner';
import { queryClient } from '@/lib/query-client';
import '@/lib/i18n';
import { ThemeProvider } from '@/components/layout/theme-provider';
import { PrivacyProvider } from '@/contexts/privacy-context';
import { AuthProvider } from '@/contexts/auth-context';
import { RequireAuth } from '@/components/auth/require-auth';
import { Layout } from '@/components/layout/layout';
import { HomePage } from '@/pages/home';
import { LoginPage } from '@/pages/login';

/** The portal exposes accounts only. Old bookmarks return to the same dashboard. */
export default function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <ThemeProvider defaultTheme="system" storageKey="vite-ui-theme">
        <PrivacyProvider>
          <AuthProvider>
            <BrowserRouter>
              <Routes>
                <Route path="/login" element={<LoginPage />} />
                <Route element={<RequireAuth />}>
                  <Route element={<Layout />}>
                    <Route path="/" element={<HomePage />} />
                    <Route path="/codex/accounts" element={<Navigate to="/#codex" replace />} />
                    <Route path="/claude/accounts" element={<Navigate to="/#claude" replace />} />
                    <Route path="/settings" element={<Navigate to="/#codex-settings" replace />} />
                    <Route path="*" element={<Navigate to="/" replace />} />
                  </Route>
                </Route>
              </Routes>
              <Toaster position="top-right" />
            </BrowserRouter>
          </AuthProvider>
        </PrivacyProvider>
      </ThemeProvider>
    </QueryClientProvider>
  );
}
