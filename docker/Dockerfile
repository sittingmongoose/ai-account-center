# syntax=docker/dockerfile:1.7

# =============================================================================
# Node and Bun are build-only tools for the pinned Leptos browser bundle.
# The runtime contains the Rust dashboard, SSH support, and no JS/Python runtime.
# =============================================================================
FROM node:26.13.0-bookworm-slim@sha256:add5c17c14752c54d2f3d519ec46a3815684d41c33f198bcb09fb998f7dcbf68 AS build

SHELL ["/bin/bash", "-lc"]

# Pin bun version for reproducible builds
ARG BUN_VERSION=1.4.4
ARG RUST_VERSION=1.99.1
ARG WASM_PACK_VERSION=0.15.1
# The Rust build stamps AAC_BUILD_COMMIT from this arg (or a git checkout).
# Docker contexts exclude .git, so the build entry points must pass the
# checkout HEAD; without it the image carries "unknown" and the dashboard
# refuses to start its private owner.
ARG AAC_BUILD_COMMIT
ENV AAC_BUILD_COMMIT=${AAC_BUILD_COMMIT}
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

# Fail fast when the build arg is missing: an "unknown" stamp builds but the
# dashboard refuses to start, so a broken image must never be produced.
RUN bash -c '[[ "$AAC_BUILD_COMMIT" =~ ^[0-9a-f]{7,40}$ ]]'

COPY . .

RUN bun run build:all

# Verify the native executables and actual Leptos browser artifacts before packaging.
# The wasm lives in dist/ui/pkg/<buildId>/; aac-xtask checks it against the manifest.
RUN test -x dist/native/ai-account-center \
  && test -x dist/native/ccs \
  && test -x dist/native/aac-http \
  && test -s dist/ui/app/index.html \
  && test -s dist/ui/ui-build-manifest.json \
  && cargo run --locked -p aac-xtask -- release verify-bundle

# Runtime stage: Rust dashboard and SSH support, without Node, Bun, or Python
# =============================================================================
FROM debian:bookworm-slim@sha256:e67eeead736ff7bbe258ef088bff683635c561b1fcfefad0806c1df4ab49ce0e AS runtime

SHELL ["/bin/bash", "-lc"]

# SSH supports configured computers; provider logins and desktop applications
# remain on their owners. Python helpers run on those managed hosts, not here.
RUN apt-get update \
  && apt-get install -y --no-install-recommends bash ca-certificates curl openssh-client \
  && rm -rf /var/lib/apt/lists/* \
  && groupadd --gid 1000 node \
  && useradd --uid 1000 --gid 1000 --create-home --home-dir /home/node --shell /usr/sbin/nologin node

WORKDIR /app

COPY --from=build /app/package.json ./

COPY docker/entrypoint.sh /usr/local/bin/ai-account-center-entrypoint
RUN chmod +x /usr/local/bin/ai-account-center-entrypoint

COPY --from=build /app/dist ./dist
COPY --from=build /app/scripts ./scripts
COPY --from=build /app/config/tables ./config/tables
COPY --from=build /app/config/private-files.json ./config/private-files.json
COPY --from=build /app/config/agent-artifacts.json ./config/agent-artifacts.json
COPY --from=build /app/config/rust-service-routes.json ./config/rust-service-routes.json
COPY --from=build /app/config/secret-policy.json ./config/secret-policy.json
# Package files the server resolves next to dist/ at runtime: the computer table and the
# agent-report schemas and catalog.
COPY --from=build /app/web-dashboard/public/computers.json ./web-dashboard/public/computers.json
COPY --from=build /app/docs/agent-reports/schemas ./docs/agent-reports/schemas
COPY --from=build /app/docs/agent-reports/catalog ./docs/agent-reports/catalog
COPY --from=build /app/README.md ./README.md
COPY --from=build /app/LICENSE ./LICENSE

RUN ln -s /app/dist/native/ai-account-center /usr/local/bin/ai-account-center \
  && ln -s /app/dist/native/ccs /usr/local/bin/ccs \
  && install -d -m 0700 -o node -g node /home/node/.ccs /home/node/.ssh

ENV CCS_HOME=/home/node
# A container is none of the registered computers: it reads only its own CCS state in place and
# reaches every computer, the Ubuntu VM included, over SSH through the pinned agent.
ENV AAC_DASHBOARD_COMPUTER=none
EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD ["curl", "--fail", "--silent", "--show-error", "--output", "/dev/null", "--max-time", "4", "http://127.0.0.1:3000/api/health"]

ENTRYPOINT ["/usr/local/bin/ai-account-center-entrypoint"]
USER node
CMD ["ai-account-center", "dashboard", "--host", "0.0.0.0", "--port", "3000", "--no-open"]
