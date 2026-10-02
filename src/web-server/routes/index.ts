/**
 * Routes Aggregator - Combines all domain-specific route modules
 *
 * This file serves as the central entry point for all API routes,
 * mounting each domain router at its appropriate path.
 */

import {
  apiAuthMiddleware,
  isDashboardWebSocketOriginAllowed,
  requireDashboardSession,
  requireLocalAccessWhenAuthDisabled,
} from '../middleware/auth-middleware';
import { apiErrorHandler, createApiRouter } from './api-router';
import {
  BAR_AUTH_NONCE_HEADER,
  BAR_AUTH_TOKEN_HEADER,
  createBarAuthProof,
  getOrCreateBarAuthToken,
  isValidBarAuthNonce,
} from '../../utils/bar-auth-token';

// Import domain routers
import accountDashboardRoutes from './account-dashboard-routes';
import accountRefreshSettingsRoutes from './account-refresh-settings-routes';
import accountAnalyticsRoutes from './account-analytics-routes';
import appUpdateRoutes from './app-update-routes';
import codexRoutes from './codex-routes';
import antigravityRoutes from './antigravity-routes';
import authRoutes from './auth-routes';
import claudeDesktopRoutes from './claude-desktop-routes';
import barRoutes from './bar-routes';

// Create the main API router (case-sensitive; handler errors stay in the request)
export const apiRoutes = createApiRouter();

const REMOTE_WRITE_ACCESS_ERROR =
  'Remote dashboard writes require localhost access when dashboard auth is disabled.';

// The native Bar probe is local when auth is disabled and uses a session when
// enabled. Its nonce-bound proof identifies the running account-center process.
const BAR_LOCAL_ACCESS_ERROR =
  'CCS Bar endpoints require localhost access when dashboard auth is disabled.';

function isMutationMethod(method: string): boolean {
  const normalized = method.toUpperCase();
  return (
    normalized === 'POST' ||
    normalized === 'PUT' ||
    normalized === 'PATCH' ||
    normalized === 'DELETE'
  );
}

// The session guard runs on the /api mount itself, so no casing of the mount
// path can reach a route without it. Public routes: auth login/check/setup, health.
apiRoutes.use(apiAuthMiddleware);

apiRoutes.use((req, res, next) => {
  // Guard the exact Bar path segment for every method, including retired paths.
  if (req.path === '/bar' || req.path.startsWith('/bar/')) {
    if (requireDashboardSession(req, res, BAR_LOCAL_ACCESS_ERROR)) {
      if (!isDashboardWebSocketOriginAllowed(req)) {
        res.status(403).json({ error: 'Native bar probes require the dashboard origin.' });
        return;
      }
      // Authenticate liveness probes with a nonce-bound HMAC so normal Bar
      // responses never disclose the persistent file token, and captured probe
      // proofs cannot be replayed for a future probe.
      const nonce = req.header(BAR_AUTH_NONCE_HEADER)?.trim() ?? '';
      if (req.method === 'GET' && req.path === '/bar/auth' && isValidBarAuthNonce(nonce)) {
        res.setHeader(BAR_AUTH_TOKEN_HEADER, createBarAuthProof(getOrCreateBarAuthToken(), nonce));
      }
      next();
    }
    return;
  }

  if (!isMutationMethod(req.method)) {
    next();
    return;
  }

  if (requireLocalAccessWhenAuthDisabled(req, res, REMOTE_WRITE_ACCESS_ERROR)) {
    next();
  }
});

// Only the account dashboard and native account controls are mounted.
apiRoutes.use('/accounts', accountDashboardRoutes);
apiRoutes.use('/accounts', accountRefreshSettingsRoutes);
apiRoutes.use('/accounts', accountAnalyticsRoutes);
apiRoutes.use('/app-updates', appUpdateRoutes);
apiRoutes.use('/auth', authRoutes);
apiRoutes.use('/claude', claudeDesktopRoutes);
apiRoutes.use('/codex', codexRoutes);
apiRoutes.use('/antigravity', antigravityRoutes);
apiRoutes.use('/bar', barRoutes);

// Public process liveness compatibility; no configuration details or repair actions.
apiRoutes.get('/health', (_req, res) => res.json({ status: 'ok' }));

// Must stay last: a failed API request ends as a generic 500 in its own response.
apiRoutes.use(apiErrorHandler);
