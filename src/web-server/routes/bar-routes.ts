/** Native bar process-identity probe. The shared quota snapshot row types
 * (QuotaWindowDetail, BalanceWindowDetail, BarSummaryRow) live in
 * services/account-dashboard-types.ts. */

import { createApiRouter } from './api-router';

const barRoutes = createApiRouter();

// Access and nonce-bound HMAC validation run in the authenticated API aggregator.
// This liveness response never reads account state or starts provider collection.
barRoutes.get('/auth', (_req, res) => res.json({ status: 'ok' }));

export default barRoutes;
