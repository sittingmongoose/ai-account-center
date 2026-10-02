import type {
  ClaudeDashboardPlatform,
  DashboardAccount,
  DashboardPlatform,
  DashboardProvider,
  DashboardProviderEntry,
  DashboardSignInKind,
  DashboardSignInUnavailableReason,
} from './account-dashboard-types';
import { DASHBOARD_PROVIDER_IDS } from './dashboard-provider-table';

/**
 * The provider registry in the dashboard DTO (CONTRACT-registry-lifecycle
 * sections 2 and 7): one server-side table of labels, order and sign-in kinds,
 * plus live facts (account counts, visibility, transport and which lifecycle
 * flows can run now). Clients render from it; their own table is a fallback
 * for older servers only.
 */
interface ProviderDefinition {
  label: string;
  longLabel: string;
  switchable: boolean;
  multiAccount: boolean;
  signIn: { kind: DashboardSignInKind; label: string; platforms: DashboardPlatform[] };
  extras: DashboardProviderEntry['extras'];
  /** Open routes that exist without the lifecycle routes. */
  openApp: ClaudeDashboardPlatform[];
  /** Open routes added with the lifecycle routes (`POST /api/accounts/:id/open`). */
  lifecycleOpenApp: ClaudeDashboardPlatform[];
}

const PROVIDERS: Readonly<Record<DashboardProvider, ProviderDefinition>> = Object.freeze({
  claude: {
    label: 'Claude',
    longLabel: 'Claude',
    switchable: false,
    multiAccount: true,
    signIn: { kind: 'desktop-profile', label: 'Desktop profile', platforms: ['mac', 'windows'] },
    extras: null,
    openApp: ['mac', 'windows'],
    lifecycleOpenApp: [],
  },
  codex: {
    label: 'Codex',
    longLabel: 'Codex',
    switchable: true,
    multiAccount: true,
    signIn: { kind: 'device-code', label: 'Device-code sign-in', platforms: ['ubuntu'] },
    extras: null,
    openApp: [],
    lifecycleOpenApp: [],
  },
  antigravity: {
    label: 'Antigravity',
    longLabel: 'Google Antigravity CLI',
    switchable: true,
    multiAccount: true,
    signIn: { kind: 'supervised-cli', label: 'Supervised CLI login', platforms: ['ubuntu'] },
    extras: null,
    openApp: [],
    lifecycleOpenApp: [],
  },
  cursor: {
    label: 'Cursor',
    longLabel: 'Cursor',
    switchable: false,
    multiAccount: false,
    signIn: { kind: 'app-session', label: 'Desktop app session', platforms: ['mac', 'windows'] },
    extras: null,
    openApp: [],
    lifecycleOpenApp: ['mac', 'windows'],
  },
  muse: {
    label: 'Muse Code',
    longLabel: 'Muse Code',
    switchable: false,
    multiAccount: false,
    signIn: { kind: 'device-code', label: 'Device-code sign-in', platforms: ['mac'] },
    extras: {
      kind: 'browser-extension',
      platform: 'mac',
      label: 'Quota sync by browser extension',
    },
    openApp: [],
    lifecycleOpenApp: [],
  },
  'kimi-code': {
    label: 'Kimi Code',
    longLabel: 'Kimi Code',
    switchable: false,
    multiAccount: true,
    signIn: { kind: 'api-key', label: 'API key', platforms: ['ubuntu'] },
    extras: null,
    openApp: [],
    lifecycleOpenApp: [],
  },
  qwen: {
    label: 'Qwen Token Plan',
    longLabel: 'Qwen Token Plan',
    switchable: false,
    multiAccount: false,
    signIn: {
      kind: 'browser-session',
      label: 'Console session by browser extension',
      platforms: ['windows'],
    },
    extras: null,
    openApp: [],
    lifecycleOpenApp: [],
  },
  zai: {
    label: 'Z.ai Coding Plan',
    longLabel: 'Z.ai Coding Plan',
    switchable: false,
    multiAccount: true,
    signIn: { kind: 'api-key', label: 'API key', platforms: ['ubuntu'] },
    extras: null,
    openApp: [],
    lifecycleOpenApp: [],
  },
  'opencode-go': {
    label: 'OpenCode Go',
    longLabel: 'OpenCode Go',
    switchable: false,
    multiAccount: true,
    signIn: { kind: 'api-key', label: 'API key', platforms: ['ubuntu'] },
    extras: {
      kind: 'browser-extension',
      platform: 'mac',
      label: 'Console wallet by browser extension',
    },
    openApp: [],
    lifecycleOpenApp: [],
  },
});

/** Sign-in kinds that carry a key or a code: they need HTTPS or a tunnel. */
const SECURE_KINDS: ReadonlySet<DashboardSignInKind> = new Set([
  'api-key',
  'device-code',
  'supervised-cli',
]);
/** Sign in again only shows a guide for these kinds; nothing runs on the server. */
const GUIDE_KINDS: ReadonlySet<DashboardSignInKind> = new Set([
  'desktop-profile',
  'app-session',
  'browser-session',
]);
const MAX_ACCOUNTS_PER_PROVIDER = 16;

export interface ProviderRegistryFacts {
  /**
   * The lifecycle routes (add, sign in again, replace key, open, re-check,
   * remove) are served. False until they ship: every sign-in reads
   * `available: false` with `not_implemented`, so clients show "Coming".
   */
  lifecycleRoutes: boolean;
  /** isSecureTransport(req) for this request (CONTRACT-auth-devices 2a). */
  secureTransport: boolean;
  /** A flow that cannot run now, for example a failed isolation preflight. */
  flows?: Partial<Record<DashboardProvider, DashboardSignInUnavailableReason>>;
}

export const DEFAULT_PROVIDER_REGISTRY_FACTS: Readonly<ProviderRegistryFacts> = Object.freeze({
  lifecycleRoutes: false,
  secureTransport: false,
});

export function buildDashboardProviders(
  accounts: readonly DashboardAccount[],
  hiddenProviders: readonly DashboardProvider[],
  facts: ProviderRegistryFacts = DEFAULT_PROVIDER_REGISTRY_FACTS
): DashboardProviderEntry[] {
  const hidden = new Set(hiddenProviders);
  return DASHBOARD_PROVIDER_IDS.map((id, order) => {
    const definition = PROVIDERS[id];
    const kind = definition.signIn.kind;
    const secureTransportRequired = SECURE_KINDS.has(kind);
    const accountCount = accounts.filter((account) => account.provider === id).length;
    const unavailableReason: DashboardSignInUnavailableReason | null = !facts.lifecycleRoutes
      ? 'not_implemented'
      : (facts.flows?.[id] ??
        (secureTransportRequired && !facts.secureTransport ? 'secure_transport_required' : null));
    const available = unavailableReason === null;
    return {
      id,
      label: definition.label,
      longLabel: definition.longLabel,
      iconKey: id,
      order,
      visible: !hidden.has(id),
      accountCount,
      switchable: definition.switchable,
      signIn: {
        kind,
        label: definition.signIn.label,
        platforms: [...definition.signIn.platforms],
        secureTransportRequired,
        available,
        unavailableReason,
      },
      extras: definition.extras ? { ...definition.extras } : null,
      capabilities: {
        multiAccount: definition.multiAccount,
        add:
          available &&
          accountCount < MAX_ACCOUNTS_PER_PROVIDER &&
          (definition.multiAccount || accountCount === 0),
        signInAgain:
          facts.lifecycleRoutes && kind !== 'api-key' && (GUIDE_KINDS.has(kind) || available),
        replaceKey: kind === 'api-key' && available,
        remove: facts.lifecycleRoutes,
        activate: definition.switchable,
        autoSwitch: definition.switchable,
        openApp: [
          ...definition.openApp,
          ...(facts.lifecycleRoutes ? definition.lifecycleOpenApp : []),
        ],
        recheck: facts.lifecycleRoutes,
      },
    };
  });
}
