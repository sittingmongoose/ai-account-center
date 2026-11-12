// Reset hints carry no account data. They invalidate observations, never authorize a spend.
export function createResetHints(win) {
  const listeners = new Set(), seen = new Set();
  const key = 'aac-reset-claims-changed', type = 'reset-claims-changed';
  let socket, channel, retry, stopped = true, serial = 0;
  const notify = () => { for (const listener of listeners) listener(); };
  const nextId = () => `${Date.now()}:${++serial}:${Math.random()}`;
  const remember = (id) => {
    if (seen.has(id)) return false;
    seen.add(id);
    if (seen.size > 128) seen.delete(seen.values().next().value);
    return true;
  };
  const relay = (hint) => {
    try { channel?.postMessage(hint); } catch { /* Storage is the fallback. */ }
    try { win.localStorage.setItem(key, JSON.stringify(hint)); } catch { /* The server socket still carries hints. */ }
  };
  const receive = (hint, peers = false) => {
    if (hint?.type !== type) return;
    const id = typeof hint.id === 'string' ? hint.id : nextId();
    if (!remember(id)) return;
    notify();
    if (peers) relay({type, id});
  };
  const visible = () => { if (win.document.visibilityState !== 'hidden') notify(); };
  const storage = (event) => {
    if (event.key !== key) return;
    try { receive(JSON.parse(event.newValue)); } catch { notify(); }
  };
  const connect = () => {
    if (stopped) return;
    try {
      socket = new win.WebSocket(`${win.location.protocol === 'https:' ? 'wss' : 'ws'}://${win.location.host}/ws`);
      socket.onopen = notify; // Reconnect reads through any missed events.
      socket.onmessage = (event) => {
        try { receive(JSON.parse(event.data), true); } catch { /* Not a reset hint. */ }
      };
      socket.onclose = () => {
        notify();
        if (!stopped) retry = win.setTimeout(connect, 1000);
      };
    } catch {
      notify();
      retry = win.setTimeout(connect, 1000);
    }
  };
  const start = () => {
    stopped = false;
    try {
      channel = new win.BroadcastChannel(key);
      channel.onmessage = (event) => receive(event.data);
    } catch { /* Storage events remain available if BroadcastChannel is unsupported. */ }
    win.addEventListener('storage', storage);
    win.addEventListener('focus', visible);
    win.addEventListener('online', visible);
    win.addEventListener('aac-shown', visible);
    win.document.addEventListener('visibilitychange', visible);
    connect();
  };
  const stop = () => {
    stopped = true;
    win.clearTimeout(retry);
    if (socket) {
      socket.onopen = socket.onmessage = socket.onclose = null;
      socket.close();
    }
    channel?.close();
    channel = undefined;
    win.removeEventListener('storage', storage);
    win.removeEventListener('focus', visible);
    win.removeEventListener('online', visible);
    win.removeEventListener('aac-shown', visible);
    win.document.removeEventListener('visibilitychange', visible);
  };
  return {
    subscribe(listener) {
      listeners.add(listener);
      if (listeners.size === 1) start();
      return () => { listeners.delete(listener); if (!listeners.size) stop(); };
    },
    publish() {
      const id = nextId(); remember(id); relay({type, id});
    },
  };
}

export function installResetHints(win) {
  win.aacResetHints ??= createResetHints(win);
  return win.aacResetHints;
}
