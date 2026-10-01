import type { ClientRequest, IncomingMessage } from 'node:http';
import { defineConfig } from 'vite';
import type { ProxyOptions } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import path from 'path';

const UI_ROOT = __dirname;
const REPO_ROOT = path.resolve(__dirname, '..');
const BACKEND_ORIGIN = 'http://localhost:3000';
const TRUSTED_VITE_DEV_ORIGINS = new Set([
  'http://localhost:5173',
  'http://127.0.0.1:5173',
  'http://[::1]:5173',
]);

function rewriteTrustedViteDevOrigin(proxyRequest: ClientRequest, request: IncomingMessage): void {
  const origin = request.headers.origin;
  if (origin && TRUSTED_VITE_DEV_ORIGINS.has(origin)) {
    proxyRequest.setHeader('origin', BACKEND_ORIGIN);
  }
}

const configureHttpProxy: NonNullable<ProxyOptions['configure']> = (proxy) => {
  proxy.on('proxyReq', rewriteTrustedViteDevOrigin);
};

const configureWebSocketProxy: NonNullable<ProxyOptions['configure']> = (proxy) => {
  proxy.on('proxyReqWs', rewriteTrustedViteDevOrigin);
};

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@shared': path.resolve(REPO_ROOT, './src/shared'),
      '@': path.resolve(__dirname, './src'),
    },
  },
  build: {
    outDir: '../dist/ui',
    emptyOutDir: true,
    sourcemap: false,
    minify: 'esbuild',
    rollupOptions: {
      output: {
        // Split only modules the account dashboard actually imports.
        manualChunks(id) {
          if (!id.includes('/node_modules/')) return;
          if (/\/node_modules\/(react|react-dom|react-router|react-router-dom)\//.test(id)) {
            return 'react-vendor';
          }
          if (id.includes('/@radix-ui/')) return 'radix-ui';
          if (id.includes('/@tanstack/')) return 'tanstack';
          if (id.includes('/lucide-react/')) return 'icons';
          if (id.includes('/sonner/')) return 'notifications';
          if (/\/node_modules\/(clsx|class-variance-authority|tailwind-merge)\//.test(id)) {
            return 'utils';
          }
        },
      },
    },
  },
  server: {
    port: 5173,
    strictPort: true,
    fs: {
      allow: [UI_ROOT, REPO_ROOT],
    },
    proxy: {
      // Translate only trusted local Vite origins for the dashboard's CSRF guard.
      // Preserve every other Origin so the backend can reject untrusted callers.
      '/api': {
        target: BACKEND_ORIGIN,
        changeOrigin: true,
        configure: configureHttpProxy,
      },
      '/ws': {
        target: 'ws://localhost:3000',
        ws: true,
        changeOrigin: true,
        configure: configureWebSocketProxy,
      },
    },
  },
});
