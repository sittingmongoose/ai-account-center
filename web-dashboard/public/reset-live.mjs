// Reset hints carry no account data. They invalidate observations, never authorize a spend.
export function createResetHints(win) {
  const listeners = new Set();
  const key = 'aac-reset-claims-changed';
  let socket, channel, retry, stopped = true, serial = 0;
  const notify = () => { for (const listener of listeners) listener(); };
  const visible = () => { if (win.document.visibilityState !== 'hidden') notify(); };
  const storage = (event) => { if (event.key === key) notify(); };
  const connect = () => {
    if (stopped) return;
    try {
      socket = new win.WebSocket(`${win.location.protocol === 'https:' ? 'wss' : 'ws'}://${win.location.host}/ws`);
      socket.onopen = notify; // Reconnect reads through any missed events.
      socket.onmessage = (event) => {
        try { if (JSON.parse(event.data).type === 'reset-claims-changed') notify(); } catch { /* Not a reset hint. */ }
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
      channel.onmessage = notify;
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
      try { channel?.postMessage(key); } catch { /* Storage is the fallback. */ }
      try { win.localStorage.setItem(key, `${Date.now()}:${++serial}:${Math.random()}`); } catch { /* The server socket still carries hints. */ }
    },
  };
}

export function installResetHints(win) {
  win.aacResetHints ??= createResetHints(win);
  return win.aacResetHints;
}
