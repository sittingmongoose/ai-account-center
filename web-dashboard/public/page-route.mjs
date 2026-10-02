// URL state for the three pages (CONTRACT-serving-misc section 3). The server serves index.html at /, /login,
// /analytics, /accounts and /accounts/<provider>, and answers /settings and /home with a redirect, so the bridge
// reads the page from the path and pushes the path form. `?view=home|analytics|accounts` stays an alias.
export const PAGES = Object.freeze(['home', 'analytics', 'accounts']);

/** The page a URL opens: `?view=` first (the alias), then the path; anything else is Home. */
export function pageFromUrl(pathname, search = '') {
  let view = null;
  try { view = new URLSearchParams(typeof search === 'string' ? search : '').get('view'); } catch { view = null; }
  if (PAGES.includes(view)) return view;
  const path = typeof pathname === 'string' ? pathname : '';
  if (path === '/analytics') return 'analytics';
  if (path === '/accounts' || /^\/accounts\/[a-z0-9][a-z0-9-]{0,63}$/.test(path)) return 'accounts';
  return 'home';
}

/** The path a page is pushed as. */
export function pagePath(page) {
  return page === 'analytics' ? '/analytics' : page === 'accounts' ? '/accounts' : '/';
}
