// The upgrade path for a cached classic page; not the classic app's bridge. The classic Slint app was retired (Leptos
// finish, step 9) and its bridge with it. A device that chose the classic app (aac-frame=classic) can still open the
// copy of the classic start page that its previous service worker stored (a slow launch falls back to it after 3 s),
// and that page loads exactly one script: this address, as a module. Here it forgets the classic choice and moves the
// page to the app frame, at the tab its address named (lib.rs page_from_location, case for case) with the rest of its
// query and its #fragment. It loads nothing else, so it works whatever else the old cache holds.

const TABS = ['home', 'analytics', 'accounts', 'status', 'scoreboard'];
/** The classic app's own query keys; the tab is in the frame's path instead (lib.rs kept_search). */
const RETIRED_KEYS = ['view', 'classic', 'content'];

/** A query key as URLSearchParams reads it (`+` is a space, `%XX` is decoded); a malformed one stays as written. */
function decodedKey(pair) {
  const key = pair.split('=')[0].replace(/\+/g, ' ');
  try {
    return decodeURIComponent(key);
  } catch {
    return key;
  }
}

/** The frame address for a classic page's address: `/app/<tab>` plus the query without the classic keys. */
export function frameAddress(pathname, search) {
  const raw = typeof search === 'string' ? search : '';
  let view = null;
  try {
    view = new URLSearchParams(raw).get('view');
  } catch {
    view = null;
  }
  const path = typeof pathname === 'string' ? pathname : '/';
  const trimmed = path.replace(/\/+$/, '');
  let tab;
  if (path === '/app' || path.startsWith('/app/')) {
    const last = trimmed.split('/').pop();
    tab = TABS.includes(last) ? last : 'status';
  } else if (TABS.includes(view)) {
    tab = view;
  } else if (['/analytics', '/accounts', '/status', '/scoreboard'].includes(trimmed)) {
    tab = trimmed.slice(1);
  } else {
    tab = /^\/accounts\/[a-z0-9][a-z0-9-]{0,63}$/.test(trimmed) ? 'accounts' : 'home';
  }
  const kept = raw
    .replace(/^\?/, '')
    .split('&')
    .filter((pair) => pair !== '' && !RETIRED_KEYS.includes(decodedKey(pair)));
  return `/app/${tab}${kept.length ? `?${kept.join('&')}` : ''}`;
}

/** Forget the classic choice and open the frame in this page's place (no history entry for the classic page). The
 * #fragment comes along as written (an empty one adds nothing), as the frame keeps it when it takes an address over. */
export function leaveClassic(win) {
  try {
    win.localStorage.removeItem('aac-frame');
  } catch {
    // storage refused: the frame ignores the choice anyway
  }
  const hash =
    typeof win.location.hash === 'string' && win.location.hash.length > 1 ? win.location.hash : '';
  win.location.replace(frameAddress(win.location.pathname, win.location.search) + hash);
}

if (typeof window !== 'undefined' && window.location) leaveClassic(window);
