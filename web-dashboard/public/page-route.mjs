// URL state for the pages (CONTRACT-serving-misc section 3). The server serves index.html at /, /login,
// /analytics, /accounts, /accounts/<provider>, /status and /scoreboard, and answers /settings and /home with a
// redirect, so the bridge reads the page from the path and pushes the path form. `?view=<page>` stays an alias.
// Status and Scoreboard are Leptos pages the shell shows in an iframe (shell-frame.mjs, docs/status-tab.md).
export const PAGES = Object.freeze(['home', 'analytics', 'accounts', 'status', 'scoreboard']);

const PATHS = Object.freeze({ home: '/', analytics: '/analytics', accounts: '/accounts', status: '/status', scoreboard: '/scoreboard' });

/** The page a URL opens: `?view=` first (the alias), then the path; anything else is Home. */
export function pageFromUrl(pathname, search = '') {
  let view = null;
  try { view = new URLSearchParams(typeof search === 'string' ? search : '').get('view'); } catch { view = null; }
  if (PAGES.includes(view)) return view;
  const path = typeof pathname === 'string' ? pathname : '';
  if (path === '/analytics') return 'analytics';
  if (path === '/accounts' || /^\/accounts\/[a-z0-9][a-z0-9-]{0,63}$/.test(path)) return 'accounts';
  if (path === '/status') return 'status';
  if (path === '/scoreboard') return 'scoreboard';
  return 'home';
}

/** The path a page is pushed as. */
export function pagePath(page) {
  return Object.hasOwn(PATHS, page) ? PATHS[page] : '/';
}
