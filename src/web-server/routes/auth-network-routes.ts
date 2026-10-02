import type { Request, Response, Router } from 'express';
import { mutateConfig } from '../../config/config-loader-facade';
import {
  describeConnection,
  isDirectLoopbackRequest,
  localNetworkTrust,
} from '../middleware/secure-transport';
import { withAuthWriteGate } from '../services/dashboard-auth-files';
import { invalidateDashboardNetworkSettings } from '../services/dashboard-network-config';
import {
  audit,
  invalidBody,
  readAuthBody,
  requireAuthConfigured,
  sendAuthError,
} from './auth-route-helpers';
import { requireBrowserSession } from './auth-session-routes';

/**
 * CONTRACT-auth-devices section 2a, rule 4: the owner's "Trust my local
 * network" switch (`dashboard_network.trust_local_network` in config.yaml).
 *
 * - `GET /api/auth/network` reads it, the trusted ranges and this connection.
 * - `PUT /api/auth/network` `{trustLocalNetwork}` turns it off from any
 *   signed-in browser session, and on only from the dashboard computer itself
 *   (a direct loopback request), since turning it on lets passwords, keys and
 *   codes cross the network unencrypted. The config file can always set it.
 *
 * The ranges (`dashboard_network.trusted_networks`) are edited only in the file.
 */
export interface NetworkTrustView {
  trustLocalNetwork: boolean;
  trustedNetworks: string[];
  connection: { peer: string; trusted: boolean };
  /** True when this request may turn the trust on (a direct loopback request). */
  canTurnOn: boolean;
}

function view(req: Request): NetworkTrustView {
  const trust = localNetworkTrust();
  return {
    trustLocalNetwork: trust.enabled,
    trustedNetworks: trust.networks.map((network) => network.cidr),
    connection: describeConnection(req),
    canTurnOn: isDirectLoopbackRequest(req),
  };
}

function readNetwork(req: Request, res: Response): void {
  if (!requireAuthConfigured(res)) return;
  if (!requireBrowserSession(req, res)) return;
  if (req.originalUrl.includes('?')) {
    sendAuthError(res, 400, 'unexpected_query', 'This request does not take a query string.');
    return;
  }
  res.json(view(req));
}

async function changeNetwork(req: Request, res: Response): Promise<void> {
  if (!requireAuthConfigured(res)) return;
  if (!requireBrowserSession(req, res)) return;
  const body = readAuthBody(req, res, 'required', ['trustLocalNetwork']);
  if (!body) return;
  const wanted = body.trustLocalNetwork;
  if (typeof wanted !== 'boolean') {
    invalidBody(res);
    return;
  }
  const loopback = isDirectLoopbackRequest(req);
  if (wanted && !loopback) {
    sendAuthError(
      res,
      403,
      'loopback_required',
      'Turn on local network trust on the dashboard computer itself, or in config.yaml.'
    );
    return;
  }
  let changed = false;
  try {
    changed = await withAuthWriteGate(async () => {
      if (localNetworkTrust().enabled === wanted) return false;
      mutateConfig((config) => {
        const existing = config.dashboard_network ?? {};
        config.dashboard_network = { ...existing, trust_local_network: wanted };
      });
      invalidateDashboardNetworkSettings();
      return true;
    });
  } catch {
    invalidateDashboardNetworkSettings();
    sendAuthError(res, 500, 'write_failed', 'The setting could not be saved.');
    return;
  }
  if (changed) {
    audit('auth.network.trust_changed', 'Local network trust changed', {
      trustLocalNetwork: wanted,
      from: loopback ? 'loopback' : 'remote',
    });
  }
  res.json(view(req));
}

export function registerAuthNetworkRoutes(router: Router): void {
  router.get('/network', readNetwork);
  router.put('/network', (req, res) => {
    void changeNetwork(req, res);
  });
}
