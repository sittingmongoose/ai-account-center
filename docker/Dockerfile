# syntax=docker/dockerfile:1.7

# =============================================================================
# Build the checked-out AI Account Center source and pinned Leptos browser UI.
# Both build and runtime dependencies resolve from the committed bun.lock.
# =============================================================================
FROM node:26.13.0-bookworm-slim@sha256:add5c17c14752c54d2f3d519ec46a3815684d41c33f198bcb09fb998f7dcbf68 AS build

SHELL ["/bin/bash", "-lc"]

# Pin bun version for reproducible builds
ARG BUN_VERSION=1.4.4
ARG RUST_VERSION=1.99.1
ARG WASM_PACK_VERSION=0.15.1
ENV BUN_INSTALL=/usr/local/bun
ENV CARGO_HOME=/usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup
ENV PATH="$CARGO_HOME/bin:$BUN_INSTALL/bin:$PATH"

RUN apt-get update \
  && apt-get install -y --no-install-recommends curl ca-certificates unzip build-essential pkg-config libssl-dev \
  && rm -rf /var/lib/apt/lists/*

# Install specific bun version
RUN curl -fsSL https://bun.sh/install | bash -s "bun-v${BUN_VERSION}"

RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- \
      -y --profile minimal --default-toolchain "${RUST_VERSION}" --target wasm32-unknown-unknown --component rustfmt \
  && "$CARGO_HOME/bin/cargo" install wasm-pack --version "${WASM_PACK_VERSION}" --locked

WORKDIR /app

# Dependency install layer (avoid running install scripts inside the image build)
COPY package.json bun.lock bunfig.toml ./
RUN bun install --frozen-lockfile --ignore-scripts

COPY . .

RUN bun run build:all

# The image ships both native modules. The prebuilt directories are
# machine-specific and never committed, so build them for this platform here,
# then fail loudly if either module is missing: a dashboard without stored
# history is not a shippable image.
RUN bun run store:build && bun run status:build
RUN test -f crates/aac-store/prebuilt/*.node \
  && test -f crates/aac-status/prebuilt/*.node \
  && test -f dist/native/aac-store.*.node \
  && test -f dist/native/aac-status.*.node

# Verify the executable and actual Leptos browser artifacts before packaging.
# The wasm lives in dist/ui/pkg/<buildId>/; verify-bundle.js checks it against the manifest.
RUN test -x dist/native/ai-account-center \
  && test -x dist/native/ccs \
  && test -x dist/native/aac-http \
  && test -s dist/ui/app/index.html \
  && test -s dist/ui/ui-build-manifest.json \
  && node scripts/verify-bundle.js \
  && node scripts/stage-dashboard-package.js /tmp/aac-package

# Build runtime deps from the committed bun.lock to keep Docker installs pinned.
# --ignore-scripts avoids user-home side effects from postinstall during image build.
RUN --mount=type=cache,target=/root/.bun \
    bun install --frozen-lockfile --production --ignore-scripts

# =============================================================================
# Runtime stage: dashboard server and collector prerequisites, without Bun
# =============================================================================
FROM node:26.13.0-bookworm-slim@sha256:add5c17c14752c54d2f3d519ec46a3815684d41c33f198bcb09fb998f7dcbf68 AS runtime

SHELL ["/bin/bash", "-lc"]

# Python and SSH support explicitly configured account collectors. Provider
# logins, desktop applications, and host credentials are not installed here.
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates python3 openssh-client \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy pinned production dependencies resolved from the committed bun.lock.
COPY --from=build /app/package.json ./
COPY --from=build /app/node_modules ./node_modules

COPY docker/entrypoint.sh /usr/local/bin/ai-account-center-entrypoint
RUN chmod +x /usr/local/bin/ai-account-center-entrypoint

COPY --from=build /tmp/aac-package/dist ./dist
COPY --from=build /app/scripts ./scripts
COPY --from=build /app/config/tables ./config/tables
COPY --from=build /app/config/private-files.json ./config/private-files.json
COPY --from=build /app/config/agent-artifacts.json ./config/agent-artifacts.json
COPY --from=build /app/config/rust-service-routes.json ./config/rust-service-routes.json
COPY --from=build /app/config/secret-policy.json ./config/secret-policy.json
# Package files the server resolves next to dist/ at runtime (tests/unit/docker/docker-hygiene.test.ts
# derives this list from src/): the computer table and the agent-report schemas and catalog.
COPY --from=build /app/web-dashboard/public/computers.json ./web-dashboard/public/computers.json
COPY --from=build /app/docs/agent-reports/schemas ./docs/agent-reports/schemas
COPY --from=build /app/docs/agent-reports/catalog ./docs/agent-reports/catalog
COPY --from=build /app/browser-bridge ./browser-bridge
COPY --from=build /app/README.md ./README.md
COPY --from=build /app/LICENSE ./LICENSE

RUN ln -s /app/dist/native/ai-account-center /usr/local/bin/ai-account-center \
  && ln -s /app/dist/native/ccs /usr/local/bin/ccs \
  && install -d -m 0700 -o node -g node /home/node/.ccs

ENV CCS_HOME=/home/node NODE_ENV=production
EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD node -e "const http=require('http');const req=http.get('http://127.0.0.1:3000/api/health',res=>{res.resume();process.exit(res.statusCode===200?0:1);});req.on('error',()=>process.exit(1));req.setTimeout(4500,()=>{req.destroy();process.exit(1);});"

ENTRYPOINT ["/usr/local/bin/ai-account-center-entrypoint"]
USER node
CMD ["ai-account-center", "dashboard", "--host", "0.0.0.0", "--port", "3000", "--no-open"]
